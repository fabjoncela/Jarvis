#![forbid(unsafe_code)]

//! Contained lifecycle shell for a future on-demand local model runtime.
//! This binary loads no model, opens no network endpoint, and accepts no
//! planning request. Its only current operation is cooperative shutdown.

use std::process::ExitCode;

use jarvis_windows_ffi::{wait_for_current_fixed_worker_shutdown, FixedInternalWorker};

fn main() -> ExitCode {
    match wait_for_current_fixed_worker_shutdown(FixedInternalWorker::Model) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Model worker could not open its manager shutdown signal: {error}");
            ExitCode::from(2)
        }
    }
}
