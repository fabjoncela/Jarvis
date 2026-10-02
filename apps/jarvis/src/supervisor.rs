//! Serialized supervisor foundation with one fixed pre-match capability.
//!
//! `Supervisor` is the sole owner of profile state, configured routing, action
//! validation, the fixed internal-worker manifest, and an empty owned Job
//! Object. It has no event loop yet, so each current CLI invocation creates it
//! for validation and drops it before exit.

use std::error::Error;
use std::fmt;
use std::path::Path;

use jarvis_actions::{Action, ActionPlan, ActionRegistry, ValidatedPlan, ValidationError};
use jarvis_config::{ConfigError, FaceitSetupStatus, ValidatedConfiguration};
use jarvis_core::{ProfileGeneration, RequestContext, RequestId};
use jarvis_gaming::{ensure_cs2_not_running, WindowsCs2ProcessSource};
use jarvis_profiles::{
    GateError, Profile, ProfileController, ShutdownVerification, TransitionCause, TransitionStart,
    TransitionState,
};
use jarvis_router::{RouteResult, Router};
use jarvis_windows::{
    run_pre_match_handoff, InternalWorker, PreMatchHandoffError, PreMatchHandoffReport, Win32Error,
    WindowsWorkerManager, WorkerManagerError, WorkerManifest, WorkerProcessIdentity,
    WorkerShutdownReport,
};

#[derive(Debug)]
pub enum SupervisorError {
    Configuration(ConfigError),
    WorkerJob(Win32Error),
}

#[derive(Debug)]
pub enum WorkerStartError {
    ProfileGate(GateError),
    Manager(WorkerManagerError),
}

#[derive(Debug)]
pub enum CompetitiveEntryError {
    WorkerManager(WorkerManagerError),
}

#[derive(Debug)]
pub enum PreMatchHandoffRequestError {
    Validation(ValidationError),
    ConfirmationRequired,
    Readiness(String),
    Execution(PreMatchHandoffError),
}

impl fmt::Display for PreMatchHandoffRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(formatter, "pre-match action rejected: {error:?}"),
            Self::ConfirmationRequired => {
                write!(
                    formatter,
                    "the fixed pre-match sequence requires explicit confirmation"
                )
            }
            Self::Readiness(error) => {
                write!(
                    formatter,
                    "pre-match launch denied by readiness check: {error}"
                )
            }
            Self::Execution(error) => error.fmt(formatter),
        }
    }
}

impl Error for PreMatchHandoffRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Execution(error) => Some(error),
            Self::Validation(_) | Self::ConfirmationRequired | Self::Readiness(_) => None,
        }
    }
}

impl fmt::Display for CompetitiveEntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkerManager(error) => {
                write!(
                    formatter,
                    "competitive containment could not verify worker shutdown: {error}"
                )
            }
        }
    }
}

impl Error for CompetitiveEntryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WorkerManager(error) => Some(error),
        }
    }
}

impl fmt::Display for WorkerStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileGate(error) => {
                write!(formatter, "worker start denied by profile gate: {error:?}")
            }
            Self::Manager(error) => error.fmt(formatter),
        }
    }
}

impl Error for WorkerStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProfileGate(_) => None,
            Self::Manager(error) => Some(error),
        }
    }
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
    pub transition: TransitionState,
    pub generation: ProfileGeneration,
    pub action_gate_open: bool,
    pub worker_start_gate_open: bool,
    pub active_owned_worker_processes: u32,
    pub configured_internal_worker_count: usize,
    pub faceit_setup_status: FaceitSetupStatus,
}

/// Result of a serialized Competitive Mode entry. No worker shutdown outcome
/// is considered clean until the manager has verified its owned Job Object is
/// empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompetitiveEntryResult {
    pub start: TransitionStart,
    pub clean_shutdown: bool,
    pub used_forced_termination: bool,
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
/// explicit even though the current CLI does not yet run a resident event loop
/// or process monitor. Its only external application capability is the
/// explicit fixed pre-match handoff.
pub struct Supervisor {
    configuration: ValidatedConfiguration,
    controller: ProfileController,
    router: Router,
    registry: ActionRegistry,
    worker_manifest: WorkerManifest,
    worker_manager: WindowsWorkerManager,
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
        let worker_manager = WindowsWorkerManager::create().map_err(SupervisorError::WorkerJob)?;

