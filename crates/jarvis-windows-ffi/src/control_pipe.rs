//! Fixed, same-user named-pipe boundary for the one resident policy supervisor.
//!
//! The protocol is deliberately limited to status, the fixed FACEIT/CS2
//! handoff, the fixed worker containment self-check, and bounded preferred-app
//! memory requests. It carries no paths, PIDs, command lines, or arbitrary
//! action payloads.

use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ACCESS_DENIED, ERROR_BROKEN_PIPE, ERROR_MORE_DATA,
    ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    GetNamedPipeServerProcessId, SetNamedPipeHandleState, WaitNamedPipeW, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};

const PIPE_NAME: &str = r"\\.\pipe\Jarvis.CompetitiveSupervisor.v1";
const PIPE_OWNER_DACL: &str = "D:P(A;;GA;;;OW)";
const MAX_REQUEST_BYTES: usize = 160;
const MAX_RESPONSE_BYTES: usize = 2 * 1024;
const REMEMBER_PREFERRED_APP_PREFIX: &str = "JARVIS/1 REMEMBER_PREFERRED_APP ";
const FORGET_PREFERRED_APP_PREFIX: &str = "JARVIS/1 FORGET_PREFERRED_APP ";
const RESTORE_PREFERRED_APP_PREFIX: &str = "JARVIS/1 RESTORE_PREFERRED_APP ";

/// The complete set of application references accepted by the preferred-app
/// control protocol. These are identifiers, never executable paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreferredApp {
    Chrome,
    Discord,
}

impl PreferredApp {
    #[must_use]
    pub const fn application_id(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Discord => "discord",
        }
    }

    const fn wire(self) -> &'static str {
        match self {
            Self::Chrome => "CHROME",
            Self::Discord => "DISCORD",
        }
    }

    fn parse_wire(value: &str) -> Option<Self> {
        match value {
            "CHROME" => Some(Self::Chrome),
            "DISCORD" => Some(Self::Discord),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorControlCommand {
    Status,
    PrepareFaceitCs2,
    WorkerSelfCheck,
    RememberPreferredApp {
        event_id: u128,
        expected_generation: u64,
        app: PreferredApp,
    },
    ShowPreferredApp,
    PreferredAppState,
    ForgetPreferredApp {
        event_id: u128,
        expected_generation: u64,
        expected_event_id: u128,
    },
    RestorePreferredApp {
        event_id: u128,
        expected_generation: u64,
        app: PreferredApp,
        expected_tombstone_id: u128,
    },
}

impl SupervisorControlCommand {
    fn wire(self) -> Vec<u8> {
        match self {
            Self::Status => b"JARVIS/1 STATUS".to_vec(),
            Self::PrepareFaceitCs2 => b"JARVIS/1 PREPARE_FACEIT_CS2".to_vec(),
            Self::WorkerSelfCheck => b"JARVIS/1 WORKER_SELF_CHECK".to_vec(),
            Self::RememberPreferredApp {
                event_id,
                expected_generation,
                app,
            } => format!(
                "{REMEMBER_PREFERRED_APP_PREFIX}{event_id:032x} {expected_generation} {}",
                app.wire()
            )
            .into_bytes(),
            Self::ShowPreferredApp => b"JARVIS/1 SHOW_PREFERRED_APP".to_vec(),
            Self::PreferredAppState => b"JARVIS/1 PREFERRED_APP_STATE".to_vec(),
            Self::ForgetPreferredApp { event_id, expected_generation, expected_event_id } =>
                format!("{FORGET_PREFERRED_APP_PREFIX}{event_id:032x} {expected_generation} {expected_event_id:032x}").into_bytes(),
            Self::RestorePreferredApp { event_id, expected_generation, app, expected_tombstone_id } =>
                format!("{RESTORE_PREFERRED_APP_PREFIX}{event_id:032x} {expected_generation} {} {expected_tombstone_id:032x}", app.wire()).into_bytes(),
        }
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_REQUEST_BYTES {
            return None;
        }
        match bytes {
            b"JARVIS/1 STATUS" => Some(Self::Status),
            b"JARVIS/1 PREPARE_FACEIT_CS2" => Some(Self::PrepareFaceitCs2),
            b"JARVIS/1 WORKER_SELF_CHECK" => Some(Self::WorkerSelfCheck),
            b"JARVIS/1 SHOW_PREFERRED_APP" => Some(Self::ShowPreferredApp),
            b"JARVIS/1 PREFERRED_APP_STATE" => Some(Self::PreferredAppState),
            _ => {
                let text = std::str::from_utf8(bytes).ok()?;
                if let Some(fields) = text.strip_prefix(FORGET_PREFERRED_APP_PREFIX) {
                    let mut parts = fields.split(' ');
                    let event_id = parse_event_id(parts.next()?)?;
                    let expected_generation = parse_generation(parts.next()?)?;
                    let expected_event_id = parse_event_id(parts.next()?)?;
                    return parts.next().is_none().then_some(Self::ForgetPreferredApp {
                        event_id,
                        expected_generation,
                        expected_event_id,
                    });
                }
                if let Some(fields) = text.strip_prefix(RESTORE_PREFERRED_APP_PREFIX) {
                    let mut parts = fields.split(' ');
                    let event_id = parse_event_id(parts.next()?)?;
                    let expected_generation = parse_generation(parts.next()?)?;
                    let app = PreferredApp::parse_wire(parts.next()?)?;
                    let expected_tombstone_id = parse_event_id(parts.next()?)?;
                    return parts.next().is_none().then_some(Self::RestorePreferredApp {
                        event_id,
                        expected_generation,
                        app,
                        expected_tombstone_id,
                    });
                }
                let fields = text.strip_prefix(REMEMBER_PREFERRED_APP_PREFIX)?;
                let mut parts = fields.split(' ');
                let event_hex = parts.next()?;
                let generation_text = parts.next()?;
                let app_text = parts.next()?;
                if parts.next().is_some()
                    || event_hex.len() != 32
                    || !event_hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || generation_text.is_empty()
                    || generation_text.len() > 1 && generation_text.starts_with('0')
                    || !generation_text.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return None;
                }
                Some(Self::RememberPreferredApp {
                    event_id: u128::from_str_radix(event_hex, 16).ok()?,
                    expected_generation: generation_text.parse().ok()?,
                    app: PreferredApp::parse_wire(app_text)?,
                })
            }
        }
    }
}

fn parse_event_id(value: &str) -> Option<u128> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    u128::from_str_radix(value, 16).ok()
}

