//! Temporary, fixed-scope Windows lifecycle notifications.
//!
//! Process notifications are WMI start/stop hints filtered to Jarvis's closed
//! trigger-name set. The service notification is bound to the already-verified
//! `FACEITService` registration and requests status-query access only. Neither
//! source exports event PIDs or authoritative state. Every hint must be
//! followed by the caller's existing full identity-checked snapshot. A failed
//! or lost source is surfaced as an error so callers can remain restricted.

use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::core::{IUnknown, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CoUninitialize,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemContext, IWbemLocator, IWbemServices, WbemLocator,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_S_NO_ERROR, WBEM_S_TIMEDOUT,
};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_SUCCESS, WAIT_IO_COMPLETION};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, NotifyServiceStatusChangeW, OpenSCManagerW, OpenServiceW, SC_HANDLE,
    SC_MANAGER_CONNECT, SERVICE_CONTINUE_PENDING, SERVICE_NOTIFY_2W,
    SERVICE_NOTIFY_CONTINUE_PENDING, SERVICE_NOTIFY_PAUSED, SERVICE_NOTIFY_PAUSE_PENDING,
    SERVICE_NOTIFY_RUNNING, SERVICE_NOTIFY_START_PENDING, SERVICE_NOTIFY_STATUS_CHANGE_2,
    SERVICE_NOTIFY_STOPPED, SERVICE_NOTIFY_STOP_PENDING, SERVICE_PAUSED, SERVICE_PAUSE_PENDING,
    SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STOPPED, SERVICE_STOP_PENDING,
};
use windows_sys::Win32::System::Threading::SleepEx;

use super::{
    canonical_regular_file, query_service_binary_path, same_windows_path, wide_null_os,
    FixedCompetitiveProcess, VerifiedPreMatchSetup,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const PROCESS_EVENT_WAIT_SLICE_MS: i32 = 25;
const SERVICE_APC_WAIT_SLICE_MS: u32 = 25;

/// One coalesced lifecycle wake, or a backend failure which must keep policy
/// fail-closed. `Ok(false)` from [`CompetitiveLifecycleNotifications::wait_for_change`]
/// means only that the wait timed out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LifecycleNotificationFailure {
    source: &'static str,
    code: i64,
}

impl LifecycleNotificationFailure {
    const fn new(source: &'static str, code: i64) -> Self {
        Self { source, code }
    }

    /// Stable subsystem/operation label; it contains no process details.
    #[must_use]
    pub const fn source(self) -> &'static str {
        self.source
    }

    /// HRESULT or Win32 error code, stored as a signed integer for both APIs.
    #[must_use]
    pub const fn code(self) -> i64 {
        self.code
    }
}

impl fmt::Display for LifecycleNotificationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "competitive lifecycle notification {} failed (code {})",
            self.source, self.code
        )
    }
}

impl Error for LifecycleNotificationFailure {}

#[derive(Default)]
struct PendingSignal {
    hint_pending: bool,
    failure: Option<LifecycleNotificationFailure>,
}

/// Capacity-one signaling: any number of lifecycle edges collapse to one
/// pending wake. A backend failure supersedes a wake and remains sticky.
#[derive(Default)]
struct CoalescedSignal {
    pending: Mutex<PendingSignal>,
    changed: Condvar,
}

impl CoalescedSignal {
    fn notify_hint(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pending.failure.is_none() {
            pending.hint_pending = true;
            self.changed.notify_one();
        }
    }

    fn fail(&self, failure: LifecycleNotificationFailure) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.hint_pending = false;
        pending.failure.get_or_insert(failure);
        self.changed.notify_one();
    }

    fn wait_for_change(&self, timeout: Duration) -> Result<bool, LifecycleNotificationFailure> {
        let deadline = Instant::now() + timeout;
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(failure) = pending.failure {
                return Err(failure);
            }
            if pending.hint_pending {
                pending.hint_pending = false;
                return Ok(true);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let (next, result) = self
                .changed
                .wait_timeout(pending, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            pending = next;
            if result.timed_out() && !pending.hint_pending && pending.failure.is_none() {
                return Ok(false);
            }
        }
    }
}