        Ok(Self {
            configuration,
            controller: ProfileController::new(),
            router,
            registry,
            worker_manifest: WorkerManifest,
            worker_manager,
        })
    }

    pub fn status(&self) -> Result<SupervisorStatus, SupervisorError> {
        Ok(SupervisorStatus {
            profile: self.controller.profile(),
            transition: self.controller.transition(),
            generation: self.controller.generation(),
            action_gate_open: self.controller.action_gate_open(),
            worker_start_gate_open: self.controller.worker_start_gate_open(),
            active_owned_worker_processes: self
                .worker_manager
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

    /// Executes the one fixed pre-match handoff only after the caller collected
    /// fresh user confirmation. The supervisor validates its registered action
    /// once, then the same request generation is admitted again before each
    /// externally visible process start.
    pub fn prepare_faceit_cs2(
        &mut self,
        user_confirmed: bool,
    ) -> Result<PreMatchHandoffReport, PreMatchHandoffRequestError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        let validated = self
            .registry
            .validate(
                ActionPlan::single(Action::StartCompetitiveHandoff),
                request,
                &self.controller,
            )
            .map_err(PreMatchHandoffRequestError::Validation)?;
        if !validated.requires_confirmation() || !user_confirmed {
            return Err(PreMatchHandoffRequestError::ConfirmationRequired);
        }

        ensure_cs2_not_running(&mut WindowsCs2ProcessSource)
            .map_err(|error| PreMatchHandoffRequestError::Readiness(error.to_string()))?;

        run_pre_match_handoff(|| self.controller.authorize_action(request))
            .map_err(PreMatchHandoffRequestError::Execution)
    }

    /// Starts one fixed internal worker only after the current profile gate
    /// authorizes that worker-start request. The worker manager performs the
    /// suspended-create, assign, verify, resume sequence before returning.
    pub fn start_internal_worker(
        &mut self,
        worker: InternalWorker,
        installation_directory: &Path,
    ) -> Result<WorkerProcessIdentity, WorkerStartError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.controller
            .authorize_worker_start(request)
            .map_err(WorkerStartError::ProfileGate)?;
        self.worker_manager
            .start(worker, installation_directory)
            .map_err(WorkerStartError::Manager)
    }

    /// Stops only processes in this supervisor's opaque worker Job Object.
    /// The manager requests the fixed cooperative exit, waits a bounded time,
    /// and verifies Job Object emptiness after any forced fallback.
    pub fn shutdown_internal_workers(
        &mut self,
    ) -> Result<WorkerShutdownReport, WorkerManagerError> {
        self.worker_manager.shutdown()
    }

    /// Enters Competitive Mode by closing both profile gates before the owned
    /// worker Job Object is asked to stop. Any manager failure is recorded as a
    /// restricted fault, so callers cannot restore God Mode after an
    /// unverified containment transition.
    pub fn enter_competitive(
        &mut self,
        cause: TransitionCause,
    ) -> Result<CompetitiveEntryResult, CompetitiveEntryError> {
        let start = self.controller.enter_competitive(cause);
        if start.was_already_restricted {
            return match self.worker_manager.active_process_count() {
                Ok(0) if self.controller.transition() == TransitionState::Stable => {
                    Ok(CompetitiveEntryResult {
                        start,
                        clean_shutdown: true,
                        used_forced_termination: false,
                    })
                }
                Ok(active_processes) => {
                    self.controller
                        .finish_competitive_shutdown(ShutdownVerification::Incomplete {
                            reason: format!(
                                "{active_processes} owned worker process(es) remain during repeated competitive entry"
                            ),
                        });
                    Ok(CompetitiveEntryResult {
                        start,
                        clean_shutdown: false,
                        used_forced_termination: false,
                    })
                }
                Err(error) => {
                    self.controller
                        .finish_competitive_shutdown(ShutdownVerification::Incomplete {
                            reason: error.to_string(),
                        });
                    Err(CompetitiveEntryError::WorkerManager(
                        WorkerManagerError::Job(error),
                    ))
                }
            };
        }

        match self.worker_manager.shutdown() {
            Ok(report) => {
                self.controller
                    .finish_competitive_shutdown(ShutdownVerification::Clean);
                Ok(CompetitiveEntryResult {
                    start,
                    clean_shutdown: true,
                    used_forced_termination: report.used_forced_termination,
                })
            }
            Err(error) => {
                self.controller
                    .finish_competitive_shutdown(ShutdownVerification::Incomplete {
                        reason: error.to_string(),
                    });
                Err(CompetitiveEntryError::WorkerManager(error))
            }
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
        assert_eq!(status.transition, TransitionState::Stable);
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

    #[test]
    fn pre_match_handoff_requires_explicit_confirmation_before_execution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        assert!(matches!(
            supervisor.prepare_faceit_cs2(false),
            Err(PreMatchHandoffRequestError::ConfirmationRequired)
        ));
    }

    #[test]
    fn pre_match_handoff_is_denied_after_competitive_mode_closes_the_gate() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        supervisor
            .controller
            .enter_competitive(TransitionCause::Manual);

        assert!(matches!(
            supervisor.prepare_faceit_cs2(true),
            Err(PreMatchHandoffRequestError::Validation(
                ValidationError::ProfileGate(GateError::ProfileRestricted)
            ))
        ));
    }

    #[test]
    fn competitive_profile_denies_worker_start_before_path_resolution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        supervisor
            .controller
            .enter_competitive(jarvis_profiles::TransitionCause::Manual);

        assert!(matches!(
            supervisor.start_internal_worker(InternalWorker::Session, Path::new("does-not-exist")),
            Err(WorkerStartError::ProfileGate(GateError::ProfileRestricted))
        ));
    }

    #[test]
    fn competitive_entry_closes_gates_after_the_real_worker_manager_verifies_empty() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");

        let result = supervisor
            .enter_competitive(TransitionCause::Manual)
            .expect("empty owned Job Object should verify cleanly");
        assert!(result.clean_shutdown);
        assert!(!result.used_forced_termination);

        let status = supervisor
            .status()
            .expect("query contained supervisor state");
        assert_eq!(status.profile, Profile::Competitive);
        assert_eq!(status.transition, TransitionState::Stable);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
        assert!(matches!(
            supervisor.start_internal_worker(InternalWorker::Session, Path::new("does-not-exist")),
            Err(WorkerStartError::ProfileGate(GateError::ProfileRestricted))
        ));
    }
}