fn parse_generation(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SupervisorControlPipeError {
    AlreadyRunning,
    Unavailable { operation: &'static str, code: u32 },
    WrongServerImage,
    InvalidResponse,
    ResponseTooLarge,
}

impl fmt::Display for SupervisorControlPipeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => {
                write!(formatter, "the authoritative supervisor is already running")
            }
            Self::Unavailable { operation, code } => {
                write!(
                    formatter,
                    "supervisor control pipe {operation} failed (Win32 {code})"
                )
            }
            Self::WrongServerImage => write!(
                formatter,
                "the control pipe is not owned by this Jarvis executable"
            ),
            Self::InvalidResponse => write!(
                formatter,
                "the supervisor returned an invalid control response"
            ),
            Self::ResponseTooLarge => write!(
                formatter,
                "the supervisor control response exceeded its size limit"
            ),
        }
    }
}

impl Error for SupervisorControlPipeError {}

pub struct SupervisorControlPipeServer {
    handle: HANDLE,
}

impl SupervisorControlPipeServer {
    pub fn bind() -> Result<Self, SupervisorControlPipeError> {
        Self::bind_named(PIPE_NAME)
    }

    fn bind_named(name: &str) -> Result<Self, SupervisorControlPipeError> {
        let pipe_name = wide(name);
        let dacl = wide(PIPE_OWNER_DACL);
        let mut security_descriptor: *mut c_void = null_mut();

        // SAFETY: both UTF-16 strings are NUL-terminated and remain live for
        // this call; the output pointer is valid and initialized to null.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                dacl.as_ptr(),
                SDDL_REVISION_1,
                &mut security_descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(last_error("DACL conversion"));
        }

        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security_descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: the security descriptor came from the system SDDL converter,
        // the attributes live through CreateNamedPipeW, and the pipe name is
        // NUL-terminated. The first-instance flag prevents joining an existing
        // pipe namespace entry; the DACL grants access only to the object owner.
        let handle = unsafe {
            CreateNamedPipeW(
                pipe_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                MAX_RESPONSE_BYTES as u32,
                MAX_REQUEST_BYTES as u32,
                0,
                &attributes,
            )
        };
        let creation_error = if handle == INVALID_HANDLE_VALUE {
            // SAFETY: capture the error immediately after the failed pipe
            // creation call on this thread.
            Some(unsafe { GetLastError() })
        } else {
            None
        };
        // SAFETY: this descriptor was allocated by the SDDL conversion API and
        // is no longer needed after CreateNamedPipeW has copied it.
        unsafe {
            let _ = LocalFree(security_descriptor);
        }

