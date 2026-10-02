//! Narrow, reviewed Win32 FFI boundary for Jarvis-owned worker jobs.
//!
//! This crate intentionally exposes no process-opening or arbitrary process
//! assignment API. It can create, query, and terminate an unnamed Job Object
//! that only future trusted worker-launch code may populate.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::fs;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessW, GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess,
    GetProcessTimes, OpenEventW, ResumeThread, SetEvent, TerminateProcess, WaitForSingleObject,
    CREATE_SUSPENDED, INFINITE, PROCESS_INFORMATION, STARTUPINFOW, SYNCHRONIZATION_SYNCHRONIZE,
};

/// A Win32 call failed. Error codes are retained for local diagnostics without
/// logging process arguments or other sensitive runtime data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Win32Error {
    operation: &'static str,
    code: u32,
}

impl Win32Error {
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        self.operation
    }

    #[must_use]
    pub const fn code(&self) -> u32 {
        self.code
    }
}

impl fmt::Display for Win32Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed with Win32 error {}",
            self.operation, self.code
        )
    }
}

impl Error for Win32Error {}

/// The complete hard-coded set of binaries the reviewed FFI layer may create.
/// This is deliberately an enum rather than an executable path or command-line
/// parameter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedInternalWorker {
    Session,
}

impl FixedInternalWorker {
    #[must_use]
    pub const fn executable_name(self) -> &'static str {
        match self {
            Self::Session => "jarvis-session-worker.exe",
        }
    }
}

#[derive(Debug)]
pub enum WorkerLaunchError {
    Resolve {
        operation: &'static str,
        source: std::io::Error,
    },
    NotRegularFile,
    OutsideInstallationDirectory,
    ShutdownSignalAlreadyExists,
    Win32(Win32Error),
    NotAssignedToExpectedJob,
}

impl fmt::Display for WorkerLaunchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Resolve { operation, source } => {
                write!(
                    formatter,
                    "could not {operation} the fixed internal worker: {source}"
                )
            }
            Self::NotRegularFile => {
                write!(formatter, "the fixed internal worker is not a regular file")
            }
            Self::OutsideInstallationDirectory => {
                write!(
                    formatter,
                    "the fixed internal worker must resolve inside its installation directory"
                )
            }
            Self::ShutdownSignalAlreadyExists => {
                write!(
                    formatter,
                    "the fixed worker shutdown signal name already exists"
                )
            }
            Self::Win32(error) => error.fmt(formatter),
            Self::NotAssignedToExpectedJob => {
                write!(
                    formatter,
                    "the suspended worker was not verified in its expected Job Object"
                )
            }
        }
    }
}

impl Error for WorkerLaunchError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Resolve { source, .. } => Some(source),
            Self::Win32(error) => Some(error),
            Self::NotRegularFile
            | Self::OutsideInstallationDirectory
            | Self::ShutdownSignalAlreadyExists
            | Self::NotAssignedToExpectedJob => None,
        }
    }
}

impl From<Win32Error> for WorkerLaunchError {
    fn from(error: Win32Error) -> Self {
        Self::Win32(error)
    }
}

/// Immutable identity collected from the process handle while the process is
/// still suspended. The creation time prevents later PID reuse from being
/// mistaken for this worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InternalWorkerIdentity {
    process_id: u32,
    creation_time_filetime: u64,
}

impl InternalWorkerIdentity {
    #[must_use]
    pub const fn process_id(self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn creation_time_filetime(self) -> u64 {
        self.creation_time_filetime
    }
}

/// An unnamed, non-inheritable Job Object configured to terminate all of its
/// members when the final handle closes.
///
/// The raw handle is never exposed, preventing this safe API from accidentally
/// attaching or terminating a user application.
pub struct KillOnCloseJob {
    handle: HANDLE,
}

impl fmt::Debug for KillOnCloseJob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KillOnCloseJob")
            .finish_non_exhaustive()
    }
}