/// Owns temporary, local notifications for Jarvis's fixed competitive
/// triggers. It has no API to start/stop a process or service and does not
/// return event payloads or PIDs. Each `true` result is only a wake-up hint;
/// callers must re-query the complete fixed snapshot before changing policy.
///
/// Construction is all-or-nothing: both WMI process streams and the verified
/// FACEIT service notification must be active before `start` succeeds. Any
/// setup/runtime failure is an error and must be treated as unhealthy. WMI
/// namespace permissions are machine/account dependent; no elevation or
/// permanent event subscription is installed here.
pub struct CompetitiveLifecycleNotifications {
    signal: Arc<CoalescedSignal>,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl CompetitiveLifecycleNotifications {
    /// Establishes fixed process start/stop WMI subscriptions and a read-only
    /// status subscription to the already-verified `FACEITService`.
    pub fn start(setup: &VerifiedPreMatchSetup) -> Result<Self, LifecycleNotificationFailure> {
        let signal = Arc::new(CoalescedSignal::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);

        let worker_signal = Arc::clone(&signal);
        let worker_cancelled = Arc::clone(&cancelled);
        let service_path = setup.faceit_service.clone();
        let worker = thread::Builder::new()
            .name("jarvis-trigger-notifications".to_owned())
            .spawn(move || {
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_notification_worker(
                        &service_path,
                        &worker_signal,
                        worker_cancelled.as_ref(),
                        &ready_sender,
                    )
                }));
                match run {
                    Ok(Ok(())) => {}
                    Ok(Err(failure)) => worker_signal.fail(failure),
                    Err(_) => {
                        let failure = LifecycleNotificationFailure::new("watcher worker panic", 0);
                        worker_signal.fail(failure);
                        let _ = ready_sender.send(Err(failure));
                    }
                }
            })
            .map_err(|_| LifecycleNotificationFailure::new("watcher thread creation", 0))?;

        match ready_receiver.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                signal,
                cancelled,
                worker: Some(worker),
            }),
            Ok(Err(failure)) => {
                let _ = worker.join();
                Err(failure)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancelled.store(true, Ordering::Release);
                // Do not detach a watcher. If an in-flight local COM call
                // exceeds the readiness deadline, join still waits for it to
                // unwind before this failed start returns.
                let _ = worker.join();
                Err(LifecycleNotificationFailure::new(
                    "watcher startup timeout",
                    0,
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                Err(LifecycleNotificationFailure::new(
                    "watcher stopped during startup",
                    0,
                ))
            }
        }
    }

    /// Waits for one coalesced hint. `Ok(true)` never means a trigger is active;
    /// perform a fresh authoritative snapshot. A sticky error means event
    /// coverage was lost and must not be interpreted as a clear state.
    pub fn wait_for_change(&self, timeout: Duration) -> Result<bool, LifecycleNotificationFailure> {
        self.signal.wait_for_change(timeout)
    }
}

impl Drop for CompetitiveLifecycleNotifications {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy)]
enum ProcessEventKind {
    Start,
    Stop,
}

fn process_event_query(kind: ProcessEventKind) -> String {
    let class = match kind {
        ProcessEventKind::Start => "Win32_ProcessStartTrace",
        ProcessEventKind::Stop => "Win32_ProcessStopTrace",
    };
    let mut query = format!("SELECT * FROM {class} WHERE ");
    for (index, process) in fixed_processes().iter().enumerate() {
        if index != 0 {
            query.push_str(" OR ");
        }
        query.push_str("ProcessName = '");
        query.push_str(process.executable_name());
        query.push('\'');
    }
    query
}

fn fixed_processes() -> [FixedCompetitiveProcess; 3] {
    [
        FixedCompetitiveProcess::Cs2,
        FixedCompetitiveProcess::FaceitAcLauncher,
        FixedCompetitiveProcess::FaceitService,
    ]
}

#[cfg(test)]
fn is_fixed_process_name(candidate: &str) -> bool {
    fixed_processes()
        .iter()
        .any(|process| candidate.eq_ignore_ascii_case(process.executable_name()))
}