        if let Some(code) = creation_error {
            return if code == ERROR_ACCESS_DENIED || code == ERROR_PIPE_BUSY {
                Err(SupervisorControlPipeError::AlreadyRunning)
            } else {
                Err(SupervisorControlPipeError::Unavailable {
                    operation: "creation",
                    code,
                })
            };
        }

        Ok(Self { handle })
    }

    /// Accepts and answers one fixed request. `handler` executes on the
    /// caller's thread so the application can route every operation through
    /// its sole mutable Supervisor owner.
    pub fn serve_one(
        &mut self,
        mut handler: impl FnMut(SupervisorControlCommand) -> String,
    ) -> Result<(), SupervisorControlPipeError> {
        // SAFETY: `self.handle` is a live synchronous server-pipe handle. A
        // client may already have connected, which is the documented
        // ERROR_PIPE_CONNECTED success case.
        let connected = unsafe { ConnectNamedPipe(self.handle, null_mut()) };
        if connected == 0 {
            // SAFETY: capture the error immediately after ConnectNamedPipe.
            let code = unsafe { GetLastError() };
            if code != ERROR_PIPE_CONNECTED {
                return Err(SupervisorControlPipeError::Unavailable {
                    operation: "accept",
                    code,
                });
            }
        }

        let result = self.handle_connected_client(&mut handler);
        // SAFETY: this handle is the connected server end for this one client;
        // it is disconnected before the same instance is reused.
        let disconnected = unsafe { DisconnectNamedPipe(self.handle) };
        result?;
        if disconnected == 0 {
            // SAFETY: capture the error immediately after DisconnectNamedPipe.
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_DATA && code != ERROR_BROKEN_PIPE {
                return Err(SupervisorControlPipeError::Unavailable {
                    operation: "disconnect",
                    code,
                });
            }
        }
        Ok(())
    }

    fn handle_connected_client(
        &mut self,
        handler: &mut impl FnMut(SupervisorControlCommand) -> String,
    ) -> Result<(), SupervisorControlPipeError> {
        if !client_image_matches(self.handle)? {
            write_reply(self.handle, b"ERR CLIENT_IDENTITY")?;
            return Ok(());
        }

        let mut request = [0_u8; MAX_REQUEST_BYTES];
        let mut bytes_read = 0_u32;
        // SAFETY: `request` is a writable buffer of the specified capacity;
        // the handle is connected and synchronous, so a null OVERLAPPED is valid.
        let read = unsafe {
            ReadFile(
                self.handle,
                request.as_mut_ptr(),
                request.len() as u32,
                &mut bytes_read,
                null_mut(),
            )
        };
        if read == 0 {
            // SAFETY: capture the error immediately after the failed ReadFile.
            let code = unsafe { GetLastError() };
            if code == ERROR_MORE_DATA {
                write_reply(self.handle, b"ERR REQUEST_TOO_LARGE")?;
                return Ok(());
            }
            if code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA {
                return Ok(());
            }
            return Err(SupervisorControlPipeError::Unavailable {
                operation: "request read",
                code,
            });
        }

        let Some(command) = SupervisorControlCommand::parse(&request[..bytes_read as usize]) else {
            write_reply(self.handle, b"ERR REQUEST")?;
            return Ok(());
        };
        let response = handler(command);
        if response.is_empty()
            || response.len() > MAX_RESPONSE_BYTES
            || !response.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        {
            write_reply(self.handle, b"ERR RESPONSE")?;
            return Ok(());
        }
        write_reply(self.handle, response.as_bytes())
    }
}

