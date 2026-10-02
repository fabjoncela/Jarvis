//! Fixed, same-user named-pipe boundary for the one resident policy supervisor.
//!
//! The protocol is deliberately limited to status, the fixed FACEIT/CS2
//! handoff, and the fixed worker containment self-check. It carries no paths,
//! PIDs, command lines, or arbitrary action payloads.

use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::ptr::{null, null_mut};

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
const MAX_REQUEST_BYTES: usize = 96;
const MAX_RESPONSE_BYTES: usize = 2 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorControlCommand {
    Status,
    PrepareFaceitCs2,
    WorkerSelfCheck,
}

impl SupervisorControlCommand {
    const fn wire(self) -> &'static [u8] {
        match self {
            Self::Status => b"JARVIS/1 STATUS",
            Self::PrepareFaceitCs2 => b"JARVIS/1 PREPARE_FACEIT_CS2",
            Self::WorkerSelfCheck => b"JARVIS/1 WORKER_SELF_CHECK",
        }
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"JARVIS/1 STATUS" => Some(Self::Status),
            b"JARVIS/1 PREPARE_FACEIT_CS2" => Some(Self::PrepareFaceitCs2),
            b"JARVIS/1 WORKER_SELF_CHECK" => Some(Self::WorkerSelfCheck),
            _ => None,
        }
    }
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
        let pipe_name = wide(PIPE_NAME);
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
    let pipe_name = wide(PIPE_NAME);
    // SAFETY: the pipe name is NUL-terminated and remains live for this call.
    let waited = unsafe { WaitNamedPipeW(pipe_name.as_ptr(), 1_500) };
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
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error("open"));
    }
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
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn fixed_protocol_round_trips_through_same_executable() {
        let (ready_sender, ready_receiver) = mpsc::channel();
        let server_thread = std::thread::spawn(move || {
            let mut server = SupervisorControlPipeServer::bind().expect("bind test pipe");
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
            SupervisorControlPipeServer::bind().err(),
            Some(SupervisorControlPipeError::AlreadyRunning)
        );
        let response = send_supervisor_control_request(SupervisorControlCommand::Status)
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
}
