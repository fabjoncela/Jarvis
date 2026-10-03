//! Serialized supervisor foundation with fixed pre-match and narrow desktop capabilities.
//!
//! `Supervisor` is the sole owner of profile state, configured routing, action
//! validation, the fixed internal-worker manifest, and an owned Job Object.
//! CLI monitor iterations reconcile through this serialized owner.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_actions::{Action, ActionPlan, ActionRegistry, ValidatedPlan, ValidationError};
use jarvis_config::{ConfigError, FaceitSetupStatus, ValidatedConfiguration};
use jarvis_core::{ProfileGeneration, RequestContext, RequestId};
use jarvis_gaming::{ensure_cs2_not_running, TriggerObservation, WindowsCs2ProcessSource};
use jarvis_model_protocol::{
    admit_model_proposal, encode_planning_request, AdmissionError, ModelAdmission,
    ModelReplyReceiver, PendingModelRequest, ReplyMailboxError, RequestError,
};
use jarvis_profiles::{
    GateError, Profile, ProfileController, RestoreError, ShutdownVerification, TransitionCause,
    TransitionStart, TransitionState,
};
use jarvis_router::{RouteResult, Router};
use jarvis_session_context::SessionContext;
use jarvis_storage::{
    CanonicalId, CorrectionTerm, EventId, PreferenceKey, RecordOutcome, ReferenceId, StorageError,
    Store, StoredPreference, StoredPreferenceTombstone,
};
use jarvis_verification::{
    DispatchState, ExecutionAnchor, ExecutionId, FeedbackKind, RecordResult, UserFeedback,
    VerificationCase, VerificationError,
};
use jarvis_windows::{
    observe_fixed_discord_process_count, read_default_master_volume, run_pre_match_handoff,
    set_default_master_volume, verify_fixed_chrome_launcher, verify_fixed_discord_launcher,
    AudioEndpointError, DiscordProcessObservationError, InternalWorker, PreMatchHandoffError,
    PreMatchHandoffReport, PreMatchSetupError, Win32Error, WindowsWorkerManager,
    WorkerManagerError, WorkerManifest, WorkerProcessIdentity, WorkerShutdownReport,
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
pub enum PersistentMemoryError {
    ProfileGateClosed,
    StaleGeneration,
    Storage(StorageError),
}

impl fmt::Display for PersistentMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProfileGateClosed => {
                formatter.write_str("persistent memory is disabled outside stable God Mode")
            }
            Self::StaleGeneration => formatter.write_str("profile generation changed"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl Error for PersistentMemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProfileGateClosed | Self::StaleGeneration => None,
            Self::Storage(error) => Some(error),
        }
    }
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

#[derive(Debug)]
pub enum DiscordLaunchRequestError {
    Validation(ValidationError),
    Authorization(GateError),
    UnexpectedConfirmation,
    Setup(PreMatchSetupError),
    Launch(Win32Error),
}

#[derive(Debug)]
pub enum ChromeLaunchRequestError {
    Validation(ValidationError),
    Authorization(GateError),
    UnexpectedConfirmation,
    Setup(PreMatchSetupError),
    Launch(Win32Error),
}

impl fmt::Display for ChromeLaunchRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(formatter, "Chrome action rejected: {error:?}"),
            Self::Authorization(error) => write!(formatter, "Chrome action denied: {error:?}"),
            Self::UnexpectedConfirmation => {
                formatter.write_str("Chrome action unexpectedly required confirmation")
            }
            Self::Setup(error) => error.fmt(formatter),
            Self::Launch(error) => write!(formatter, "could not start Chrome: {error}"),
        }
    }
}

impl Error for ChromeLaunchRequestError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscordFeedbackError {
    Gate(GateError),
    UnknownExecution,
    Verification(VerificationError),
}

#[derive(Debug)]
pub enum VolumeRequestError {
    Validation(ValidationError),
    Authorization(GateError),
    Audio(AudioEndpointError),
}

impl fmt::Display for VolumeRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(formatter, "volume action rejected: {error:?}"),
            Self::Authorization(error) => write!(formatter, "volume action denied: {error:?}"),
            Self::Audio(error) => error.fmt(formatter),
        }
    }
}

impl Error for VolumeRequestError {}

struct DiscordExecution {
    reference: u128,
    case: VerificationCase,
}

impl fmt::Display for DiscordLaunchRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(error) => write!(formatter, "Discord action rejected: {error:?}"),
            Self::Authorization(error) => write!(formatter, "Discord action denied: {error:?}"),
            Self::UnexpectedConfirmation => {
                formatter.write_str("Discord action was unexpectedly marked confirmation-required")
            }
            Self::Setup(error) => error.fmt(formatter),
            Self::Launch(error) => write!(formatter, "could not start Discord: {error}"),
        }
    }
}

impl Error for DiscordLaunchRequestError {}

#[derive(Debug)]
pub enum DiscordStatusError {
    Gate(GateError),
    Observation(DiscordProcessObservationError),
}

impl fmt::Display for DiscordStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gate(error) => write!(formatter, "Discord status denied: {error:?}"),
            Self::Observation(error) => error.fmt(formatter),
        }
    }
}

impl Error for DiscordStatusError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandoffAttemptState {
    Rejected,
    Stopped,
    Dispatched,
}