impl Drop for SupervisorControlPipeServer {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this server and is not used afterwards.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn send_supervisor_control_request(
    command: SupervisorControlCommand,
) -> Result<String, SupervisorControlPipeError> {
    send_supervisor_control_request_to(PIPE_NAME, command)
}

fn send_supervisor_control_request_to(
    name: &str,
    command: SupervisorControlCommand,
) -> Result<String, SupervisorControlPipeError> {
    let pipe_name = wide(name);
    let deadline = Instant::now() + Duration::from_millis(1_500);
    let handle = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SupervisorControlPipeError::Unavailable {
                operation: "wait",
                code: ERROR_PIPE_BUSY,
            });
        }
        // SAFETY: the pipe name is NUL-terminated and remains live for this call.
        let waited =
            unsafe { WaitNamedPipeW(pipe_name.as_ptr(), remaining.as_millis().max(1) as u32) };
        if waited == 0 {
            // SAFETY: capture the error immediately after WaitNamedPipeW.
            let code = unsafe { GetLastError() };
            return Err(SupervisorControlPipeError::Unavailable {
                operation: "wait",
                code,
            });
        }

        // SAFETY: the name is valid, and the client requests only synchronous
        // read/write access to the fixed local pipe.
        let handle = unsafe {
            CreateFileW(
                pipe_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            break handle;
        }
        // Another client can win the instance between WaitNamedPipe and
        // CreateFile; retry only that race within the original deadline.
        // SAFETY: capture the error immediately after CreateFileW.
        let code = unsafe { GetLastError() };
        if code != ERROR_PIPE_BUSY {
            return Err(SupervisorControlPipeError::Unavailable {
                operation: "open",
                code,
            });
        }
    };
    let result = send_connected_request(handle, command);
    // SAFETY: this client handle is owned by the current function.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

fn send_connected_request(
    handle: HANDLE,
    command: SupervisorControlCommand,
) -> Result<String, SupervisorControlPipeError> {
    if !server_image_matches(handle)? {
        return Err(SupervisorControlPipeError::WrongServerImage);
    }

    let read_mode = PIPE_READMODE_MESSAGE;
    // SAFETY: `handle` is an open named-pipe client handle and `read_mode` is
    // a writable mode value supported by the message-type server pipe.
    let configured = unsafe { SetNamedPipeHandleState(handle, &read_mode, null(), null()) };
    if configured == 0 {
        return Err(last_error("set message mode"));
    }

    let request = command.wire();
    let mut bytes_written = 0_u32;
    // SAFETY: the fixed request slice remains live and the pipe handle is
    // synchronous; `bytes_written` is a valid out pointer.
    let written = unsafe {
        WriteFile(
            handle,
            request.as_ptr(),
            request.len() as u32,
            &mut bytes_written,
            null_mut(),
        )
    };
    if written == 0 || bytes_written as usize != request.len() {
        return Err(last_error("request write"));
    }

    let mut response = vec![0_u8; MAX_RESPONSE_BYTES];
    let mut bytes_read = 0_u32;
    // SAFETY: `response` is a writable buffer of the specified capacity and
    // the connected pipe is synchronous.
    let read = unsafe {
        ReadFile(
            handle,
            response.as_mut_ptr(),
            response.len() as u32,
            &mut bytes_read,
            null_mut(),
        )
    };
    if read == 0 {
        // SAFETY: capture the error immediately after the failed ReadFile.
        let code = unsafe { GetLastError() };
        return if code == ERROR_MORE_DATA {
            Err(SupervisorControlPipeError::ResponseTooLarge)
        } else {
            Err(SupervisorControlPipeError::Unavailable {
                operation: "response read",
                code,
            })
        };
    }
    response.truncate(bytes_read as usize);
    String::from_utf8(response).map_err(|_| SupervisorControlPipeError::InvalidResponse)
}

fn client_image_matches(pipe: HANDLE) -> Result<bool, SupervisorControlPipeError> {
    let mut process_id = 0_u32;
    // SAFETY: the server pipe is connected and `process_id` is a valid output
    // pointer for the connected client's kernel-reported process identity.
    let queried = unsafe { GetNamedPipeClientProcessId(pipe, &mut process_id) };
    if queried == 0 {
        return Err(last_error("client process query"));
    }
    process_image_path(process_id).map(|path| current_executable_path() == path)
}

fn server_image_matches(pipe: HANDLE) -> Result<bool, SupervisorControlPipeError> {
    let mut process_id = 0_u32;
    // SAFETY: the pipe is connected and `process_id` is a valid output pointer.
    let queried = unsafe { GetNamedPipeServerProcessId(pipe, &mut process_id) };
    if queried == 0 {
        return Err(last_error("server process query"));
    }
    process_image_path(process_id).map(|path| current_executable_path() == path)
}

fn process_image_path(process_id: u32) -> Result<PathBuf, SupervisorControlPipeError> {
    // SAFETY: only limited query rights are requested; the PID came from the
    // named-pipe kernel metadata and is not used to alter the target process.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process.is_null() {
        return Err(last_error("peer process open"));
    }

    let mut buffer = vec![0_u16; 32_768];
    let mut length = buffer.len() as u32;
    // SAFETY: `process` is a live limited-query handle and the UTF-16 buffer
    // remains writable for the synchronous query.
    let queried =
        unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };
    // SAFETY: this limited-query process handle is owned by the current function.
    unsafe {
        let _ = CloseHandle(process);
    }
    if queried == 0 || length as usize > buffer.len() {
        return Err(last_error("peer image query"));
    }

    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    path.canonicalize()
        .map_err(|_| SupervisorControlPipeError::InvalidResponse)
}