fn run_notification_worker(
    verified_service_path: &Path,
    signal: &Arc<CoalescedSignal>,
    cancelled: &AtomicBool,
    ready_sender: &mpsc::SyncSender<Result<(), LifecycleNotificationFailure>>,
) -> Result<(), LifecycleNotificationFailure> {
    let setup = (|| {
        let apartment = ComApartment::initialize()?;
        let (start_events, stop_events) = open_process_event_subscriptions()?;
        if cancelled.load(Ordering::Acquire) {
            return Err(LifecycleNotificationFailure::new(
                "watcher startup cancelled",
                0,
            ));
        }

        let service = VerifiedServiceNotification::open(verified_service_path, signal)?;
        Ok((apartment, start_events, stop_events, service))
    })();

    let (apartment, start_events, stop_events, mut service) = match setup {
        Ok(setup) => setup,
        Err(failure) => {
            let _ = ready_sender.send(Err(failure));
            return Err(failure);
        }
    };

    if cancelled.load(Ordering::Acquire) {
        let _ = ready_sender.send(Err(LifecycleNotificationFailure::new(
            "watcher startup cancelled",
            0,
        )));
        return Ok(());
    }
    let _ = ready_sender.send(Ok(()));

    while !cancelled.load(Ordering::Acquire) {
        let start_seen = poll_event(&start_events, ProcessEventKind::Start)?;
        let stop_seen = poll_event(&stop_events, ProcessEventKind::Stop)?;
        if start_seen || stop_seen {
            signal.notify_hint();
        }

        // SCM callbacks are APCs queued to this exact thread. SleepEx's
        // alertable wait dispatches them without doing work in the callback.
        // The fixed WMI pulls above have bounded timeouts, so stop/drop remains
        // responsive without a process snapshot poll.
        // SAFETY: This waits on the current thread, which registered the
        // service APC callback; alertable mode dispatches only that callback.
        let sleep_result = unsafe { SleepEx(SERVICE_APC_WAIT_SLICE_MS, 1) };
        if sleep_result == WAIT_IO_COMPLETION && service.callback_received() {
            signal.notify_hint();
            service.rearm()?;
        }
    }

    service.close();
    drop(stop_events);
    drop(start_events);
    drop(apartment);
    Ok(())
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, LifecycleNotificationFailure> {
        // SAFETY: This dedicated watcher thread has no prior COM apartment and
        // requests the multithreaded apartment used by the local WMI provider.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if initialized.is_err() {
            return Err(LifecycleNotificationFailure::new(
                "WMI COM apartment initialization",
                i64::from(initialized.0),
            ));
        }

        // SAFETY: The watcher initializes process COM security once before
        // creating any WMI proxies; all optional credential/descriptor inputs
        // are null, so WMI uses this process's current user token.
        let security = unsafe {
            CoInitializeSecurity(
                None,
                -1,
                None,
                None,
                windows::Win32::System::Com::RPC_C_AUTHN_LEVEL_DEFAULT,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
                None,
            )
        };
        if let Err(error) = security {
            // SAFETY: CoInitializeEx succeeded above on this same thread.
            unsafe { CoUninitialize() };
            return Err(LifecycleNotificationFailure::new(
                "WMI COM security initialization",
                i64::from(error.code().0),
            ));
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: This guard is created only after successful CoInitializeEx
        // and is dropped on the same dedicated watcher thread.
        unsafe { CoUninitialize() };
    }
}