/// Process-local dispatch history, not proof that FACEIT, Chrome, or CS2 is ready.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LastHandoffAttempt {
    pub request: RequestContext,
    pub state: HandoffAttemptState,
    pub report: PreMatchHandoffReport,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityReconciliation {
    EnteredCompetitive {
        cause: TransitionCause,
        clean_shutdown: bool,
    },
    RestoredGodMode,
    RemainsRestricted,
    RestoreDenied(RestoreError),
    Unchanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteOutcome {
    Validated(ValidatedPlan),
    InvalidPattern(String),
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelRequestError {
    ProfileGate(GateError),
    KnownDeterministicRoute,
    InvalidDeterministicPattern,
    ProtectedTopicRequiresExplicitWorkflow,
    Protocol(RequestError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ModelResponseError {
    NoPendingRequest,
    Admission(AdmissionError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ModelPollError {
    Mailbox(ReplyMailboxError),
    Response(ModelResponseError),
}

/// The one authoritative mutable owner for a future long-lived Jarvis process.
///
/// All request admission takes `&mut self`, keeping policy and monitor
/// reconciliation serialized. External application capabilities are limited
/// to the explicit fixed pre-match handoff and fixed Discord/Chrome launchers.
pub struct Supervisor {
    configuration: ValidatedConfiguration,
    controller: ProfileController,
    router: Router,
    registry: ActionRegistry,
    worker_manifest: WorkerManifest,
    worker_manager: WindowsWorkerManager,
    healthy_clear_since: Option<Instant>,
    last_handoff_attempt: Option<LastHandoffAttempt>,
    last_discord_execution: Option<DiscordExecution>,
    session_context: SessionContext,
    pending_model_request: Option<PendingModelRequest>,
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
            healthy_clear_since: None,
            last_handoff_attempt: None,
            last_discord_execution: None,
            session_context: SessionContext::new(ProfileGeneration::initial()),
            pending_model_request: None,
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

    pub fn last_handoff_attempt(&self) -> Option<LastHandoffAttempt> {
        self.last_handoff_attempt
    }

    /// Appends an explicit typed preference. Store opening and migration work
    /// happen only after both God Mode gates are open; the per-call connection
    /// is dropped before returning and is never owned by a worker. The caller
    /// must obtain explicit consent before invoking this API.
    pub fn record_explicit_preference(
        &self,
        expected_generation: u64,
        event_id: &EventId,
        key: &PreferenceKey,
        target: &ReferenceId,
    ) -> Result<RecordOutcome, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.record_preference(event_id, key, target)
        })
    }

    pub fn read_explicit_preference(
        &self,
        expected_generation: u64,
        key: &PreferenceKey,
    ) -> Result<Option<StoredPreference>, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.preference(key)
        })
    }

    pub fn read_explicit_preference_tombstone(
        &self,
        expected_generation: u64,
        key: &PreferenceKey,
    ) -> Result<Option<StoredPreferenceTombstone>, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.preference_tombstone(key)
        })
    }

    pub fn forget_explicit_preference(
        &self,
        expected_generation: u64,
        event_id: &EventId,
        key: &PreferenceKey,
        expected_current_event_id: &EventId,
    ) -> Result<RecordOutcome, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.forget_preference(event_id, key, expected_current_event_id)
        })
    }

    pub fn restore_explicit_preference(
        &self,
        expected_generation: u64,
        event_id: &EventId,
        key: &PreferenceKey,
        target: &ReferenceId,
        expected_tombstone_event_id: &EventId,
    ) -> Result<RecordOutcome, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.restore_preference(event_id, key, target, expected_tombstone_event_id)
        })
    }

    /// Appends an explicit correction using bounded identifiers, never a raw
    /// utterance or transcript. There is no user-facing correction command
    /// yet; future trusted callers must obtain explicit consent first.
    #[allow(dead_code)]
    pub fn record_explicit_correction(
        &self,
        expected_generation: u64,
        event_id: &EventId,
        term: &CorrectionTerm,
        canonical_id: &CanonicalId,
    ) -> Result<RecordOutcome, PersistentMemoryError> {
        self.with_persistent_store(expected_generation, Store::open_local_app_data, |store| {
            store.record_correction(event_id, term, canonical_id)
        })
    }

    fn with_persistent_store<T>(
        &self,
        expected_generation: u64,
        open_store: impl FnOnce() -> Result<Store, StorageError>,
        write: impl FnOnce(&mut Store) -> Result<T, StorageError>,
    ) -> Result<T, PersistentMemoryError> {
        if self.controller.profile() != Profile::GodMode
            || self.controller.transition() != TransitionState::Stable
            || !self.controller.action_gate_open()
            || !self.controller.worker_start_gate_open()
        {
            return Err(PersistentMemoryError::ProfileGateClosed);
        }
        if self.controller.generation().value() != expected_generation {
            return Err(PersistentMemoryError::StaleGeneration);
        }

        let mut store = open_store().map_err(PersistentMemoryError::Storage)?;
        write(&mut store).map_err(PersistentMemoryError::Storage)
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

    /// Prepares, but does not send, one bounded request for a future owned
    /// local model worker. Deterministic commands and malformed patterns never
    /// fall through to the model. No utterance is retained by the supervisor.
    #[allow(dead_code)]
    pub fn begin_model_request(&mut self, command: &str) -> Result<Vec<u8>, ModelRequestError> {
        match self.router.route(command) {
            RouteResult::Plan(_) => return Err(ModelRequestError::KnownDeterministicRoute),
            RouteResult::InvalidPattern { .. } => {
                return Err(ModelRequestError::InvalidDeterministicPattern);
            }
            RouteResult::Unknown => {}
        }
        if command
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| {
                matches!(
                    token.to_ascii_lowercase().as_str(),
                    "faceit" | "cs2" | "anticheat"
                )
            })
        {
            return Err(ModelRequestError::ProtectedTopicRequiresExplicitWorkflow);
        }
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.controller
            .authorize_action(request)
            .map_err(ModelRequestError::ProfileGate)?;
        self.controller
            .authorize_worker_start(request)
            .map_err(ModelRequestError::ProfileGate)?;
        let encoded =
            encode_planning_request(command, request).map_err(ModelRequestError::Protocol)?;
        if let Some(previous) = self.pending_model_request.take() {
            previous.cancel();
        }
        self.pending_model_request = Some(PendingModelRequest::new(request, Instant::now()));
        Ok(encoded)
    }

    /// One-shot validation only. No operating-system effect is dispatched.
    #[allow(dead_code)]
    pub fn admit_model_response(
        &mut self,
        response: &[u8],
    ) -> Result<ModelAdmission, ModelResponseError> {
        let pending = self
            .pending_model_request
            .take()
            .ok_or(ModelResponseError::NoPendingRequest)?;
        admit_model_proposal(
            response,
            &pending,
            Instant::now(),
            &self.registry,
            &self.controller,
        )
        .map_err(ModelResponseError::Admission)
    }

    /// Checks at most one bounded queued reply without waiting for model I/O.
    /// A late reply after Competitive entry is rejected by the absent pending
    /// request and can never produce an executable plan.
    #[allow(dead_code)]
    pub fn poll_model_reply(
        &mut self,
        receiver: &ModelReplyReceiver,
    ) -> Result<Option<ModelAdmission>, ModelPollError> {
        let Some(response) = receiver.try_receive().map_err(ModelPollError::Mailbox)? else {
            return Ok(None);
        };
        self.admit_model_response(&response)
            .map(Some)
            .map_err(ModelPollError::Response)
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
        let result = (|| {
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
        })();
        let (state, report) = match &result {
            Ok(report) => (HandoffAttemptState::Dispatched, *report),
            Err(PreMatchHandoffRequestError::Execution(
                PreMatchHandoffError::Authorization { completed, .. }
                | PreMatchHandoffError::Launch { completed, .. },
            )) => (HandoffAttemptState::Stopped, *completed),
            Err(_) => (
                HandoffAttemptState::Rejected,
                PreMatchHandoffReport::empty(),
            ),
        };
        self.last_handoff_attempt = Some(LastHandoffAttempt {
            request,
            state,
            report,
        });
        result
    }

    /// Executes only the configured Discord launch action. The registry and
    /// profile gate are checked before FFI, and the same generation is checked
    /// again immediately before the fixed native launch path.
    pub fn open_discord(&mut self) -> Result<(RequestId, u128), DiscordLaunchRequestError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.authorize_discord_request(request)?;
        let launcher = verify_fixed_discord_launcher().map_err(DiscordLaunchRequestError::Setup)?;
        self.controller
            .authorize_action(request)
            .map_err(DiscordLaunchRequestError::Authorization)?;
        launcher
            .start()
            .map_err(DiscordLaunchRequestError::Launch)?;
        let reference = self.record_discord_dispatch(request);
        Ok((request.request_id, reference))
    }

    /// Launches only the fixed signed Chrome installation without a URL or
    /// caller-provided arguments. This is separate from the pre-match handoff.
    pub fn open_chrome(&mut self) -> Result<RequestId, ChromeLaunchRequestError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.authorize_chrome_request(request)?;
        let launcher = verify_fixed_chrome_launcher().map_err(ChromeLaunchRequestError::Setup)?;
        self.controller
            .authorize_action(request)
            .map_err(ChromeLaunchRequestError::Authorization)?;
        launcher.start().map_err(ChromeLaunchRequestError::Launch)?;
        Ok(request.request_id)
    }

    fn record_discord_dispatch(&mut self, request: RequestContext) -> u128 {
        let now = Instant::now();
        let execution = ExecutionId::next();
        // This is a correlation reference, not an authorization token. The
        // execution counter prevents reuse within one owner, and the timestamp
        // makes accidental reuse after a monitor restart unlikely.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let reference = (u128::from(execution.value()) << 64)
            | (nanos as u64 ^ u64::from(std::process::id())) as u128;
        let anchor = ExecutionAnchor::new(request, execution);
        let mut case = VerificationCase::new(anchor, now, Duration::from_secs(5 * 60))
            .expect("fixed nonzero verification window");
        case.record_dispatch(anchor, DispatchState::Accepted, now, request.generation)
            .expect("fresh execution dispatch");
        self.session_context
            .record_execution(anchor, None, now, request.generation)
            .expect("serialized fresh Discord dispatch");
        self.last_discord_execution = Some(DiscordExecution { reference, case });
        reference
    }

    /// Attaches one explicit report to the exact latest Discord dispatch.
    /// A report is not trusted machine evidence and cannot verify success.
    pub fn record_discord_feedback(
        &mut self,
        reference: u128,
        kind: FeedbackKind,
    ) -> Result<RecordResult, DiscordFeedbackError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.controller
            .authorize_action(request)
            .map_err(DiscordFeedbackError::Gate)?;
        let execution = self
            .last_discord_execution
            .as_mut()
            .filter(|execution| execution.reference == reference)
            .ok_or(DiscordFeedbackError::UnknownExecution)?;
        let anchor = execution.case.anchor();
        execution
            .case
            .record_feedback(
                UserFeedback {
                    id: 1,
                    anchor,
                    kind,
                },
                Instant::now(),
                self.controller.generation(),
            )
            .map_err(DiscordFeedbackError::Verification)
    }

    /// Resolve a recent unambiguous Discord dispatch for a shorthand user
    /// report. Session context only selects an anchor; it grants no action or
    /// machine-verification authority.
    pub fn record_latest_discord_feedback(
        &mut self,
        kind: FeedbackKind,
    ) -> Result<RecordResult, DiscordFeedbackError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.controller
            .authorize_action(request)
            .map_err(DiscordFeedbackError::Gate)?;
        let anchor = self
            .session_context
            .resolve_feedback_anchor(Instant::now(), self.controller.generation())
            .ok_or(DiscordFeedbackError::UnknownExecution)?;
        let reference = self
            .last_discord_execution
            .as_ref()
            .filter(|execution| execution.case.anchor() == anchor)
            .map(|execution| execution.reference)
            .ok_or(DiscordFeedbackError::UnknownExecution)?;
        self.record_discord_feedback(reference, kind)
    }

    /// Read-only current-process observation in God Mode. Presence is not
    /// attributed to a preceding launch request or treated as UI readiness.
    pub fn discord_process_count(&self) -> Result<u32, DiscordStatusError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.authorize_discord_observation(request)?;
        observe_fixed_discord_process_count().map_err(DiscordStatusError::Observation)
    }

    /// Executes one bounded typed volume action against the default console
    /// output endpoint. The native adapter cannot select another target.
    pub fn set_master_volume(
        &mut self,
        percent: u8,
    ) -> Result<(RequestId, f32), VolumeRequestError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.authorize_volume_request(request, percent)?;
        let observed = set_default_master_volume(percent).map_err(VolumeRequestError::Audio)?;
        Ok((request.request_id, observed))
    }

    /// On-demand read-only observation; the endpoint identity is fixed by
    /// Windows for the current user, never selected by IPC payload.
    pub fn master_volume_status(&self) -> Result<f32, VolumeRequestError> {
        let request = RequestContext::new(RequestId::next(), self.controller.generation());
        self.controller
            .authorize_action(request)
            .map_err(VolumeRequestError::Authorization)?;
        read_default_master_volume().map_err(VolumeRequestError::Audio)
    }

    fn authorize_volume_request(
        &self,
        request: RequestContext,
        percent: u8,
    ) -> Result<(), VolumeRequestError> {
        let validated = self
            .registry
            .validate(
                ActionPlan::single(Action::SetMasterVolume { percent }),
                request,
                &self.controller,
            )
            .map_err(VolumeRequestError::Validation)?;
        debug_assert!(!validated.requires_confirmation());
        self.controller
            .authorize_action(request)
            .map_err(VolumeRequestError::Authorization)
    }

    fn authorize_discord_observation(
        &self,
        request: RequestContext,
    ) -> Result<(), DiscordStatusError> {
        self.controller
            .authorize_action(request)
            .map_err(DiscordStatusError::Gate)
    }

    fn authorize_discord_request(
        &self,
        request: RequestContext,
    ) -> Result<(), DiscordLaunchRequestError> {
        let validated = self
            .registry
            .validate(
                ActionPlan::single(Action::LaunchApplication {
                    application_id: "discord".to_owned(),
                }),
                request,
                &self.controller,
            )
            .map_err(DiscordLaunchRequestError::Validation)?;
        if validated.requires_confirmation() {
            return Err(DiscordLaunchRequestError::UnexpectedConfirmation);
        }
        self.controller
            .authorize_action(request)
            .map_err(DiscordLaunchRequestError::Authorization)?;
        Ok(())
    }

    fn authorize_chrome_request(
        &self,
        request: RequestContext,
    ) -> Result<(), ChromeLaunchRequestError> {
        let validated = self
            .registry
            .validate(
                ActionPlan::single(Action::LaunchApplication {
                    application_id: "chrome".to_owned(),
                }),
                request,
                &self.controller,
            )
            .map_err(ChromeLaunchRequestError::Validation)?;
        if validated.requires_confirmation() {
            return Err(ChromeLaunchRequestError::UnexpectedConfirmation);
        }
        self.controller
            .authorize_action(request)
            .map_err(ChromeLaunchRequestError::Authorization)
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
        self.healthy_clear_since = None;
        let start = self.controller.enter_competitive(cause);
        if let Some(pending) = self.pending_model_request.take() {
            pending.cancel();
        }
        self.session_context.clear();
        if let Some(execution) = self.last_discord_execution.as_mut() {
            execution
                .case
                .cancel_for_generation_change(self.controller.generation());
        }
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

    /// Applies a startup or polling snapshot to the supervisor-owned profile.
    /// Any active trigger or unhealthy observation closes both gates before
    /// workers are asked to stop. God Mode is restored only after a healthy
    /// snapshot reports that every fixed trigger has cleared.
    pub fn reconcile_activity(
        &mut self,
        observation: TriggerObservation,
    ) -> Result<ActivityReconciliation, CompetitiveEntryError> {
        self.reconcile_activity_at(observation, Instant::now())
    }

    pub(crate) fn reconcile_activity_at(
        &mut self,
        observation: TriggerObservation,
        now: Instant,
    ) -> Result<ActivityReconciliation, CompetitiveEntryError> {
        let snapshot = observation.snapshot;
        let cause = snapshot.first_cause().or_else(|| {
            (!snapshot.detection_healthy).then_some(TransitionCause::DetectionUnhealthy)
        });

        if let Some(cause) = cause {
            self.healthy_clear_since = None;
            if self.controller.profile() == Profile::Competitive {
                return Ok(ActivityReconciliation::RemainsRestricted);
            }
            let entry = self.enter_competitive(cause)?;
            return Ok(ActivityReconciliation::EnteredCompetitive {
                cause,
                clean_shutdown: entry.clean_shutdown,
            });
        }

        if self.controller.profile() == Profile::Competitive {
            if self.controller.transition() == TransitionState::RestrictedFault {
                self.healthy_clear_since = None;
                return Ok(ActivityReconciliation::RestoreDenied(
                    RestoreError::RestrictedFault,
                ));
            }

            let clear_since = *self.healthy_clear_since.get_or_insert(now);
            let cooldown =
                Duration::from_secs(u64::from(self.configuration.exit_cooldown_seconds()));
            if now.saturating_duration_since(clear_since) < cooldown {
                return Ok(ActivityReconciliation::RemainsRestricted);
            }

            return Ok(
                match self.controller.restore_god_mode(
                    snapshot.any_automatic_trigger_active(),
                    snapshot.detection_healthy,
                ) {
                    Ok(()) => ActivityReconciliation::RestoredGodMode,
                    Err(error) => ActivityReconciliation::RestoreDenied(error),
                },
            );
        }

        self.healthy_clear_since = None;
        Ok(ActivityReconciliation::Unchanged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_gaming::{DetectionHealth, TriggerObservation, TriggerSnapshot};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STORAGE_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    struct StorageTestRoot(std::path::PathBuf);

    impl StorageTestRoot {
        fn new() -> Self {
            loop {
                let sequence = NEXT_STORAGE_TEST_ROOT.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "jarvis-supervisor-storage-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create isolated storage test directory: {error}"),
                }
            }
        }
    }

    impl Drop for StorageTestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_config_directory() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
    }

    fn observation(cs2_running: bool, detection_healthy: bool) -> TriggerObservation {
        TriggerObservation {
            snapshot: TriggerSnapshot {
                cs2_running,
                detection_healthy,
                ..TriggerSnapshot::default()
            },
            health: if detection_healthy {
                DetectionHealth::Healthy
            } else {
                DetectionHealth::ActivityObservationUnavailable
            },
        }
    }

    #[test]
    fn supervisor_owns_an_empty_job_and_fixed_worker_manifest() {
        let supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let status = supervisor.status().expect("query empty owned Job Object");
        assert_eq!(status.profile, Profile::GodMode);
        assert_eq!(status.transition, TransitionState::Stable);
        assert_eq!(status.active_owned_worker_processes, 0);
        assert_eq!(status.configured_internal_worker_count, 2);
    }

    #[test]
    fn trusted_explicit_writer_persists_typed_preference_in_god_mode() {
        let supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let root = StorageTestRoot::new();
        let event_id = EventId::new("request-1").unwrap();
        let key = PreferenceKey::new("preferred_browser").unwrap();
        let target = ReferenceId::new("app:chrome").unwrap();

        let result = supervisor
            .with_persistent_store(
                supervisor.controller.generation().value(),
                || Store::open_in_test_root(&root.0),
                |store| store.record_preference(&event_id, &key, &target),
            )
            .expect("stable God Mode permits explicit preference writes");
        assert_eq!(result, RecordOutcome::Inserted);

        // This read happens only while the fixture remains in God Mode.
        let store = Store::open_in_test_root(&root.0).unwrap();
        let stored = store.preference(&key).unwrap().unwrap();
        assert_eq!(stored.event_id, event_id);
        assert_eq!(stored.target, target);
    }

    #[test]
    fn competitive_gate_denies_before_opening_or_accessing_storage() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        supervisor
            .reconcile_activity(observation(true, true))
            .expect("CS2 activity should enter Competitive Mode");
        let root = StorageTestRoot::new();
        let event_id = EventId::new("request-2").unwrap();
        let key = PreferenceKey::new("preferred_browser").unwrap();
        let target = ReferenceId::new("app:chrome").unwrap();
        let mut open_attempted = false;

        let result = supervisor.with_persistent_store(
            supervisor.controller.generation().value(),
            || {
                open_attempted = true;
                Store::open_in_test_root(&root.0)
            },
            |store| store.record_preference(&event_id, &key, &target),
        );
        assert!(matches!(
            result,
            Err(PersistentMemoryError::ProfileGateClosed)
        ));
        assert!(!open_attempted);
        assert!(!root.0.join("jarvis.db").exists());
    }

    #[test]
    fn stale_generation_denies_before_opening_storage() {
        let supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let root = StorageTestRoot::new();
        let mut opened = false;
        let result: Result<(), PersistentMemoryError> = supervisor.with_persistent_store(
            supervisor.controller.generation().value() + 1,
            || {
                opened = true;
                Store::open_in_test_root(&root.0)
            },
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(PersistentMemoryError::StaleGeneration)
        ));
        assert!(!opened);
        assert!(!root.0.join("jarvis.db").exists());
    }

    #[test]
    fn preference_from_before_competitive_entry_cannot_replay_after_restoration() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let prior_generation = supervisor.controller.generation().value();
        let start = Instant::now();
        supervisor
            .reconcile_activity_at(observation(true, true), start)
            .expect("trigger should close the gates");
        assert_eq!(supervisor.controller.profile(), Profile::Competitive);
        supervisor
            .reconcile_activity_at(observation(false, true), start + Duration::from_secs(1))
            .expect("healthy clear begins the cooldown");
        supervisor
            .reconcile_activity_at(observation(false, true), start + Duration::from_secs(10))
            .expect("healthy clear after cooldown restores God Mode");
        assert_eq!(supervisor.controller.profile(), Profile::GodMode);
        assert!(supervisor.controller.generation().value() > prior_generation);

        let root = StorageTestRoot::new();
        let mut opened = false;
        let result: Result<(), PersistentMemoryError> = supervisor.with_persistent_store(
            prior_generation,
            || {
                opened = true;
                Store::open_in_test_root(&root.0)
            },
            |_| Ok(()),
        );
        assert!(matches!(
            result,
            Err(PersistentMemoryError::StaleGeneration)
        ));
        assert!(!opened);
        assert!(!root.0.join("jarvis.db").exists());
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
    fn model_fallback_is_only_prepared_for_unknown_text_and_is_one_shot() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        assert_eq!(
            supervisor.begin_model_request("open discord"),
            Err(ModelRequestError::KnownDeterministicRoute)
        );
        assert_eq!(
            supervisor.begin_model_request("set volume to 101"),
            Err(ModelRequestError::InvalidDeterministicPattern)
        );
        for protected in ["open faceit", "visit faceit.com", "start CS2 again"] {
            assert_eq!(
                supervisor.begin_model_request(protected),
                Err(ModelRequestError::ProtectedTopicRequiresExplicitWorkflow)
            );
        }
        let encoded = supervisor.begin_model_request("what can you do?").unwrap();
        let request: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(request["utterance"], "what can you do?");
        let reply = serde_json::json!({
            "schema_version": 1,
            "request_id": request["request_id"],
            "profile_generation": request["profile_generation"],
            "action": {"type": "no_action"}
        });
        let reply = serde_json::to_vec(&reply).unwrap();
        assert_eq!(
            supervisor.admit_model_response(&reply),
            Ok(ModelAdmission::NoAction)
        );
        assert_eq!(
            supervisor.admit_model_response(&reply),
            Err(ModelResponseError::NoPendingRequest)
        );
    }

    #[test]
    fn competitive_entry_cancels_pending_model_response_without_loading_worker() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let encoded = supervisor.begin_model_request("unknown command").unwrap();
        let request: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let reply = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "request_id": request["request_id"],
            "profile_generation": request["profile_generation"],
            "action": {"type": "launch_application", "application_id": "discord"}
        }))
        .unwrap();
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        assert_eq!(
            supervisor.admit_model_response(&reply),
            Err(ModelResponseError::NoPendingRequest)
        );
        assert!(matches!(
            supervisor.begin_model_request("another unknown command"),
            Err(ModelRequestError::ProfileGate(_))
        ));
        assert_eq!(
            supervisor.status().unwrap().active_owned_worker_processes,
            0
        );
    }

    #[test]
    fn mailbox_poll_is_nonblocking_and_rejects_reply_after_competitive_entry() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let (sender, receiver) = jarvis_model_protocol::model_reply_mailbox();
        assert_eq!(supervisor.poll_model_reply(&receiver), Ok(None));
        let encoded = supervisor.begin_model_request("unknown request").unwrap();
        let request: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        let reply = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "request_id": request["request_id"],
            "profile_generation": request["profile_generation"],
            "action": {"type": "no_action"}
        }))
        .unwrap();
        sender.try_submit(&reply).unwrap();
        assert_eq!(
            supervisor.poll_model_reply(&receiver),
            Ok(Some(ModelAdmission::NoAction))
        );
        assert_eq!(supervisor.poll_model_reply(&receiver), Ok(None));

        supervisor
            .begin_model_request("another unknown request")
            .unwrap();
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        sender.try_submit(&reply).unwrap();
        assert_eq!(
            supervisor.poll_model_reply(&receiver),
            Err(ModelPollError::Response(
                ModelResponseError::NoPendingRequest
            ))
        );
    }

    #[test]
    fn pre_match_handoff_requires_explicit_confirmation_before_execution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        assert!(matches!(
            supervisor.prepare_faceit_cs2(false),
            Err(PreMatchHandoffRequestError::ConfirmationRequired)
        ));
        let attempt = supervisor.last_handoff_attempt().expect("attempt recorded");
        assert_eq!(attempt.state, HandoffAttemptState::Rejected);
        assert_eq!(attempt.report, PreMatchHandoffReport::empty());
        assert_eq!(attempt.request.generation, ProfileGeneration::initial());
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
        let attempt = supervisor.last_handoff_attempt().expect("denial recorded");
        assert_eq!(attempt.state, HandoffAttemptState::Rejected);
        assert_eq!(attempt.report, PreMatchHandoffReport::empty());
    }

    #[test]
    fn discord_launch_is_denied_in_competitive_mode_before_native_setup() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        supervisor
            .controller
            .enter_competitive(TransitionCause::Manual);
        assert!(matches!(
            supervisor.open_discord(),
            Err(DiscordLaunchRequestError::Validation(
                ValidationError::ProfileGate(GateError::ProfileRestricted)
            ))
        ));
    }

    #[test]
    fn chrome_launch_is_denied_before_native_setup_and_stale_replay() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let old = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        assert!(supervisor.authorize_chrome_request(old).is_ok());
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        assert!(matches!(
            supervisor.open_chrome(),
            Err(ChromeLaunchRequestError::Validation(
                ValidationError::ProfileGate(GateError::ProfileRestricted)
            ))
        ));
        assert!(matches!(
            supervisor.authorize_chrome_request(old),
            Err(ChromeLaunchRequestError::Validation(
                ValidationError::ProfileGate(GateError::StaleGeneration)
            ))
        ));
    }

    #[test]
    fn discord_launch_request_cannot_replay_after_profile_generation_change() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let old = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        assert!(supervisor.authorize_discord_request(old).is_ok());
        supervisor
            .controller
            .enter_competitive(TransitionCause::Cs2Process);
        supervisor
            .controller
            .finish_competitive_shutdown(ShutdownVerification::Clean);
        supervisor.controller.restore_god_mode(false, true).unwrap();
        assert!(matches!(
            supervisor.authorize_discord_request(old),
            Err(DiscordLaunchRequestError::Validation(
                ValidationError::ProfileGate(GateError::StaleGeneration)
            ))
        ));
    }

    #[test]
    fn discord_feedback_is_exact_deduplicated_and_not_machine_verification() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let request = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        let reference = supervisor.record_discord_dispatch(request);
        assert_eq!(
            supervisor.record_discord_feedback(reference ^ 1, FeedbackKind::Worked),
            Err(DiscordFeedbackError::UnknownExecution)
        );
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Worked),
            Ok(RecordResult::Recorded)
        );
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Worked),
            Ok(RecordResult::AlreadyRecorded)
        );
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Failed),
            Err(DiscordFeedbackError::Verification(
                VerificationError::ConflictingDuplicate
            ))
        );
        assert_eq!(
            supervisor
                .last_discord_execution
                .as_ref()
                .unwrap()
                .case
                .machine_outcome(),
            jarvis_verification::MachineOutcome::Pending
        );
        let next_request =
            RequestContext::new(RequestId::next(), supervisor.controller.generation());
        let next_reference = supervisor.record_discord_dispatch(next_request);
        assert_ne!(reference, next_reference);
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Worked),
            Err(DiscordFeedbackError::UnknownExecution)
        );
    }

    #[test]
    fn latest_discord_feedback_requires_one_unambiguous_recent_execution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        assert_eq!(
            supervisor.record_latest_discord_feedback(FeedbackKind::Worked),
            Err(DiscordFeedbackError::UnknownExecution)
        );
        let first = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        supervisor.record_discord_dispatch(first);
        assert_eq!(
            supervisor.record_latest_discord_feedback(FeedbackKind::Worked),
            Ok(RecordResult::Recorded)
        );
        let second = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        supervisor.record_discord_dispatch(second);
        assert_eq!(
            supervisor.record_latest_discord_feedback(FeedbackKind::Worked),
            Err(DiscordFeedbackError::UnknownExecution)
        );
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        assert!(supervisor.session_context.is_empty());
        assert_eq!(
            supervisor.record_latest_discord_feedback(FeedbackKind::Worked),
            Err(DiscordFeedbackError::Gate(GateError::ProfileRestricted))
        );
    }

    #[test]
    fn discord_feedback_is_invalidated_by_competitive_entry() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let request = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        let reference = supervisor.record_discord_dispatch(request);
        supervisor
            .enter_competitive(TransitionCause::Cs2Process)
            .unwrap();
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Worked),
            Err(DiscordFeedbackError::Gate(GateError::ProfileRestricted))
        );
        assert_eq!(
            supervisor
                .last_discord_execution
                .as_ref()
                .unwrap()
                .case
                .machine_outcome(),
            jarvis_verification::MachineOutcome::Canceled
        );
        supervisor.controller.restore_god_mode(false, true).unwrap();
        assert_eq!(
            supervisor.record_discord_feedback(reference, FeedbackKind::Worked),
            Err(DiscordFeedbackError::Verification(
                VerificationError::StaleGeneration
            ))
        );
    }

    #[test]
    fn discord_status_is_denied_in_competitive_mode_before_native_observation() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        supervisor
            .controller
            .enter_competitive(TransitionCause::Manual);
        assert!(matches!(
            supervisor.discord_process_count(),
            Err(DiscordStatusError::Gate(GateError::ProfileRestricted))
        ));
    }

    #[test]
    fn volume_action_is_bounded_and_denied_across_profile_generation() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let old = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        assert!(supervisor.authorize_volume_request(old, 50).is_ok());
        assert!(matches!(
            supervisor.authorize_volume_request(old, 101),
            Err(VolumeRequestError::Validation(
                ValidationError::InvalidVolume(101)
            ))
        ));
        supervisor
            .controller
            .enter_competitive(TransitionCause::Cs2Process);
        assert!(matches!(
            supervisor.authorize_volume_request(old, 50),
            Err(VolumeRequestError::Validation(
                ValidationError::ProfileGate(GateError::StaleGeneration)
            ))
        ));
        let current = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        assert!(matches!(
            supervisor.authorize_volume_request(current, 50),
            Err(VolumeRequestError::Validation(
                ValidationError::ProfileGate(GateError::ProfileRestricted)
            ))
        ));
        assert!(matches!(
            supervisor.master_volume_status(),
            Err(VolumeRequestError::Authorization(
                GateError::ProfileRestricted
            ))
        ));
    }

    #[test]
    fn discord_status_rejects_a_stale_generation_after_restore() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory()).unwrap();
        let old = RequestContext::new(RequestId::next(), supervisor.controller.generation());
        assert!(supervisor.authorize_discord_observation(old).is_ok());
        supervisor
            .controller
            .enter_competitive(TransitionCause::Cs2Process);
        supervisor
            .controller
            .finish_competitive_shutdown(ShutdownVerification::Clean);
        supervisor.controller.restore_god_mode(false, true).unwrap();
        assert!(matches!(
            supervisor.authorize_discord_observation(old),
            Err(DiscordStatusError::Gate(GateError::StaleGeneration))
        ));
    }

    #[test]
    fn competitive_profile_denies_worker_start_before_path_resolution() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        supervisor
            .controller
            .enter_competitive(jarvis_profiles::TransitionCause::Manual);

        for worker in [InternalWorker::Session, InternalWorker::Model] {
            assert!(matches!(
                supervisor.start_internal_worker(worker, Path::new("does-not-exist")),
                Err(WorkerStartError::ProfileGate(GateError::ProfileRestricted))
            ));
        }
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
        for worker in [InternalWorker::Session, InternalWorker::Model] {
            assert!(matches!(
                supervisor.start_internal_worker(worker, Path::new("does-not-exist")),
                Err(WorkerStartError::ProfileGate(GateError::ProfileRestricted))
            ));
        }
    }

    #[test]
    fn startup_reconciliation_enters_competitive_for_verified_faceit_service() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let observation = TriggerObservation {
            snapshot: jarvis_gaming::TriggerSnapshot {
                verified_faceit_service_active: true,
                detection_healthy: true,
                ..jarvis_gaming::TriggerSnapshot::default()
            },
            health: jarvis_gaming::DetectionHealth::Healthy,
        };

        assert_eq!(
            supervisor
                .reconcile_activity(observation)
                .expect("empty worker job should shut down cleanly"),
            ActivityReconciliation::EnteredCompetitive {
                cause: TransitionCause::FaceitService,
                clean_shutdown: true,
            }
        );
        let status = supervisor.status().expect("query reconciled profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
    }

    #[test]
    fn startup_reconciliation_enters_competitive_when_detection_is_unhealthy() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let observation = TriggerObservation {
            snapshot: jarvis_gaming::TriggerSnapshot::default(),
            health: jarvis_gaming::DetectionHealth::FaceitSetupUnverified,
        };

        assert_eq!(
            supervisor
                .reconcile_activity(observation)
                .expect("empty worker job should shut down cleanly"),
            ActivityReconciliation::EnteredCompetitive {
                cause: TransitionCause::DetectionUnhealthy,
                clean_shutdown: true,
            }
        );
        let status = supervisor.status().expect("query reconciled profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
    }

    #[test]
    fn healthy_reconciliation_restores_only_after_all_triggers_clear() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let start = Instant::now();
        let active = TriggerObservation {
            snapshot: jarvis_gaming::TriggerSnapshot {
                cs2_running: true,
                detection_healthy: true,
                ..jarvis_gaming::TriggerSnapshot::default()
            },
            health: jarvis_gaming::DetectionHealth::Healthy,
        };
        let clear = TriggerObservation {
            snapshot: jarvis_gaming::TriggerSnapshot {
                detection_healthy: true,
                ..jarvis_gaming::TriggerSnapshot::default()
            },
            health: jarvis_gaming::DetectionHealth::Healthy,
        };

        supervisor
            .reconcile_activity_at(active, start)
            .expect("active CS2 must enter Competitive Mode");
        assert_eq!(
            supervisor
                .reconcile_activity_at(clear, start)
                .expect("healthy-clear snapshot should start the cooldown"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(clear, start + Duration::from_secs(5))
                .expect("healthy clear should restore at exact expiry"),
            ActivityReconciliation::RestoredGodMode,
            "a continuously healthy, clear snapshot must persist for the configured cooldown"
        );
        let status = supervisor.status().expect("query restored profile");
        assert_eq!(status.profile, Profile::GodMode);
        assert!(status.action_gate_open);
        assert!(status.worker_start_gate_open);
    }

    #[test]
    fn configured_five_second_cooldown_keeps_gates_closed_until_exact_expiry() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        assert_eq!(supervisor.configuration.exit_cooldown_seconds(), 5);

        let start = Instant::now();
        assert!(matches!(
            supervisor
                .reconcile_activity_at(observation(true, true), start)
                .expect("CS2 should enter Competitive Mode"),
            ActivityReconciliation::EnteredCompetitive { .. }
        ));
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start)
                .expect("first healthy-clear snapshot starts the cooldown"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(
                    observation(false, true),
                    start + Duration::from_millis(4_999),
                )
                .expect("profile stays restricted before five seconds"),
            ActivityReconciliation::RemainsRestricted
        );
        let status = supervisor.status().expect("query restricted profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);

        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(5),)
                .expect("exact five-second expiry permits restoration"),
            ActivityReconciliation::RestoredGodMode
        );
    }

    #[test]
    fn retrigger_restarts_the_healthy_clear_cooldown() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let start = Instant::now();

        supervisor
            .reconcile_activity_at(observation(true, true), start)
            .expect("CS2 should enter Competitive Mode");
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start)
                .expect("healthy-clear snapshot starts the cooldown"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(true, true), start + Duration::from_secs(3),)
                .expect("re-trigger must keep Competitive Mode active"),
            ActivityReconciliation::RemainsRestricted
        );

        let clear_again = start + Duration::from_secs(4);
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), clear_again)
                .expect("new healthy-clear interval starts a fresh cooldown"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(
                    observation(false, true),
                    start + Duration::from_millis(8_999),
                )
                .expect("old cooldown expiry must not restore early"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(9),)
                .expect("fresh five-second cooldown has now elapsed"),
            ActivityReconciliation::RestoredGodMode
        );
    }

    #[test]
    fn unhealthy_snapshot_resets_the_healthy_clear_cooldown() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let start = Instant::now();

        supervisor
            .reconcile_activity_at(observation(true, true), start)
            .expect("CS2 should enter Competitive Mode");
        supervisor
            .reconcile_activity_at(observation(false, true), start)
            .expect("healthy-clear snapshot starts the cooldown");
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, false), start + Duration::from_secs(3),)
                .expect("unhealthy detection resets the cooldown"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(4),)
                .expect("healthy recovery begins a new clear interval"),
            ActivityReconciliation::RemainsRestricted
        );
        assert_eq!(
            supervisor
                .reconcile_activity_at(
                    observation(false, true),
                    start + Duration::from_millis(8_999),
                )
                .expect("old cooldown expiry must not restore early"),
            ActivityReconciliation::RemainsRestricted
        );
        let status = supervisor.status().expect("query restricted profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
        assert_eq!(
            supervisor
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(9),)
                .expect("new five-second healthy-clear interval has elapsed"),
            ActivityReconciliation::RestoredGodMode
        );
    }

    #[test]
    fn cooldown_does_not_bypass_manual_latch_or_restricted_fault() {
        let mut manual = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let start = Instant::now();
        manual
            .enter_competitive(TransitionCause::Manual)
            .expect("manual entry should close the gates cleanly");
        manual
            .reconcile_activity_at(observation(false, true), start)
            .expect("healthy-clear snapshot starts the cooldown");
        assert_eq!(
            manual
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(5),)
                .expect("manual latch still requires explicit release"),
            ActivityReconciliation::RestoreDenied(RestoreError::ManualLatchActive)
        );
        let status = manual.status().expect("query manual restricted profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);

        let mut faulted = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        faulted
            .enter_competitive(TransitionCause::Cs2Process)
            .expect("competitive entry should close the gates");
        faulted
            .controller
            .finish_competitive_shutdown(ShutdownVerification::Incomplete {
                reason: "test fault".to_owned(),
            });
        assert_eq!(
            faulted
                .reconcile_activity_at(observation(false, true), start)
                .expect("restricted fault must never restore"),
            ActivityReconciliation::RestoreDenied(RestoreError::RestrictedFault)
        );
        assert_eq!(
            faulted
                .reconcile_activity_at(observation(false, true), start + Duration::from_secs(30),)
                .expect("elapsed cooldown cannot release a restricted fault"),
            ActivityReconciliation::RestoreDenied(RestoreError::RestrictedFault)
        );
        let status = faulted.status().expect("query faulted restricted profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert_eq!(status.transition, TransitionState::RestrictedFault);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
    }

    #[test]
    fn unhealthy_reconciliation_never_restores_competitive_mode() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        supervisor
            .enter_competitive(TransitionCause::DetectionUnhealthy)
            .expect("empty worker job should shut down cleanly");
        let unhealthy_clear = TriggerObservation {
            snapshot: jarvis_gaming::TriggerSnapshot::default(),
            health: jarvis_gaming::DetectionHealth::ActivityObservationUnavailable,
        };

        assert_eq!(
            supervisor
                .reconcile_activity(unhealthy_clear)
                .expect("unhealthy observation remains restricted"),
            ActivityReconciliation::RemainsRestricted
        );
        let status = supervisor.status().expect("query restricted profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
    }
}