fn current_executable_path() -> PathBuf {
    std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .unwrap_or_default()
}

fn write_reply(handle: HANDLE, response: &[u8]) -> Result<(), SupervisorControlPipeError> {
    let mut bytes_written = 0_u32;
    // SAFETY: the response slice remains live and is bounded before reaching
    // this helper; the pipe is connected and synchronous.
    let written = unsafe {
        WriteFile(
            handle,
            response.as_ptr(),
            response.len() as u32,
            &mut bytes_written,
            null_mut(),
        )
    };
    if written == 0 || bytes_written as usize != response.len() {
        // SAFETY: capture the error immediately after the failed WriteFile.
        let code = unsafe { GetLastError() };
        if code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA {
            return Ok(());
        }
        return Err(SupervisorControlPipeError::Unavailable {
            operation: "response write",
            code,
        });
    }
    // SAFETY: the connected client reads exactly one bounded reply before
    // returning, so this flush is bounded by the local request/response pair.
    let flushed = unsafe { FlushFileBuffers(handle) };
    if flushed == 0 {
        // SAFETY: capture the error immediately after the failed flush.
        let code = unsafe { GetLastError() };
        if code == ERROR_BROKEN_PIPE || code == ERROR_NO_DATA {
            return Ok(());
        }
        return Err(SupervisorControlPipeError::Unavailable {
            operation: "response flush",
            code,
        });
    }
    Ok(())
}

fn last_error(operation: &'static str) -> SupervisorControlPipeError {
    SupervisorControlPipeError::Unavailable {
        operation,
        // SAFETY: reading the calling thread's last Win32 error immediately
        // follows the failed API operation.
        code: unsafe { GetLastError() },
    }
}

