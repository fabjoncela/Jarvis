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
use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_job_has_no_active_processes() {
        let job = KillOnCloseJob::create().expect("create an empty Job Object");
        assert_eq!(job.active_process_count().expect("query Job Object"), 0);
    }
}
