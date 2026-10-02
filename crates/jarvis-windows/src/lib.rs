#![forbid(unsafe_code)]

//! Windows integration boundary.
//!
//! The in-memory lifecycle adapter supports deterministic policy tests. The
//! `WindowsWorkerManager` owns an opaque Job Object and can start only a
//! fixed-manifest internal worker through the suspended-create, assign, verify,
//! resume sequence. The only user-application launch path is the fixed
//! confirmed pre-match handoff; it cannot adopt a PID or launch arbitrary
//! executables.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use jarvis_core::ProfileGeneration;
use jarvis_profiles::GateError;
use jarvis_windows_ffi::{
    launch_verified_pre_match_step, FixedInternalWorker, InternalWorkerIdentity, KillOnCloseJob,
    PreMatchLaunchOutcome, RunningInternalWorker,
};
pub use jarvis_windows_ffi::{
    observe_competitive_activity, observe_cs2_processes, send_supervisor_control_request,
    verify_default_pre_match_setup, CompetitiveActivityObservationError,
    CompetitiveActivitySnapshot, CompetitiveLifecycleNotifications, FixedCompetitiveProcess,
    LifecycleNotificationFailure, ObservedProcessIdentity, PreMatchLaunchError, PreMatchLaunchStep,
    PreMatchSetupError, PreferredApp, ProcessObservationError, SupervisorControlCommand,
    SupervisorControlPipeError, SupervisorControlPipeServer, VerifiedPreMatchSetup, Win32Error,
    WorkerLaunchError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreMatchHandoffReport {
    pub faceit_ac_launcher_started: bool,
    pub chrome_faceit_url_requested: bool,
    pub chrome_faceit_title_match_detected: bool,
    pub steam_cs2_launch_requested: bool,
}

impl PreMatchHandoffReport {
    const fn empty() -> Self {
        Self {
            faceit_ac_launcher_started: false,
            chrome_faceit_url_requested: false,
            chrome_faceit_title_match_detected: false,
            steam_cs2_launch_requested: false,
        }
    }

    fn record(&mut self, step: PreMatchLaunchStep, outcome: PreMatchLaunchOutcome) {
        match step {
            PreMatchLaunchStep::FaceitAcLauncher => self.faceit_ac_launcher_started = true,
            PreMatchLaunchStep::ChromeFaceitWebsite => match outcome {
                PreMatchLaunchOutcome::Started => self.chrome_faceit_url_requested = true,
                PreMatchLaunchOutcome::FaceitTitleMatchDetected => {
                    self.chrome_faceit_title_match_detected = true;
                }
            },
            PreMatchLaunchStep::SteamCs2 => self.steam_cs2_launch_requested = true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreMatchHandoffError {
    Authorization {
        step: PreMatchLaunchStep,
        completed: PreMatchHandoffReport,
        source: GateError,
    },
    Launch {
        step: PreMatchLaunchStep,
        completed: PreMatchHandoffReport,
        source: String,
    },
}

impl fmt::Display for PreMatchHandoffError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authorization {
                step,
                completed,
                source,
            } => write!(
                formatter,
                "pre-match handoff denied before {step:?}; completed steps: {completed:?}; {source:?}"
            ),
            Self::Launch {
                step,
                completed,
                source,
            } => write!(
                formatter,
                "pre-match handoff stopped at {step:?}; completed steps: {completed:?}; {source}"
            ),
        }
    }
}

impl Error for PreMatchHandoffError {}

/// Runs only the fixed Anti-Cheat → Chrome website → Steam app 730 sequence.
/// The supervisor reauthorizes the same request generation before every
/// external process start; each FFI launch repeats the local identity check.
pub fn run_pre_match_handoff(
    authorize_step: impl FnMut() -> Result<(), GateError>,
) -> Result<PreMatchHandoffReport, PreMatchHandoffError> {
    run_pre_match_sequence(authorize_step, |step| {
        launch_verified_pre_match_step(step).map_err(|error| error.to_string())
    })
}

fn run_pre_match_sequence(
    mut authorize_step: impl FnMut() -> Result<(), GateError>,
    mut launch_step: impl FnMut(PreMatchLaunchStep) -> Result<PreMatchLaunchOutcome, String>,
) -> Result<PreMatchHandoffReport, PreMatchHandoffError> {
    let mut report = PreMatchHandoffReport::empty();
    for step in [
        PreMatchLaunchStep::FaceitAcLauncher,
        PreMatchLaunchStep::ChromeFaceitWebsite,
        PreMatchLaunchStep::SteamCs2,
    ] {
        authorize_step().map_err(|source| PreMatchHandoffError::Authorization {
            step,
            completed: report,
            source,
        })?;
        let outcome = launch_step(step).map_err(|source| PreMatchHandoffError::Launch {
            step,
            completed: report,
            source,
        })?;
        report.record(step, outcome);
    }
    Ok(report)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerId(String);

impl WorkerId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerKind {
    Session,
    Voice,
    Stt,
    Llm,
    Tts,
    Learning,
    Embedding,
    Build,
    Sandbox,
    Extension,
    Broker,
    Script,
    Ui,
}

/// One of the built-in Jarvis binaries. This is intentionally not a path or
/// command-line string: the future supervisor may start only a manifest member
/// through its reviewed suspended-create path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InternalWorker {
    Session,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerSpec {
    id: &'static str,
    kind: WorkerKind,
    executable_name: &'static str,
}

impl WorkerSpec {
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.id
    }

    #[must_use]
    pub const fn kind(self) -> WorkerKind {
        self.kind
    }

    #[must_use]
    pub const fn executable_name(self) -> &'static str {
        self.executable_name
    }
}