impl KillOnCloseJob {
    /// Creates a fresh unnamed job. It has no members until trusted worker
    /// startup assigns a suspended child process in a later milestone.
    pub fn create() -> Result<Self, Win32Error> {
        // SAFETY: Both optional parameters are documented nullable pointers.
        // A null security descriptor requests the current-user default ACL and
        // a null name creates an unnamed, non-shared Job Object.
        let handle = unsafe { CreateJobObjectW(null(), null()) };
        if handle.is_null() {
            return Err(last_error("CreateJobObjectW"));
        }

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `handle` was returned by CreateJobObjectW above and remains
        // owned by this function. `limits` is initialized, has the documented
        // layout, and its pointer/byte length are valid for this synchronous
        // call.
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            let error = last_error("SetInformationJobObject");
            // SAFETY: `handle` is a valid owned Job Object handle and has not
            // been transferred. Closing it releases the failed setup object.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(error);
        }
        Ok(Self { handle })
    }

    /// Creates the selected fixed Jarvis worker suspended. No caller can pass
    /// an executable path, command line, inherited handle list, or elevation
    /// request through this API.
    pub fn create_suspended_fixed_worker(
        &self,
        worker: FixedInternalWorker,
        installation_directory: &Path,
    ) -> Result<SuspendedInternalWorker, WorkerLaunchError> {
        let installation_directory =
            fs::canonicalize(installation_directory).map_err(|source| {
                WorkerLaunchError::Resolve {
                    operation: "resolve the installation directory",
                    source,
                }
            })?;
        let worker_path = installation_directory.join(worker.executable_name());
        let worker_path =
            fs::canonicalize(worker_path).map_err(|source| WorkerLaunchError::Resolve {
                operation: "resolve the fixed internal worker",
                source,
            })?;
        let metadata = fs::metadata(&worker_path).map_err(|source| WorkerLaunchError::Resolve {
            operation: "inspect the fixed internal worker",
            source,
        })?;
        if !metadata.is_file() {
            return Err(WorkerLaunchError::NotRegularFile);
        }
        if worker_path.parent() != Some(installation_directory.as_path()) {
            return Err(WorkerLaunchError::OutsideInstallationDirectory);
        }

        let application_name = wide_null(&worker_path);
        let current_directory = wide_null(&installation_directory);
        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut process_information = PROCESS_INFORMATION::default();
        // SAFETY: Both UTF-16 vectors are null-terminated and remain live for
        // this synchronous call. The application path is an exact canonical
        // fixed-manifest path, not a caller command line. Process/thread
        // security attributes and inherited handles are null/false, the
        // environment is inherited, and the initialized output structures have
        // the documented layouts. CREATE_SUSPENDED prevents the primary thread
        // from running before Job Object assignment succeeds.
        let created = unsafe {
            CreateProcessW(
                application_name.as_ptr(),
                null_mut(),
                null(),
                null(),
                0,
                CREATE_SUSPENDED,
                null(),
                current_directory.as_ptr(),
                &startup,
                &mut process_information,
            )
        };
        if created == 0 {
            return Err(WorkerLaunchError::Win32(last_error("CreateProcessW")));
        }

        let identity = match process_identity(
            process_information.hProcess,
            process_information.dwProcessId,
        ) {
            Ok(identity) => identity,
            Err(error) => {
                // SAFETY: CreateProcessW returned valid owned handles. The
                // child is still suspended, so terminating and closing it here
                // prevents an unassigned process from surviving this failure.
                unsafe {
                    let _ = TerminateProcess(process_information.hProcess, 1);
                    let _ = CloseHandle(process_information.hThread);
                    let _ = CloseHandle(process_information.hProcess);
                }
                return Err(WorkerLaunchError::Win32(error));
            }
        };

        let shutdown_signal = match CooperativeShutdownSignal::create(worker, identity) {
            Ok(signal) => signal,
            Err(error) => {
                // SAFETY: CreateProcessW returned valid owned handles. The
                // child is still suspended, so terminating and closing it here
                // prevents an unassigned process from surviving setup failure.
                unsafe {
                    let _ = TerminateProcess(process_information.hProcess, 1);
                    let _ = CloseHandle(process_information.hThread);
                    let _ = CloseHandle(process_information.hProcess);
                }
                return Err(error);
            }
        };

        Ok(SuspendedInternalWorker {
            process: process_information.hProcess,
            primary_thread: process_information.hThread,
            identity,
            shutdown_signal: Some(shutdown_signal),
        })
    }

    /// Assigns a still-suspended worker to this Job Object and verifies that
    /// membership before the worker's primary thread is resumed.
    pub fn assign_and_verify(
        &self,
        worker: &SuspendedInternalWorker,
    ) -> Result<(), WorkerLaunchError> {
        // SAFETY: Both handles are valid and owned for the duration of this
        // call. `worker` was created suspended by this module and has not been
        // resumed; this Job Object is the opaque object owned by `self`.
        let assigned = unsafe { AssignProcessToJobObject(self.handle, worker.process) };
        if assigned == 0 {
            return Err(WorkerLaunchError::Win32(last_error(
                "AssignProcessToJobObject",
            )));
        }

        let mut is_member = 0;
        // SAFETY: Both handles remain valid, and `is_member` is a writable
        // BOOL-sized output location for the synchronous API call.
        let verified = unsafe { IsProcessInJob(worker.process, self.handle, &mut is_member) };
        if verified == 0 {
            return Err(WorkerLaunchError::Win32(last_error("IsProcessInJob")));
        }
        if is_member == 0 {
            return Err(WorkerLaunchError::NotAssignedToExpectedJob);
        }
        Ok(())
    }

    /// Returns the kernel-reported number of active processes in this Job.
    pub fn active_process_count(&self) -> Result<u32, Win32Error> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `self.handle` is live for `&self`; `accounting` has the
        // correct layout and stays valid for the synchronous query; the output
        // byte count is intentionally unused and may be null by API contract.
        let queried = unsafe {
            QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast::<c_void>(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        };
        if queried == 0 {
            return Err(last_error("QueryInformationJobObject"));
        }
        Ok(accounting.ActiveProcesses)
    }

    /// Terminates only processes previously assigned to this specific Job.
    /// This does not enumerate, open, or affect other desktop applications.
    pub fn terminate_all(&self, exit_code: u32) -> Result<(), Win32Error> {
        // SAFETY: `self.handle` is a valid Job Object owned by this wrapper;
        // TerminateJobObject is synchronous with respect to accepting the
        // request, while higher-level code independently verifies emptiness.
        let terminated = unsafe { TerminateJobObject(self.handle, exit_code) };
        if terminated == 0 {
            return Err(last_error("TerminateJobObject"));
        }
        Ok(())
    }
}

