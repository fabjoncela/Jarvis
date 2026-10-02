#![forbid(unsafe_code)]

//! Placeholder for the future God Mode session worker.
//! It is started only by the fixed-worker containment self-check today.

use jarvis_core::{RequestContext, RequestId};
use jarvis_profiles::ProfileController;

fn main() {
    let controller = ProfileController::new();
    let request = RequestContext::new(RequestId::next(), controller.generation());
    match controller.authorize_worker_start(request) {
        Ok(()) => println!("Session worker boundary is available; no worker loop is implemented."),
        Err(error) => eprintln!("Session worker denied by profile policy: {error:?}"),
    }
}
