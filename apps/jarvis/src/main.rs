#![forbid(unsafe_code)]

//! Minimal supervisor executable for the side-effect-free foundation.

use std::env;
use std::process::ExitCode;

use jarvis_actions::ActionRegistry;
use jarvis_core::{RequestContext, RequestId};
use jarvis_profiles::ProfileController;
use jarvis_router::{RouteResult, Router};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Jarvis: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let controller = ProfileController::new();
    match arguments.as_slice() {
        [] => self_check(&controller),
        [flag] if flag == "--self-check" => self_check(&controller),
        [flag, command] if flag == "--route" => route(command, &controller),
        _ => Err("usage: jarvis [--self-check | --route <command>]".to_owned()),
    }
}

fn self_check(controller: &ProfileController) -> Result<(), String> {
    println!("Jarvis foundation self-check");
    println!("profile: {:?}", controller.profile());
    println!("profile generation: {}", controller.generation().value());
    println!("action gate: {}", controller.action_gate_open());
    println!("worker-start gate: {}", controller.worker_start_gate_open());
    println!("execution: disabled (validation only)");
    Ok(())
}

fn route(command: &str, controller: &ProfileController) -> Result<(), String> {
    let router = Router::starter_router();
    let registry = ActionRegistry::starter_registry();
    let request = RequestContext::new(RequestId::next(), controller.generation());
    match router.route(command) {
        RouteResult::Plan(plan) => {
            let validated = registry
                .validate(plan, request, controller)
                .map_err(|error| format!("plan rejected: {error:?}"))?;
            println!("request: {}", validated.request().request_id);
            println!("validated actions: {:?}", validated.actions());
            println!(
                "confirmation required: {}",
                validated.requires_confirmation()
            );
            println!("execution: disabled in this foundation build");
            Ok(())
        }
        RouteResult::InvalidPattern { reason } => Err(reason),
        RouteResult::Unknown => {
            Err("no deterministic route; model fallback is not implemented".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_check_is_available_without_arguments() {
        assert_eq!(run(Vec::new()), Ok(()));
    }

    #[test]
    fn known_route_does_not_execute() {
        assert_eq!(
            route("Jarvis, open Discord", &ProfileController::new()),
            Ok(())
        );
    }
}