/// A fixed internal worker whose primary thread is still suspended. Dropping
/// this value always terminates the child, so a failed assignment or resume
/// cannot leave an unmanaged worker alive.
pub struct SuspendedInternalWorker {
    process: HANDLE,
    primary_thread: HANDLE,
    identity: InternalWorkerIdentity,
    shutdown_signal: Option<CooperativeShutdownSignal>,
}

impl SuspendedInternalWorker {
    #[must_use]
    pub const fn identity(&self) -> InternalWorkerIdentity {
        self.identity
    }

    /// Resumes the primary thread only after `assign_and_verify` has completed
    /// successfully. The caller cannot construct this type directly.
    pub fn resume(mut self) -> Result<RunningInternalWorker, WorkerLaunchError> {
        // SAFETY: `primary_thread` is the valid initial thread handle returned
        // by CreateProcessW. It is still owned by `self` and was created with
        // CREATE_SUSPENDED, so this call is the first release of that suspend
        // state after the higher-level Job Object verification.
        let previous_suspend_count = unsafe { ResumeThread(self.primary_thread) };
        if previous_suspend_count == u32::MAX {
            return Err(WorkerLaunchError::Win32(last_error("ResumeThread")));
        }

        // SAFETY: The primary thread handle is owned exactly once by `self`.
        // It is no longer needed after the thread has been resumed.
        unsafe {
            let _ = CloseHandle(self.primary_thread);
        }
        self.primary_thread = null_mut();
        let process = self.process;
        self.process = null_mut();
        Ok(RunningInternalWorker {
            process,
            identity: self.identity,
            shutdown_signal: self
                .shutdown_signal
                .take()
                .expect("suspended worker always owns its shutdown signal"),
        })
    }
}

