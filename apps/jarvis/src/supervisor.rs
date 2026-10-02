//! Serialized, non-executing supervisor foundation.
//!
//! `Supervisor` is the sole owner of profile state, configured routing, action
//! validation, the fixed internal-worker manifest, and an empty owned Job
//! Object. It has no event loop yet, so each current CLI invocation creates it
//! for validation and drops it before exit.

use std::error::Error;
use std::fmt;
use std::path::Path;

use jarvis_actions::{ActionRegistry, ValidatedPlan, ValidationError};
use jarvis_config::{ConfigError, FaceitSetupStatus, ValidatedConfiguration};
use jarvis_core::{ProfileGeneration, RequestContext, RequestId};
use jarvis_profiles::{Profile, ProfileController};
use jarvis_router::{RouteResult, Router};
use jarvis_windows::{Win32Error, WindowsOwnedWorkerJob, WorkerManifest};

#[derive(Debug)]
pub enum SupervisorError {
    Configuration(ConfigError),
    WorkerJob(Win32Error),
}

impl fmt::Display for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(formatter, "configuration rejected: {error}"),
            Self::WorkerJob(error) => {
                write!(formatter, "owned worker Job Object unavailable: {error}")
            }
        }
    }
}

impl Error for SupervisorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            Self::WorkerJob(error) => Some(error),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisorStatus {
    pub profile: Profile,
    pub generation: ProfileGeneration,
    pub action_gate_open: bool,
    pub worker_start_gate_open: bool,
    pub active_owned_worker_processes: u32,
    pub configured_internal_worker_count: usize,
    pub faceit_setup_status: FaceitSetupStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Validated(ValidatedPlan),
    InvalidPattern(String),
    Unknown,
}

/// The one authoritative mutable owner for a future long-lived Jarvis process.
///
/// All request admission takes `&mut self`, making the serialized ownership
/// explicit even though the current validation CLI does not yet run an event
/// loop, process monitor, or worker launcher.
#[derive(Debug)]
pub struct Supervisor {
    configuration: ValidatedConfiguration,
    controller: ProfileController,
    router: Router,
    registry: ActionRegistry,
    worker_manifest: WorkerManifest,
    owned_worker_job: WindowsOwnedWorkerJob,
}

impl Supervisor {
    pub fn bootstrap(configuration_directory: &Path) -> Result<Self, SupervisorError> {
        let configuration = ValidatedConfiguration::load_from_dir(configuration_directory)
            .map_err(SupervisorError::Configuration)?;
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
        let owned_worker_job =
            WindowsOwnedWorkerJob::create().map_err(SupervisorError::WorkerJob)?;

        Ok(Self {
            configuration,
            controller: ProfileController::new(),
            router,
            registry,
            worker_manifest: WorkerManifest,
            owned_worker_job,
        })
    }

    pub fn status(&self) -> Result<SupervisorStatus, SupervisorError> {
        Ok(SupervisorStatus {
            profile: self.controller.profile(),
            generation: self.controller.generation(),
            action_gate_open: self.controller.action_gate_open(),
            worker_start_gate_open: self.controller.worker_start_gate_open(),
            active_owned_worker_processes: self
                .owned_worker_job
                .active_process_count()
                .map_err(SupervisorError::WorkerJob)?,
            configured_internal_worker_count: self.worker_manifest.all().len(),
            faceit_setup_status: self.configuration.faceit_setup_status(),
        })
    }

    /// Validates a deterministic plan against the current supervisor-owned
    /// profile generation. It never dispatches an operating-system action.
    pub fn route(&mut self, command: &str) -> Result<RouteOutcome, ValidationError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        match self.router.route(command) {
            RouteResult::Plan(plan) => self
                .registry
                .validate(plan, request, &self.controller)
                .map(RouteOutcome::Validated),
            RouteResult::InvalidPattern { reason } => Ok(RouteOutcome::InvalidPattern(reason)),
            RouteResult::Unknown => Ok(RouteOutcome::Unknown),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config_directory() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
    }

    #[test]
    fn supervisor_owns_an_empty_job_and_fixed_worker_manifest() {
        let supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let status = supervisor.status().expect("query empty owned Job Object");
        assert_eq!(status.profile, Profile::GodMode);
        assert_eq!(status.active_owned_worker_processes, 0);
        assert_eq!(status.configured_internal_worker_count, 1);
    }

    #[test]
    fn route_is_validated_by_the_supervisor_without_execution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        assert!(matches!(
            supervisor.route("Jarvis, open Discord"),
            Ok(RouteOutcome::Validated(_))
        ));
    }
}
