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

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, FILETIME, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, GetProcessTimes, ResumeThread, TerminateProcess, CREATE_SUSPENDED,
    PROCESS_INFORMATION, STARTUPINFOW,
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

        Ok(SuspendedInternalWorker {
            process: process_information.hProcess,
            primary_thread: process_information.hThread,
            identity,
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
}

impl RunningInternalWorker {
    #[must_use]
    pub const fn identity(&self) -> InternalWorkerIdentity {
        self.identity
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
