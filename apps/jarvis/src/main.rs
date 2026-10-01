#![forbid(unsafe_code)]

//! One-shot validation CLI for the non-executing foundation.
//!
//! It intentionally creates no resident supervisor, process monitor, worker,
//! or action executor. Its profile controller is a model used only to validate
//! the policy boundary in this process.

use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use jarvis_actions::ActionRegistry;
use jarvis_config::ValidatedConfiguration;
use jarvis_core::{RequestContext, RequestId};
use jarvis_profiles::ProfileController;
use jarvis_router::{RouteResult, Router};
use jarvis_windows::WindowsJobObjectProbe;

fn main() -> ExitCode {
    match config_directory().and_then(|directory| run(env::args().skip(1).collect(), &directory)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Jarvis: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(arguments: Vec<String>, configuration_directory: &Path) -> Result<(), String> {
    let configuration = ValidatedConfiguration::load_from_dir(configuration_directory)
        .map_err(|error| format!("configuration rejected: {error}"))?;
    let controller = ProfileController::new();
    let router = Router::from_catalog(configuration.applications());
    let registry = ActionRegistry::new(
        configuration
            .applications()
            .application_ids()
            .map(str::to_owned),
        configuration
            .applications()
            .protected_close_targets()
            .map(str::to_owned),
    );
    match arguments.as_slice() {
        [] => self_check(&controller, &configuration),
        [flag] if flag == "--self-check" => self_check(&controller, &configuration),
        [flag, command] if flag == "--route" => route(command, &controller, &router, &registry),
        _ => Err("usage: jarvis [--self-check | --route <command>]".to_owned()),
    }
}

fn self_check(
    controller: &ProfileController,
    configuration: &ValidatedConfiguration,
) -> Result<(), String> {
    let job_probe = WindowsJobObjectProbe::create()
        .map_err(|error| format!("Job Object capability check unavailable: {error}"))?;
    let active_processes = job_probe
        .active_process_count()
        .map_err(|error| format!("could not inspect empty Job Object: {error}"))?;
    println!("Jarvis V1 foundation self-check");
    println!("configuration: validated");
    println!("runtime supervisor: not running");
    println!("competitive process/service monitoring: not implemented");
    println!("worker launch and Job Object containment: not implemented");
    println!("initial profile model: {:?}", controller.profile());
    println!("profile generation: {}", controller.generation().value());
    println!(
        "action gate (model only): {}",
        controller.action_gate_open()
    );
    println!(
        "worker-start gate (model only): {}",
        controller.worker_start_gate_open()
    );
    println!(
        "Job Object capability check: empty unnamed object created; active processes: {active_processes}"
    );
    println!(
        "FACEIT identity verification (not a certification): {:?}",
        configuration.faceit_setup_status()
    );
    println!(
        "execution: disabled; no configured desktop action, application launch, or worker launch was performed"
    );
    Ok(())
}

fn route(
    command: &str,
    controller: &ProfileController,
    router: &Router,
    registry: &ActionRegistry,
) -> Result<(), String> {
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
            println!(
                "runtime: validation-only; no worker lifecycle or competitive monitor is running, and no configured desktop action was performed"
            );
            Ok(())
        }
        RouteResult::InvalidPattern { reason } => Err(reason),
        RouteResult::Unknown => {
            Err("no deterministic route; model fallback is not implemented".to_owned())
        }
    }
}

fn config_directory() -> Result<PathBuf, String> {
    let current_directory = env::current_dir().map_err(|error| error.to_string())?;
    current_directory
        .ancestors()
        .map(|ancestor| ancestor.join("config"))
        .find(|candidate| candidate.is_dir())
        .ok_or_else(|| {
            "could not find a config directory; run from the Jarvis repository root".to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
    }

    #[test]
    fn self_check_is_available_without_arguments() {
        assert_eq!(run(Vec::new(), &test_config_directory()), Ok(()));
    }

    #[test]
    fn known_route_does_not_execute() {
        assert_eq!(
            run(
                vec!["--route".to_owned(), "Jarvis, open Discord".to_owned()],
                &test_config_directory(),
            ),
            Ok(())
        );
    }
}