fn open_process_event_subscriptions(
) -> Result<(IEnumWbemClassObject, IEnumWbemClassObject), LifecycleNotificationFailure> {
    // SAFETY: WbemLocator is the documented in-process WMI locator CLSID and
    // CoCreateInstance returns an owned IWbemLocator interface on this thread.
    let locator: IWbemLocator = unsafe {
        CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(|error| {
            LifecycleNotificationFailure::new("WMI locator activation", i64::from(error.code().0))
        })?
    };

    let namespace = BSTR::from("ROOT\\CIMV2");
    let empty = BSTR::new();
    // SAFETY: All BSTR arguments remain alive for this synchronous local
    // ROOT\\CIMV2 connection; credentials are intentionally empty.
    let services: IWbemServices = unsafe {
        locator
            .ConnectServer(
                &namespace,
                &empty,
                &empty,
                &empty,
                0,
                &empty,
                None::<&IWbemContext>,
            )
            .map_err(|error| {
                LifecycleNotificationFailure::new(
                    "WMI local namespace connection",
                    i64::from(error.code().0),
                )
            })?
    };
    set_wmi_proxy_security(&services, "WMI services proxy")?;

    let language = BSTR::from("WQL");
    let start_query = BSTR::from(process_event_query(ProcessEventKind::Start));
    // SAFETY: The fixed WQL is generated only from the closed trigger-name
    // enum; the returned forward-only enumerator is held on this COM thread.
    let start_events = unsafe {
        services
            .ExecNotificationQuery(
                &language,
                &start_query,
                WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
                None::<&IWbemContext>,
            )
            .map_err(|error| {
                LifecycleNotificationFailure::new(
                    "WMI fixed process-start subscription",
                    i64::from(error.code().0),
                )
            })?
    };
    set_wmi_proxy_security(&start_events, "WMI process-start event stream")?;

    let stop_query = BSTR::from(process_event_query(ProcessEventKind::Stop));
    // SAFETY: The fixed WQL is generated only from the closed trigger-name
    // enum; the returned forward-only enumerator is held on this COM thread.
    let stop_events = unsafe {
        services
            .ExecNotificationQuery(
                &language,
                &stop_query,
                WBEM_FLAG_RETURN_IMMEDIATELY | WBEM_FLAG_FORWARD_ONLY,
                None::<&IWbemContext>,
            )
            .map_err(|error| {
                LifecycleNotificationFailure::new(
                    "WMI fixed process-stop subscription",
                    i64::from(error.code().0),
                )
            })?
    };
    set_wmi_proxy_security(&stop_events, "WMI process-stop event stream")?;

    Ok((start_events, stop_events))
}

