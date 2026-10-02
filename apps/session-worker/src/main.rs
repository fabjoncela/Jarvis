#![forbid(unsafe_code)]

//! Placeholder for the future God Mode session worker.
//! It is started only by the fixed-worker containment self-check today and
//! accepts only its manager-created cooperative shutdown event.

use std::process::ExitCode;

use jarvis_windows_ffi::{wait_for_current_fixed_worker_shutdown, FixedInternalWorker};

fn main() -> ExitCode {
    match wait_for_current_fixed_worker_shutdown(FixedInternalWorker::Session) {
        Ok(()) => {
            println!("Session worker received its fixed cooperative shutdown request.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Session worker could not open its manager shutdown signal: {error}");
            ExitCode::from(2)
        }
    }
}