fn wide(value: &str) -> Vec<u16> {
    std::ffi::OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    static NEXT_TEST_PIPE_ID: AtomicUsize = AtomicUsize::new(1);

    #[test]
    fn fixed_protocol_round_trips_through_same_executable() {
        let pipe_name = format!(
            r"\\.\pipe\Jarvis.ControlPipeTest.{}.{}",
            std::process::id(),
            NEXT_TEST_PIPE_ID.fetch_add(1, Ordering::Relaxed),
        );
        let server_pipe_name = pipe_name.clone();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let mut server =
                SupervisorControlPipeServer::bind_named(&server_pipe_name).expect("bind test pipe");
            ready_sender.send(()).expect("signal server ready");
            server
                .serve_one(|command| {
                    assert_eq!(command, SupervisorControlCommand::Status);
                    "OK STATUS PROFILE=GodMode".to_owned()
                })
                .expect("serve one status request");
        });

        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("server ready");
        assert_eq!(
            SupervisorControlPipeServer::bind_named(&pipe_name).err(),
            Some(SupervisorControlPipeError::AlreadyRunning)
        );
        let response =
            send_supervisor_control_request_to(&pipe_name, SupervisorControlCommand::Status)
                .expect("send status request");
        assert_eq!(response, "OK STATUS PROFILE=GodMode");
        server_thread.join().expect("server thread should finish");
    }

    #[test]
    fn wire_parser_rejects_arbitrary_payloads() {
        assert_eq!(
            SupervisorControlCommand::parse(b"JARVIS/1 STATUS"),
            Some(SupervisorControlCommand::Status)
        );
        assert_eq!(SupervisorControlCommand::parse(b"run powershell"), None);
        assert_eq!(
            SupervisorControlCommand::parse(b"JARVIS/1 STATUS --pid 42"),
            None
        );
    }

    #[test]
    fn preferred_app_commands_have_canonical_bounded_round_trips() {
        for app in [PreferredApp::Chrome, PreferredApp::Discord] {
            let command = SupervisorControlCommand::RememberPreferredApp {
                event_id: u128::MAX,
                expected_generation: u64::MAX,
                app,
            };
            let wire = command.wire();
            assert!(wire.len() <= MAX_REQUEST_BYTES);
            assert_eq!(SupervisorControlCommand::parse(&wire), Some(command));
        }
        let chrome = SupervisorControlCommand::RememberPreferredApp {
            event_id: 0xab,
            expected_generation: 0,
            app: PreferredApp::Chrome,
        };
        assert_eq!(
            chrome.wire(),
            b"JARVIS/1 REMEMBER_PREFERRED_APP 000000000000000000000000000000ab 0 CHROME"
        );
        assert_eq!(PreferredApp::Chrome.application_id(), "chrome");
        assert_eq!(PreferredApp::Discord.application_id(), "discord");
        assert_eq!(
            SupervisorControlCommand::parse(&SupervisorControlCommand::ShowPreferredApp.wire()),
            Some(SupervisorControlCommand::ShowPreferredApp)
        );
        assert_eq!(
            SupervisorControlCommand::parse(&SupervisorControlCommand::PreferredAppState.wire()),
            Some(SupervisorControlCommand::PreferredAppState)
        );
    }

    #[test]
    fn revision_bound_preference_commands_are_canonical_and_bounded() {
        let forget = SupervisorControlCommand::ForgetPreferredApp {
            event_id: u128::MAX,
            expected_generation: u64::MAX,
            expected_event_id: u128::MAX - 1,
        };
        let restore = SupervisorControlCommand::RestorePreferredApp {
            event_id: u128::MAX,
            expected_generation: u64::MAX,
            app: PreferredApp::Discord,
            expected_tombstone_id: u128::MAX - 1,
        };
        for command in [forget, restore] {
            let wire = command.wire();
            assert!(wire.len() <= MAX_REQUEST_BYTES);
            assert_eq!(SupervisorControlCommand::parse(&wire), Some(command));
            let canonical = String::from_utf8(wire).unwrap();
            for invalid in [
                format!("{canonical} "),
                format!("{canonical}\n"),
                canonical.replace("ffffffff", "FFFFFFFF"),
                canonical.replace(" 18446744073709551615 ", " 018446744073709551615 "),
            ] {
                assert_eq!(SupervisorControlCommand::parse(invalid.as_bytes()), None);
            }
        }
    }

    #[test]
    fn preferred_app_parser_rejects_noncanonical_or_oversized_requests() {
        let valid = SupervisorControlCommand::RememberPreferredApp {
            event_id: 0xab,
            expected_generation: 7,
            app: PreferredApp::Discord,
        }
        .wire();
        let valid = String::from_utf8(valid).expect("canonical wire is ASCII");
        let invalid = [
            valid.replace("000000ab", "000000AB"),
            valid.replace("000000ab", "00000ab"),
            valid.replace(" 7 ", " 07 "),
            valid.replace(" 7 ", " +7 "),
            valid.replace("DISCORD", "discord"),
            valid.replace("DISCORD", "CS2"),
            format!("{valid} "),
            format!("{valid}\n"),
            valid.replacen(' ', "  ", 1),
            valid.replace(" 7 ", " 18446744073709551616 "),
        ];
        for request in invalid {
            assert_eq!(
                SupervisorControlCommand::parse(request.as_bytes()),
                None,
                "must reject {request:?}"
            );
        }
        assert_eq!(
            SupervisorControlCommand::parse(&[b'X'; MAX_REQUEST_BYTES + 1]),
            None
        );
        assert_eq!(
            SupervisorControlCommand::parse(b"JARVIS/1 SHOW_PREFERRED_APP "),
            None
        );
    }

    #[test]
    fn preferred_app_request_round_trips_on_private_test_pipe() {
        let pipe_name = format!(
            r"\\.\pipe\Jarvis.ControlPipeTest.{}.{}",
            std::process::id(),
            NEXT_TEST_PIPE_ID.fetch_add(1, Ordering::Relaxed),
        );
        let server_pipe_name = pipe_name.clone();
        let command = SupervisorControlCommand::RememberPreferredApp {
            event_id: 0xabc,
            expected_generation: 3,
            app: PreferredApp::Chrome,
        };
        let (ready_sender, ready_receiver) = mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let mut server = SupervisorControlPipeServer::bind_named(&server_pipe_name)
                .expect("bind private test pipe");
            ready_sender.send(()).expect("signal server ready");
            server
                .serve_one(|received| {
                    assert_eq!(received, command);
                    "OK MEMORY SAVED".to_owned()
                })
                .expect("serve preferred-app request");
        });
        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("server ready");
        assert_eq!(
            send_supervisor_control_request_to(&pipe_name, command)
                .expect("send preferred-app request"),
            "OK MEMORY SAVED"
        );
        server_thread.join().expect("server thread should finish");
    }
}