fn set_wmi_proxy_security<T: windows::core::Interface>(
    proxy: &T,
    source: &'static str,
) -> Result<(), LifecycleNotificationFailure> {
    // SAFETY: `proxy` is a live WMI COM interface. QueryInterface obtains a
    // referenced IUnknown and the remaining proxy blanket inputs are fixed.
    unsafe {
        let unknown: IUnknown = proxy.cast().map_err(|error| {
            LifecycleNotificationFailure::new(source, i64::from(error.code().0))
        })?;
        CoSetProxyBlanket(
            &unknown,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_PKT_PRIVACY,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
    }
    .map_err(|error| LifecycleNotificationFailure::new(source, i64::from(error.code().0)))
}

fn poll_event(
    events: &IEnumWbemClassObject,
    kind: ProcessEventKind,
) -> Result<bool, LifecycleNotificationFailure> {
    let mut objects = [None];
    let mut returned = 0_u32;
    // SAFETY: `events` is the owned enumerator from the active subscription;
    // the one-element object array and count are valid writable outputs.
    let result = unsafe { events.Next(PROCESS_EVENT_WAIT_SLICE_MS, &mut objects, &mut returned) };
    classify_event_next_result(result.0, returned, kind)
}

fn classify_event_next_result(
    status: i32,
    returned: u32,
    kind: ProcessEventKind,
) -> Result<bool, LifecycleNotificationFailure> {
    if status == WBEM_S_TIMEDOUT.0 {
        return Ok(false);
    }
    let source = match kind {
        ProcessEventKind::Start => "WMI process-start event stream",
        ProcessEventKind::Stop => "WMI process-stop event stream",
    };
    if status == WBEM_S_NO_ERROR.0 && returned == 1 {
        return Ok(true);
    }
    // WBEM_S_FALSE means the enumerator ended (possibly after returning an
    // object), not an ordinary empty wait. Event subscriptions are expected to
    // remain open-ended, so any non-timeout status other than one object is a
    // coverage failure and must not silently degrade into polling.
    Err(LifecycleNotificationFailure::new(source, i64::from(status)))
}

struct ServiceCallbackContext {
    signal: Arc<CoalescedSignal>,
    received: AtomicBool,
}

unsafe extern "system" fn service_notification_callback(parameter: *const c_void) {
    if parameter.is_null() {
        return;
    }
    // SAFETY: SCM passes the address of the stable SERVICE_NOTIFY_2W buffer;
    // its pContext points at the watcher thread's boxed callback context. The
    // service handle is closed and queued APCs drained before either is freed.
    // SAFETY: SCM invokes this callback with the live SERVICE_NOTIFY buffer
    // passed to NotifyServiceStatusChangeW on the same alertable thread.
    let notification = unsafe { &*(parameter.cast::<SERVICE_NOTIFY_2W>()) };
    if notification.pContext.is_null() {
        return;
    }
    // SAFETY: pContext was set to the stable boxed context owned until after
    // closing the service handle and draining queued callback APCs.
    let context = unsafe { &*(notification.pContext.cast::<ServiceCallbackContext>()) };
    context.received.store(true, Ordering::Release);
    context.signal.notify_hint();
}

struct VerifiedServiceNotification {
    manager: SC_HANDLE,
    service: SC_HANDLE,
    notification: Box<SERVICE_NOTIFY_2W>,
    callback_context: Box<ServiceCallbackContext>,
    closed: bool,
}

impl VerifiedServiceNotification {
    fn open(
        expected_path: &Path,
        signal: &Arc<CoalescedSignal>,
    ) -> Result<Self, LifecycleNotificationFailure> {
        // SAFETY: Null names select the local SCM and request connect-only
        // access; the returned handle is owned and closed below.
        let manager =
            unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
        if manager.is_null() {
            // SAFETY: Read the thread-local error immediately after failure.
            let error = unsafe { GetLastError() };
            return Err(service_error("open local service manager", error));
        }

        let service_name = wide_null_os(std::ffi::OsStr::new("FACEITService"));
        // SAFETY: `manager` is valid, the service name is a live nul-terminated
        // fixed string, and requested rights are query-only.
        let service = unsafe {
            OpenServiceW(
                manager,
                service_name.as_ptr(),
                SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS,
            )
        };
        if service.is_null() {
            // SAFETY: Read the thread-local error immediately after failure.
            let error = unsafe { GetLastError() };
            // SAFETY: `manager` is the owned handle returned above and is no
            // longer needed after this failure path.
            unsafe {
                let _ = CloseServiceHandle(manager);
            }
            return Err(service_error(
                "open verified FACEITService for query",
                error,
            ));
        }

        let verified_binary = query_service_binary_path(service)
            .map_err(|_| LifecycleNotificationFailure::new("query FACEITService identity", 0))
            .and_then(|path| {
                canonical_regular_file(&path, "FACEITService binary").map_err(|_| {
                    LifecycleNotificationFailure::new("canonicalize FACEITService identity", 0)
                })
            });
        match verified_binary {
            Ok(path) if same_windows_path(expected_path, &path) => {}
            Ok(_) => {
                close_service_handles(service, manager);
                return Err(LifecycleNotificationFailure::new(
                    "verified FACEITService identity changed",
                    0,
                ));
            }
            Err(failure) => {
                close_service_handles(service, manager);
                return Err(failure);
            }
        }

        let callback_context = Box::new(ServiceCallbackContext {
            signal: Arc::clone(signal),
            received: AtomicBool::new(false),
        });
        let mut notification = Box::new(SERVICE_NOTIFY_2W {
            dwVersion: SERVICE_NOTIFY_STATUS_CHANGE_2,
            pfnNotifyCallback: Some(service_notification_callback),
            pContext: (&*callback_context as *const ServiceCallbackContext)
                .cast_mut()
                .cast(),
            ..Default::default()
        });

        let result = register_service_notification(service, &mut notification, None);
        if result != ERROR_SUCCESS {
            close_service_handles(service, manager);
            return Err(service_error("subscribe to FACEITService status", result));
        }

        Ok(Self {
            manager,
            service,
            notification,
            callback_context,
            closed: false,
        })
    }

    fn callback_received(&self) -> bool {
        if !self.callback_context.received.swap(false, Ordering::AcqRel) {
            return false;
        }
        true
    }

    fn rearm(&mut self) -> Result<(), LifecycleNotificationFailure> {
        if self.notification.dwNotificationStatus != ERROR_SUCCESS {
            return Err(service_error(
                "FACEITService status notification lost",
                self.notification.dwNotificationStatus,
            ));
        }
        let current_state = self.notification.ServiceStatus.dwCurrentState;
        self.notification.dwNotificationStatus = ERROR_SUCCESS;
        self.notification.dwNotificationTriggered = 0;
        self.notification.ServiceStatus = Default::default();
        let result = register_service_notification(
            self.service,
            &mut self.notification,
            Some(current_state),
        );
        if result != ERROR_SUCCESS {
            return Err(service_error(
                "rearm FACEITService status notification",
                result,
            ));
        }
        Ok(())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        // Closing the service handle cancels the pending one-shot notification.
        // Drain any already queued callback on this alertable thread before
        // dropping the stable callback context/buffer.
        // SAFETY: Both fields are the owned SCM handles opened by `open`.
        let service_closed = unsafe { CloseServiceHandle(self.service) } != 0;
        // SAFETY: Runs on the registered thread and drains already-queued APCs
        // after service-handle closure canceled any outstanding request.
        let _ = unsafe { SleepEx(0, 1) };
        // SAFETY: This is the owned SCM manager handle opened by `open`.
        let manager_closed = unsafe { CloseServiceHandle(self.manager) } != 0;
        if !(service_closed && manager_closed) {
            // SAFETY: If a handle failed to close, the SCM may still reference
            // the notification storage. Leak only these two tiny buffers rather
            // than risk a callback through a freed pointer.
            let notification = std::mem::replace(
                &mut self.notification,
                // SAFETY: SERVICE_NOTIFY_2W is a C record of scalar values and
                // pointers; the zero placeholder is never registered or read.
                Box::new(unsafe { std::mem::zeroed() }),
            );
            let retained_signal = Arc::clone(&self.callback_context.signal);
            let context = std::mem::replace(
                &mut self.callback_context,
                Box::new(ServiceCallbackContext {
                    signal: retained_signal,
                    received: AtomicBool::new(false),
                }),
            );
            let _ = Box::leak(notification);
            let _ = Box::leak(context);
        }
    }
}

impl Drop for VerifiedServiceNotification {
    fn drop(&mut self) {
        self.close();
    }
}

fn register_service_notification(
    service: SC_HANDLE,
    notification: &mut SERVICE_NOTIFY_2W,
    exclude_current_state: Option<u32>,
) -> u32 {
    let mask = notification_mask(exclude_current_state);
    // SAFETY: The service handle has query-status access, the notification
    // buffer stays boxed/alive until cancellation, and callback dispatch is
    // serviced through this thread's alertable SleepEx loop.
    unsafe { NotifyServiceStatusChangeW(service, mask, notification) }
}

fn notification_mask(exclude_current_state: Option<u32>) -> u32 {
    let mut mask = SERVICE_NOTIFY_STOPPED
        | SERVICE_NOTIFY_START_PENDING
        | SERVICE_NOTIFY_STOP_PENDING
        | SERVICE_NOTIFY_RUNNING
        | SERVICE_NOTIFY_CONTINUE_PENDING
        | SERVICE_NOTIFY_PAUSE_PENDING
        | SERVICE_NOTIFY_PAUSED;
    if let Some(state) = exclude_current_state {
        let matching_status = match state {
            SERVICE_STOPPED => Some(SERVICE_NOTIFY_STOPPED),
            SERVICE_START_PENDING => Some(SERVICE_NOTIFY_START_PENDING),
            SERVICE_STOP_PENDING => Some(SERVICE_NOTIFY_STOP_PENDING),
            SERVICE_RUNNING => Some(SERVICE_NOTIFY_RUNNING),
            SERVICE_CONTINUE_PENDING => Some(SERVICE_NOTIFY_CONTINUE_PENDING),
            SERVICE_PAUSE_PENDING => Some(SERVICE_NOTIFY_PAUSE_PENDING),
            SERVICE_PAUSED => Some(SERVICE_NOTIFY_PAUSED),
            _ => None,
        };
        if let Some(matching_status) = matching_status {
            mask &= !matching_status;
        }
    }
    mask
}

fn service_error(source: &'static str, code: u32) -> LifecycleNotificationFailure {
    LifecycleNotificationFailure::new(source, i64::from(code))
}

fn close_service_handles(service: SC_HANDLE, manager: SC_HANDLE) {
    // SAFETY: These are only called with owned successful OpenService/SCM
    // handles before a notification is registered.
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_wmi_queries_are_limited_to_fixed_trigger_names() {
        let start = process_event_query(ProcessEventKind::Start);
        let stop = process_event_query(ProcessEventKind::Stop);
        for query in [&start, &stop] {
            assert!(query.contains("ProcessName = 'cs2.exe'"));
            assert!(query.contains("ProcessName = 'faceitclient.exe'"));
            assert!(query.contains("ProcessName = 'faceitservice.exe'"));
            assert!(!query.contains("chrome.exe"));
            assert!(!query.contains("ProcessId"));
        }
        assert!(start.starts_with("SELECT * FROM Win32_ProcessStartTrace WHERE "));
        assert!(stop.starts_with("SELECT * FROM Win32_ProcessStopTrace WHERE "));
    }

    #[test]
    fn process_name_filter_is_case_insensitive_and_closed() {
        assert!(is_fixed_process_name("CS2.EXE"));
        assert!(is_fixed_process_name("FaceItClient.exe"));
        assert!(!is_fixed_process_name("faceservice.exe"));
        assert!(!is_fixed_process_name("chrome.exe"));
        assert!(!is_fixed_process_name("cs2.exe.extra"));
    }

    #[test]
    fn only_wmi_timeout_or_one_object_are_accepted() {
        assert_eq!(
            classify_event_next_result(WBEM_S_TIMEDOUT.0, 0, ProcessEventKind::Start,),
            Ok(false)
        );
        assert_eq!(
            classify_event_next_result(WBEM_S_NO_ERROR.0, 1, ProcessEventKind::Start,),
            Ok(true)
        );
        assert!(classify_event_next_result(
            windows::Win32::System::Wmi::WBEM_S_FALSE.0,
            0,
            ProcessEventKind::Stop,
        )
        .is_err());
        assert!(classify_event_next_result(0, 0, ProcessEventKind::Stop).is_err());
    }

    #[test]
    fn lifecycle_wakes_coalesce_and_backend_failure_wins() {
        let signal = CoalescedSignal::default();
        signal.notify_hint();
        signal.notify_hint();
        signal.notify_hint();
        assert_eq!(signal.wait_for_change(Duration::ZERO), Ok(true));
        assert_eq!(signal.wait_for_change(Duration::ZERO), Ok(false));

        signal.notify_hint();
        let failure = LifecycleNotificationFailure::new("fixture watcher failure", 7);
        signal.fail(failure);
        assert_eq!(signal.wait_for_change(Duration::ZERO), Err(failure));
        assert_eq!(signal.wait_for_change(Duration::ZERO), Err(failure));
    }

    #[test]
    fn service_status_rearm_excludes_only_the_reported_state() {
        assert_eq!(
            notification_mask(Some(SERVICE_RUNNING)),
            SERVICE_NOTIFY_STOPPED
                | SERVICE_NOTIFY_START_PENDING
                | SERVICE_NOTIFY_STOP_PENDING
                | SERVICE_NOTIFY_CONTINUE_PENDING
                | SERVICE_NOTIFY_PAUSE_PENDING
                | SERVICE_NOTIFY_PAUSED
        );
        assert_eq!(
            notification_mask(Some(SERVICE_STOPPED)),
            SERVICE_NOTIFY_START_PENDING
                | SERVICE_NOTIFY_STOP_PENDING
                | SERVICE_NOTIFY_RUNNING
                | SERVICE_NOTIFY_CONTINUE_PENDING
                | SERVICE_NOTIFY_PAUSE_PENDING
                | SERVICE_NOTIFY_PAUSED
        );
    }
}