/// The complete V1 internal-worker allowlist. It deliberately exposes no
/// constructor and no method that accepts an arbitrary executable or PID.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerManifest;

impl WorkerManifest {
    const WORKERS: [WorkerSpec; 1] = [WorkerSpec {
        id: "session",
        kind: WorkerKind::Session,
        executable_name: "jarvis-session-worker.exe",
    }];

    #[must_use]
    pub const fn all(&self) -> &'static [WorkerSpec] {
        &Self::WORKERS
    }

    #[must_use]
    pub const fn specification(&self, worker: InternalWorker) -> WorkerSpec {
        match worker {
            InternalWorker::Session => Self::WORKERS[0],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerState {
    Running,
    Stopped,
    Unresponsive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedWorker {
    pub id: WorkerId,
    pub kind: WorkerKind,
    pub state: WorkerState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    pub generation: ProfileGeneration,
    pub remaining_workers: Vec<WorkerId>,
}

impl ShutdownReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.remaining_workers.is_empty()
    }
}

pub trait WorkerLifecycle {
    fn shutdown_owned_workers(&mut self, generation: ProfileGeneration) -> ShutdownReport;
    fn owned_workers(&self) -> &[OwnedWorker];
}

/// A controlled test double. An unresponsive worker remains visible in the
/// shutdown report, which lets callers test restricted-fault behavior without
/// killing a process.
#[derive(Debug, Default)]
pub struct InMemoryWorkerLifecycle {
    workers: Vec<OwnedWorker>,
}

impl InMemoryWorkerLifecycle {
    #[must_use]
    pub fn with_running(workers: impl IntoIterator<Item = (WorkerId, WorkerKind)>) -> Self {
        Self {
            workers: workers
                .into_iter()
                .map(|(id, kind)| OwnedWorker {
                    id,
                    kind,
                    state: WorkerState::Running,
                })
                .collect(),
        }
    }

    pub fn mark_unresponsive(&mut self, id: &str) {
        if let Some(worker) = self
            .workers
            .iter_mut()
            .find(|worker| worker.id.as_str() == id)
        {
            worker.state = WorkerState::Unresponsive;
        }
    }
}

impl WorkerLifecycle for InMemoryWorkerLifecycle {
    fn shutdown_owned_workers(&mut self, generation: ProfileGeneration) -> ShutdownReport {
        for worker in &mut self.workers {
            if worker.state == WorkerState::Running {
                worker.state = WorkerState::Stopped;
            }
        }
        ShutdownReport {
            generation,
            remaining_workers: self
                .workers
                .iter()
                .filter(|worker| worker.state != WorkerState::Stopped)
                .map(|worker| worker.id.clone())
                .collect(),
        }
    }

    fn owned_workers(&self) -> &[OwnedWorker] {
        &self.workers
    }
}

/// An opaque Job Object owned by the serialized Jarvis supervisor.
///
/// The unnamed Job Object and its raw handle remain opaque. Only
/// `WindowsWorkerManager` can use it to start a fixed internal worker; no
/// public PID or executable-path assignment API exists.
pub struct WindowsOwnedWorkerJob {
    job: KillOnCloseJob,
}

impl WindowsOwnedWorkerJob {
    pub fn create() -> Result<Self, Win32Error> {
        Ok(Self {
            job: KillOnCloseJob::create()?,
        })
    }

    pub fn active_process_count(&self) -> Result<u32, Win32Error> {
        self.job.active_process_count()
    }

    fn start_fixed_worker(
        &self,
        worker: InternalWorker,
        installation_directory: &Path,
    ) -> Result<RunningInternalWorker, WorkerLaunchError> {
        let worker = match worker {
            InternalWorker::Session => FixedInternalWorker::Session,
        };
        let suspended = self
            .job
            .create_suspended_fixed_worker(worker, installation_directory)?;
        self.job.assign_and_verify(&suspended)?;
        suspended.resume()
    }
}

/// Recorded identity for a running fixed-manifest worker. It is captured from
/// the process handle while suspended, before the worker can run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerProcessIdentity {
    worker: InternalWorker,
    process_id: u32,
    creation_time_filetime: u64,
}