impl Drop for SuspendedInternalWorker {
    fn drop(&mut self) {
        // SAFETY: Any remaining handles are owned by this value. The process
        // is either still suspended or failed to resume, so terminating it is
        // required to uphold the no-unmanaged-worker guarantee. Closing null
        // handles is avoided after ownership has moved into RunningInternalWorker.
        unsafe {
            if !self.process.is_null() {
                let _ = TerminateProcess(self.process, 1);
            }
            if !self.primary_thread.is_null() {
                let _ = CloseHandle(self.primary_thread);
            }
            if !self.process.is_null() {
                let _ = CloseHandle(self.process);
            }
        }
    }
}

/// A verified, resumed fixed internal worker. Its raw process handle remains
/// private; the owner can retain it only through the trusted worker manager.
pub struct RunningInternalWorker {
    process: HANDLE,
    identity: InternalWorkerIdentity,
    shutdown_signal: CooperativeShutdownSignal,
}

impl RunningInternalWorker {
    #[must_use]
    pub const fn identity(&self) -> InternalWorkerIdentity {
        self.identity
    }

    /// Signals the fixed worker's sole cooperative operation: exit. It cannot
    /// carry a command, argument, executable path, or arbitrary IPC payload.
    pub fn request_cooperative_shutdown(&self) -> Result<(), Win32Error> {
        self.shutdown_signal.signal()
    }

    /// Observes whether the exact process handle has exited. The identity was
    /// captured before resume, so this never follows a reused PID.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<bool, Win32Error> {
        match wait_for_handle(self.process, timeout)? {
            WaitState::Signaled => Ok(true),
            WaitState::TimedOut => Ok(false),
        }
    }

    /// Returns the worker exit code only after the exact process handle is
    /// observed signaled. It never queries a PID that could have been reused.
    pub fn exited_with_code(&self) -> Result<Option<u32>, Win32Error> {
        if !self.wait_for_exit(Duration::ZERO)? {
            return Ok(None);
        }
        let mut exit_code = 0;
        // SAFETY: `process` is the still-owned process handle returned by
        // CreateProcessW, and `exit_code` is a valid writable output location.
        let queried = unsafe { GetExitCodeProcess(self.process, &mut exit_code) };
        if queried == 0 {
            return Err(last_error("GetExitCodeProcess"));
        }
        Ok(Some(exit_code))
    }
}