/// The result of stopping the fixed worker set. A clean report is issued only
/// after the owned Job Object reports no active process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerShutdownReport {
    pub used_forced_termination: bool,
    pub all_workers_exited_successfully: bool,
}

impl WorkerProcessIdentity {
    #[must_use]
    pub const fn worker(self) -> InternalWorker {
        self.worker
    }

    #[must_use]
    pub const fn process_id(self) -> u32 {
        self.process_id
    }

    #[must_use]
    pub const fn creation_time_filetime(self) -> u64 {
        self.creation_time_filetime
    }
}

#[derive(Debug)]
pub enum WorkerManagerError {
    AlreadyRunning(InternalWorker),
    OwnedJobNotEmpty { active_processes: u32 },
    ShutdownIncomplete { active_processes: u32 },
    Launch(WorkerLaunchError),
    Job(Win32Error),
}

impl fmt::Display for WorkerManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning(worker) => {
                write!(formatter, "{worker:?} worker is already running")
            }
            Self::OwnedJobNotEmpty { active_processes } => write!(
                formatter,
                "the owned Job Object still contains {active_processes} process(es)"
            ),
            Self::ShutdownIncomplete { active_processes } => write!(
                formatter,
                "the owned Job Object still contains {active_processes} process(es) after forced termination"
            ),
            Self::Launch(error) => error.fmt(formatter),
            Self::Job(error) => error.fmt(formatter),
        }
    }
}

impl Error for WorkerManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::AlreadyRunning(_)
            | Self::OwnedJobNotEmpty { .. }
            | Self::ShutdownIncomplete { .. } => None,
            Self::Launch(error) => Some(error),
            Self::Job(error) => Some(error),
        }
    }
}

struct ManagedWorker {
    identity: WorkerProcessIdentity,
    process: RunningInternalWorker,
}

/// Trusted manager for the fixed internal-worker manifest. It intentionally
/// has no method that accepts a user PID, executable path, command line, or
/// inherited handle list.
pub struct WindowsWorkerManager {
    job: WindowsOwnedWorkerJob,
    workers: Vec<ManagedWorker>,
}

impl WindowsWorkerManager {
    const COOPERATIVE_SHUTDOWN_DEADLINE: Duration = Duration::from_millis(500);
    const FORCED_TERMINATION_VERIFICATION_DEADLINE: Duration = Duration::from_millis(500);
    const EMPTY_JOB_POLL_INTERVAL: Duration = Duration::from_millis(10);

    pub fn create() -> Result<Self, Win32Error> {
        Ok(Self {
            job: WindowsOwnedWorkerJob::create()?,
            workers: Vec::new(),
        })
    }

    pub fn active_process_count(&self) -> Result<u32, Win32Error> {
        self.job.active_process_count()
    }

    pub fn start(
        &mut self,
        worker: InternalWorker,
        installation_directory: &Path,
    ) -> Result<WorkerProcessIdentity, WorkerManagerError> {
        self.reconcile_for_start()?;
        if self
            .workers
            .iter()
            .any(|managed| managed.identity.worker() == worker)
        {
            return Err(WorkerManagerError::AlreadyRunning(worker));
        }

        let process = self
            .job
            .start_fixed_worker(worker, installation_directory)
            .map_err(WorkerManagerError::Launch)?;
        let identity = worker_identity(worker, process.identity());
        self.workers.push(ManagedWorker { identity, process });
        Ok(identity)
    }

    /// Requests the fixed workers' one cooperative operation, exit, then waits
    /// a bounded interval for the owned Job Object to become empty. If that
    /// does not happen, it terminates only this Job Object and independently
    /// verifies that the kernel reports no remaining member process.
    pub fn shutdown(&mut self) -> Result<WorkerShutdownReport, WorkerManagerError> {
        if self
            .active_process_count()
            .map_err(WorkerManagerError::Job)?
            == 0
        {
            let all_workers_exited_successfully = self.all_workers_exited_successfully()?;
            self.workers.clear();
            return Ok(WorkerShutdownReport {
                used_forced_termination: false,
                all_workers_exited_successfully,
            });
        }

        let mut all_signals_delivered = true;
        for managed in &self.workers {
            if managed.process.request_cooperative_shutdown().is_err() {
                all_signals_delivered = false;
            }
        }
        if all_signals_delivered && self.wait_until_empty(Self::COOPERATIVE_SHUTDOWN_DEADLINE)? {
            let all_workers_exited_successfully = self.all_workers_exited_successfully()?;
            self.workers.clear();
            return Ok(WorkerShutdownReport {
                used_forced_termination: false,
                all_workers_exited_successfully,
            });
        }

        self.job
            .job
            .terminate_all(1)
            .map_err(WorkerManagerError::Job)?;
        if self.wait_until_empty(Self::FORCED_TERMINATION_VERIFICATION_DEADLINE)? {
            self.workers.clear();
            return Ok(WorkerShutdownReport {
                used_forced_termination: true,
                all_workers_exited_successfully: false,
            });
        }

        Err(WorkerManagerError::ShutdownIncomplete {
            active_processes: self
                .active_process_count()
                .map_err(WorkerManagerError::Job)?,
        })
    }

    /// Drops identities for workers known to have exited only when the Job
    /// Object is empty. An exited parent with a surviving owned child blocks
    /// restart rather than allowing another worker to be created.
    fn reconcile_for_start(&mut self) -> Result<(), WorkerManagerError> {
        let active_processes = self
            .active_process_count()
            .map_err(WorkerManagerError::Job)?;
        if active_processes == 0 {
            self.workers.clear();
            return Ok(());
        }

        let all_known_workers_exited = self
            .workers
            .iter()
            .try_fold(true, |all_exited, managed| {
                managed
                    .process
                    .wait_for_exit(Duration::ZERO)
                    .map(|exited| all_exited && exited)
            })
            .map_err(WorkerManagerError::Job)?;
        if self.workers.is_empty() || all_known_workers_exited {
            return Err(WorkerManagerError::OwnedJobNotEmpty { active_processes });
        }
        Ok(())
    }

    fn wait_until_empty(&self, timeout: Duration) -> Result<bool, WorkerManagerError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self
                .active_process_count()
                .map_err(WorkerManagerError::Job)?
                == 0
            {
                return Ok(true);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            thread::sleep(remaining.min(Self::EMPTY_JOB_POLL_INTERVAL));
        }
    }

    fn all_workers_exited_successfully(&self) -> Result<bool, WorkerManagerError> {
        self.workers
            .iter()
            .try_fold(true, |all_succeeded, managed| {
                managed
                    .process
                    .exited_with_code()
                    .map(|exit_code| all_succeeded && exit_code == Some(0))
            })
            .map_err(WorkerManagerError::Job)
    }
}