impl Drop for RunningInternalWorker {
    fn drop(&mut self) {
        // SAFETY: The process handle is owned exactly once by this wrapper.
        // The Job Object, rather than closing this observation handle, owns
        // the worker lifetime and applies KILL_ON_JOB_CLOSE when appropriate.
        unsafe {
            let _ = CloseHandle(self.process);
        }
    }
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned exactly once by this wrapper. Closing the
        // final handle intentionally applies KILL_ON_JOB_CLOSE only to
        // processes previously assigned to this unnamed Jarvis-owned job.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Waits for the current fixed worker's manager-created shutdown event. This
/// is the only worker-side control operation currently supported.
pub fn wait_for_current_fixed_worker_shutdown(
    worker: FixedInternalWorker,
) -> Result<(), Win32Error> {
    // SAFETY: GetCurrentProcess returns the documented pseudo-handle for this
    // process, and GetCurrentProcessId has no pointer requirements.
    let identity = process_identity(unsafe { GetCurrentProcess() }, unsafe {
        GetCurrentProcessId()
    })?;
    let name = shutdown_signal_name(worker, identity);
    // SAFETY: `name` is a null-terminated UTF-16 value that remains live for
    // this synchronous call. The worker asks only for wait/synchronize access
    // to its own manager-created event and does not inherit the handle.
    let event = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if event.is_null() {
        return Err(last_error("OpenEventW"));
    }
    let result = wait_for_handle(event, Duration::from_millis(u64::from(INFINITE)));
    // SAFETY: `event` was returned as an owned handle by OpenEventW and is not
    // used after this close.
    unsafe {
        let _ = CloseHandle(event);
    }
    match result? {
        WaitState::Signaled => Ok(()),
        WaitState::TimedOut => Err(Win32Error {
            operation: "WaitForSingleObject unexpectedly timed out",
            code: WAIT_TIMEOUT,
        }),
    }
}

/// An owned, per-worker named event created before the worker is resumed.
/// Its name includes both PID and creation time, preventing PID reuse from
/// addressing a later worker. A pre-existing name is rejected rather than
/// reused.
struct CooperativeShutdownSignal {
    handle: HANDLE,
}

impl CooperativeShutdownSignal {
    fn create(
        worker: FixedInternalWorker,
        identity: InternalWorkerIdentity,
    ) -> Result<Self, WorkerLaunchError> {
        let name = shutdown_signal_name(worker, identity);
        // SAFETY: The event is non-inheritable by passing null security
        // attributes, starts nonsignaled, and uses a null-terminated UTF-16
        // name that remains live for this synchronous call.
        let handle = unsafe { CreateEventW(null(), 1, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(WorkerLaunchError::Win32(last_error("CreateEventW")));
        }
        // SAFETY: CreateEventW documents GetLastError as ERROR_ALREADY_EXISTS
        // when it opens an existing named event. This read is immediately
        // after the call, before any other Win32 operation.
        let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already_exists {
            // SAFETY: `handle` is the just-opened event handle and is owned by
            // this function until it is returned.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(WorkerLaunchError::ShutdownSignalAlreadyExists);
        }
        Ok(Self { handle })
    }

    fn signal(&self) -> Result<(), Win32Error> {
        // SAFETY: `handle` is a live event handle owned by this wrapper. The
        // event carries no payload; signaling it can only request worker exit.
        let signaled = unsafe { SetEvent(self.handle) };
        if signaled == 0 {
            return Err(last_error("SetEvent"));
        }
        Ok(())
    }
}

impl Drop for CooperativeShutdownSignal {
    fn drop(&mut self) {
        // SAFETY: The named event handle is owned exactly once by this
        // wrapper. Closing it cannot affect any desktop process.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitState {
    Signaled,
    TimedOut,
}

fn wait_for_handle(handle: HANDLE, timeout: Duration) -> Result<WaitState, Win32Error> {
    let timeout_millis = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
    // SAFETY: `handle` is a live process or event handle owned by the caller,
    // and the bounded timeout is passed by value to this synchronous wait.
    let result = unsafe { WaitForSingleObject(handle, timeout_millis) };
    match result {
        WAIT_OBJECT_0 => Ok(WaitState::Signaled),
        WAIT_TIMEOUT => Ok(WaitState::TimedOut),
        WAIT_FAILED => Err(last_error("WaitForSingleObject")),
        unexpected => Err(Win32Error {
            operation: "WaitForSingleObject returned an unexpected state",
            code: unexpected,
        }),
    }
}

fn last_error(operation: &'static str) -> Win32Error {
    // SAFETY: GetLastError reads thread-local state and requires no pointers.
    let code = unsafe { GetLastError() };
    Win32Error { operation, code }
}

fn process_identity(
    process: HANDLE,
    process_id: u32,
) -> Result<InternalWorkerIdentity, Win32Error> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: `process` is a live handle returned by CreateProcessW, and the
    // four FILETIME outputs are initialized writable values for this synchronous
    // query. The process is still suspended while the identity is captured.
    let queried =
        unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) };
    if queried == 0 {
        return Err(last_error("GetProcessTimes"));
    }
    Ok(InternalWorkerIdentity {
        process_id,
        creation_time_filetime: filetime_value(created),
    })
}

fn shutdown_signal_name(worker: FixedInternalWorker, identity: InternalWorkerIdentity) -> Vec<u16> {
    let name = format!(
        "Local\\Jarvis.{}.Shutdown.{}.{}",
        worker.executable_name(),
        identity.process_id(),
        identity.creation_time_filetime()
    );
    name.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_null(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn filetime_value(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_job_has_no_active_processes() {
        let job = KillOnCloseJob::create().expect("create an empty Job Object");
        assert_eq!(job.active_process_count().expect("query Job Object"), 0);
    }
}