fn worker_identity(
    worker: InternalWorker,
    identity: InternalWorkerIdentity,
) -> WorkerProcessIdentity {
    WorkerProcessIdentity {
        worker,
        process_id: identity.process_id(),
        creation_time_filetime: identity.creation_time_filetime(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_reports_unresponsive_owned_workers() {
        let mut lifecycle =
            InMemoryWorkerLifecycle::with_running([(WorkerId::new("voice-1"), WorkerKind::Voice)]);
        lifecycle.mark_unresponsive("voice-1");
        let report = lifecycle.shutdown_owned_workers(ProfileGeneration::initial());
        assert_eq!(report.remaining_workers, vec![WorkerId::new("voice-1")]);
    }

    #[test]
    fn native_owned_worker_job_verifies_an_empty_job() {
        let lifecycle = WindowsOwnedWorkerJob::create().expect("create empty Job Object");
        assert_eq!(
            lifecycle
                .active_process_count()
                .expect("query empty Job Object"),
            0
        );
    }

    #[test]
    fn manifest_allows_only_the_checked_in_session_worker() {
        let manifest = WorkerManifest;
        assert_eq!(manifest.all().len(), 1);
        assert_eq!(
            manifest
                .specification(InternalWorker::Session)
                .executable_name(),
            FixedInternalWorker::Session.executable_name()
        );
        assert_eq!(
            manifest.specification(InternalWorker::Session),
            WorkerSpec {
                id: "session",
                kind: WorkerKind::Session,
                executable_name: "jarvis-session-worker.exe",
            }
        );
    }

    #[test]
    fn empty_worker_manager_has_a_verified_clean_shutdown() {
        let mut manager = WindowsWorkerManager::create().expect("create empty worker manager");
        assert_eq!(
            manager.shutdown().expect("verify empty Job Object"),
            WorkerShutdownReport {
                used_forced_termination: false,
                all_workers_exited_successfully: true,
            }
        );
        assert_eq!(manager.active_process_count().expect("query empty job"), 0);
    }

    #[test]
    fn pre_match_sequence_admits_and_starts_only_the_fixed_order() {
        let mut authorization_checks = 0;
        let mut started = Vec::new();
        let report = run_pre_match_sequence(
            || {
                authorization_checks += 1;
                Ok(())
            },
            |step| {
                started.push(step);
                Ok(PreMatchLaunchOutcome::Started)
            },
        )
        .expect("all fixed steps should pass the mock admission and launcher");

        assert_eq!(
            started,
            vec![
                PreMatchLaunchStep::FaceitAcLauncher,
                PreMatchLaunchStep::ChromeFaceitWebsite,
                PreMatchLaunchStep::SteamCs2,
            ]
        );
        assert_eq!(authorization_checks, 3);
        assert!(report.faceit_ac_launcher_started);
        assert!(report.chrome_faceit_url_requested);
        assert!(!report.chrome_faceit_title_match_detected);
        assert!(report.steam_cs2_launch_requested);
    }

    #[test]
    fn faceit_title_match_is_reported_and_does_not_stop_steam_handoff() {
        let mut started = Vec::new();
        let mut authorization_checks = 0;
        let report = run_pre_match_sequence(
            || {
                authorization_checks += 1;
                Ok(())
            },
            |step| {
                started.push(step);
                Ok(if step == PreMatchLaunchStep::ChromeFaceitWebsite {
                    PreMatchLaunchOutcome::FaceitTitleMatchDetected
                } else {
                    PreMatchLaunchOutcome::Started
                })
            },
        )
        .expect("a FACEIT title match should satisfy the Chrome step");

        assert_eq!(authorization_checks, 3);
        assert_eq!(
            started,
            vec![
                PreMatchLaunchStep::FaceitAcLauncher,
                PreMatchLaunchStep::ChromeFaceitWebsite,
                PreMatchLaunchStep::SteamCs2,
            ]
        );
        assert!(report.faceit_ac_launcher_started);
        assert!(!report.chrome_faceit_url_requested);
        assert!(report.chrome_faceit_title_match_detected);
        assert!(report.steam_cs2_launch_requested);
    }

    #[test]
    fn pre_match_sequence_stops_after_a_failed_step_and_reports_partial_progress() {
        let mut started = Vec::new();
        let result = run_pre_match_sequence(
            || Ok(()),
            |step| {
                started.push(step);
                if step == PreMatchLaunchStep::ChromeFaceitWebsite {
                    Err("mock Chrome start failure".to_owned())
                } else {
                    Ok(PreMatchLaunchOutcome::Started)
                }
            },
        );

        assert_eq!(
            result,
            Err(PreMatchHandoffError::Launch {
                step: PreMatchLaunchStep::ChromeFaceitWebsite,
                completed: PreMatchHandoffReport {
                    faceit_ac_launcher_started: true,
                    chrome_faceit_url_requested: false,
                    chrome_faceit_title_match_detected: false,
                    steam_cs2_launch_requested: false,
                },
                source: "mock Chrome start failure".to_owned(),
            })
        );
        assert_eq!(
            started,
            vec![
                PreMatchLaunchStep::FaceitAcLauncher,
                PreMatchLaunchStep::ChromeFaceitWebsite,
            ]
        );
    }

    #[test]
    fn pre_match_sequence_rechecks_authorization_before_each_step() {
        let mut authorization_checks = 0;
        let mut started = Vec::new();
        let result = run_pre_match_sequence(
            || {
                authorization_checks += 1;
                if authorization_checks == 2 {
                    Err(GateError::StaleGeneration)
                } else {
                    Ok(())
                }
            },
            |step| {
                started.push(step);
                Ok(PreMatchLaunchOutcome::Started)
            },
        );

        assert_eq!(
            result,
            Err(PreMatchHandoffError::Authorization {
                step: PreMatchLaunchStep::ChromeFaceitWebsite,
                completed: PreMatchHandoffReport {
                    faceit_ac_launcher_started: true,
                    chrome_faceit_url_requested: false,
                    chrome_faceit_title_match_detected: false,
                    steam_cs2_launch_requested: false,
                },
                source: GateError::StaleGeneration,
            })
        );
        assert_eq!(started, vec![PreMatchLaunchStep::FaceitAcLauncher]);
    }
}
