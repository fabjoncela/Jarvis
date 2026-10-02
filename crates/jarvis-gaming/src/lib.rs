#![forbid(unsafe_code)]

//! Competitive-mode orchestration. Trigger observations are abstract so this
//! crate can be tested without looking into a game or anti-cheat process.

use jarvis_profiles::{
    ProfileController, RestoreError, ShutdownVerification, TransitionCause, TransitionStart,
    TransitionState,
};
use jarvis_windows::{
    observe_cs2_processes, verify_default_pre_match_setup, PreMatchSetupError,
    ProcessObservationError, WorkerLifecycle, WorkerState,
};

/// Read-only setup check for the three fixed pre-match launch targets. The
/// handoff invokes it again immediately before every step; a previous
/// successful check is never a durable grant.
pub trait PreMatchSetupSource {
    fn verify_pre_match_setup(&mut self) -> Result<(), PreMatchSetupError>;
}

/// Native source for the fixed local FACEIT, Chrome, Steam, and CS2 checks.
/// It has no API for arbitrary paths, services, processes, or URLs.
pub struct WindowsPreMatchSetupSource;

impl PreMatchSetupSource for WindowsPreMatchSetupSource {
    fn verify_pre_match_setup(&mut self) -> Result<(), PreMatchSetupError> {
        verify_default_pre_match_setup().map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreMatchSetupStatus {
    VerifiedForCurrentCheck,
    Unverified,
}

/// Converts a verifier result into an explicit, non-cacheable status. The
/// error is retained by the caller for diagnostics; this status alone may not
/// authorize an operating-system action.
pub fn pre_match_setup_status(
    source: &mut impl PreMatchSetupSource,
) -> Result<PreMatchSetupStatus, PreMatchSetupError> {
    source
        .verify_pre_match_setup()
        .map(|()| PreMatchSetupStatus::VerifiedForCurrentCheck)
}

/// Read-only source for the fixed `cs2.exe` trigger. This trait keeps policy
/// tests deterministic and prevents the gaming layer from accepting arbitrary
/// process names or PIDs.
pub trait Cs2ProcessSource {
    fn cs2_is_running(&mut self) -> Result<bool, ProcessObservationError>;
}

#[derive(Debug)]
pub enum PreMatchReadinessError {
    Cs2AlreadyRunning,
    Cs2ObservationUnavailable(ProcessObservationError),
}

impl std::fmt::Display for PreMatchReadinessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cs2AlreadyRunning => write!(formatter, "CS2 is already running"),
            Self::Cs2ObservationUnavailable(error) => {
                write!(
                    formatter,
                    "could not verify whether CS2 is running: {error}"
                )
            }
        }
    }
}

impl std::error::Error for PreMatchReadinessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cs2ObservationUnavailable(error) => Some(error),
            Self::Cs2AlreadyRunning => None,
        }
    }
}

/// Denies a fresh pre-match launch if CS2 is already active or the fixed
/// read-only process snapshot is unavailable.
pub fn ensure_cs2_not_running(
    source: &mut impl Cs2ProcessSource,
) -> Result<(), PreMatchReadinessError> {
    match source.cs2_is_running() {
        Ok(false) => Ok(()),
        Ok(true) => Err(PreMatchReadinessError::Cs2AlreadyRunning),
        Err(error) => Err(PreMatchReadinessError::Cs2ObservationUnavailable(error)),
    }
}

/// The ordinary Windows implementation uses a Tool Help process snapshot with
/// limited-query PID/creation-time verification. It does not attach to, inject
/// into, or modify the game process.
pub struct WindowsCs2ProcessSource;

impl Cs2ProcessSource for WindowsCs2ProcessSource {
    fn cs2_is_running(&mut self) -> Result<bool, ProcessObservationError> {
        Ok(!observe_cs2_processes()?.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetectionHealth {
    Healthy,
    FaceitSetupUnverified,
    Cs2ObservationUnavailable,
}

/// Result of one read-only trigger snapshot. Any unhealthy result has
/// `detection_healthy == false`, so restoration remains denied by policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TriggerObservation {
    pub snapshot: TriggerSnapshot,
    pub health: DetectionHealth,
}

/// Produces fail-closed snapshots. FACEIT must remain unverified until setup
/// supplies explicit locally reviewed process and service identities; it is
/// never guessed from a vendor-like name.
pub struct FailClosedTriggerDetector<S> {
    source: S,
    faceit_setup_verified: bool,
}

impl<S> FailClosedTriggerDetector<S> {
    #[must_use]
    pub const fn new(source: S, faceit_setup_verified: bool) -> Self {
        Self {
            source,
            faceit_setup_verified,
        }
    }
}

impl<S: Cs2ProcessSource> FailClosedTriggerDetector<S> {
    pub fn observe(&mut self) -> TriggerObservation {
        match self.source.cs2_is_running() {
            Ok(cs2_running) if self.faceit_setup_verified => TriggerObservation {
                snapshot: TriggerSnapshot {
                    cs2_running,
                    detection_healthy: true,
                    ..TriggerSnapshot::default()
                },
                health: DetectionHealth::Healthy,
            },
            Ok(cs2_running) => TriggerObservation {
                snapshot: TriggerSnapshot {
                    cs2_running,
                    detection_healthy: false,
                    ..TriggerSnapshot::default()
                },
                health: DetectionHealth::FaceitSetupUnverified,
            },
            Err(_) => TriggerObservation {
                snapshot: TriggerSnapshot {
                    detection_healthy: false,
                    ..TriggerSnapshot::default()
                },
                health: DetectionHealth::Cs2ObservationUnavailable,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TriggerSnapshot {
    pub cs2_running: bool,
    pub verified_faceit_process_running: bool,
    pub verified_faceit_service_active: bool,
    pub detection_healthy: bool,
}

impl TriggerSnapshot {
    #[must_use]
    pub const fn any_automatic_trigger_active(self) -> bool {
        self.cs2_running
            || self.verified_faceit_process_running
            || self.verified_faceit_service_active
    }

    #[must_use]
    pub const fn first_cause(self) -> Option<TransitionCause> {
        if self.cs2_running {
            Some(TransitionCause::Cs2Process)
        } else if self.verified_faceit_process_running {
            Some(TransitionCause::FaceitProcess)
        } else if self.verified_faceit_service_active {
            Some(TransitionCause::FaceitService)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompetitiveTransitionResult {
    pub start: TransitionStart,
    pub clean_shutdown: bool,
    pub remaining_worker_count: usize,
}

pub struct CompetitiveCoordinator;

impl CompetitiveCoordinator {
    /// Starts containment before asking workers to stop. An incomplete report
    /// deliberately leaves the controller in `RestrictedFault`.
    pub fn enter(
        controller: &mut ProfileController,
        workers: &mut impl WorkerLifecycle,
        cause: TransitionCause,
    ) -> CompetitiveTransitionResult {
        let start = controller.enter_competitive(cause);
        // A transition fault cannot be erased by receiving another trigger.
        // Recovery must be an explicit, independently verified operation.
        if start.was_already_restricted {
            let remaining_worker_count = workers
                .owned_workers()
                .iter()
                .filter(|worker| worker.state != WorkerState::Stopped)
                .count();
            return CompetitiveTransitionResult {
                start,
                clean_shutdown: controller.transition() == TransitionState::Stable
                    && remaining_worker_count == 0,
                remaining_worker_count,
            };
        }
        let report = workers.shutdown_owned_workers(start.generation);
        let clean_shutdown = report.is_clean();
        let remaining_worker_count = report.remaining_workers.len();
        let verification = if clean_shutdown {
            ShutdownVerification::Clean
        } else {
            ShutdownVerification::Incomplete {
                reason: format!("{} owned worker(s) did not exit", remaining_worker_count),
            }
        };
        controller.finish_competitive_shutdown(verification);
        CompetitiveTransitionResult {
            start,
            clean_shutdown,
            remaining_worker_count,
        }
    }

    pub fn restore_if_safe(
        controller: &mut ProfileController,
        snapshot: TriggerSnapshot,
    ) -> Result<(), RestoreError> {
        controller.restore_god_mode(
            snapshot.any_automatic_trigger_active(),
            snapshot.detection_healthy,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_profiles::{Profile, TransitionState};
    use jarvis_windows::{FixedCompetitiveProcess, InMemoryWorkerLifecycle, WorkerId, WorkerKind};

    struct FixedCs2Source(Result<bool, ProcessObservationError>);

    struct FixedPreMatchSetupSource(Result<(), PreMatchSetupError>);

    impl PreMatchSetupSource for FixedPreMatchSetupSource {
        fn verify_pre_match_setup(&mut self) -> Result<(), PreMatchSetupError> {
            match &self.0 {
                Ok(()) => Ok(()),
                Err(_) => Err(PreMatchSetupError::Cs2ManifestMissing),
            }
        }
    }

    impl Cs2ProcessSource for FixedCs2Source {
        fn cs2_is_running(&mut self) -> Result<bool, ProcessObservationError> {
            match &self.0 {
                Ok(running) => Ok(*running),
                Err(_) => Err(ProcessObservationError::CandidateIdentityUnavailable {
                    process: FixedCompetitiveProcess::Cs2,
                }),
            }
        }
    }

    #[test]
    fn unverified_faceit_setup_keeps_detection_unhealthy_even_when_cs2_is_seen() {
        let mut detector = FailClosedTriggerDetector::new(FixedCs2Source(Ok(true)), false);
        let observation = detector.observe();
        assert!(observation.snapshot.cs2_running);
        assert!(!observation.snapshot.detection_healthy);
        assert_eq!(observation.health, DetectionHealth::FaceitSetupUnverified);
    }

    #[test]
    fn unavailable_cs2_snapshot_fails_closed() {
        let mut detector = FailClosedTriggerDetector::new(
            FixedCs2Source(Err(ProcessObservationError::CandidateIdentityUnavailable {
                process: FixedCompetitiveProcess::Cs2,
            })),
            true,
        );
        let observation = detector.observe();
        assert!(!observation.snapshot.cs2_running);
        assert!(!observation.snapshot.detection_healthy);
        assert_eq!(
            observation.health,
            DetectionHealth::Cs2ObservationUnavailable
        );
    }

    #[test]
    fn pre_match_status_is_not_a_persistent_authorization() {
        let mut source = FixedPreMatchSetupSource(Ok(()));
        assert!(matches!(
            pre_match_setup_status(&mut source),
            Ok(PreMatchSetupStatus::VerifiedForCurrentCheck)
        ));
    }

    #[test]
    fn pre_match_setup_failure_is_propagated() {
        let mut source = FixedPreMatchSetupSource(Err(PreMatchSetupError::Cs2ManifestMissing));
        assert!(matches!(
            pre_match_setup_status(&mut source),
            Err(PreMatchSetupError::Cs2ManifestMissing)
        ));
    }

    #[test]
    fn pre_match_is_denied_when_cs2_is_already_running() {
        let mut source = FixedCs2Source(Ok(true));
        assert!(matches!(
            ensure_cs2_not_running(&mut source),
            Err(PreMatchReadinessError::Cs2AlreadyRunning)
        ));
    }

    #[test]
    fn pre_match_is_denied_when_cs2_observation_fails() {
        let mut source =
            FixedCs2Source(Err(ProcessObservationError::CandidateIdentityUnavailable {
                process: FixedCompetitiveProcess::Cs2,
            }));
        assert!(matches!(
            ensure_cs2_not_running(&mut source),
            Err(PreMatchReadinessError::Cs2ObservationUnavailable(_))
        ));
    }

    #[test]
    fn incomplete_worker_shutdown_stays_restricted() {
        let mut controller = ProfileController::new();
        let mut workers =
            InMemoryWorkerLifecycle::with_running([(WorkerId::new("llm-1"), WorkerKind::Llm)]);
        workers.mark_unresponsive("llm-1");
        let result = CompetitiveCoordinator::enter(
            &mut controller,
            &mut workers,
            TransitionCause::Cs2Process,
        );
        assert!(!result.clean_shutdown);
        assert_eq!(controller.profile(), Profile::Competitive);
        assert_eq!(controller.transition(), TransitionState::RestrictedFault);
    }

    #[test]
    fn active_trigger_prevents_restore() {
        let mut controller = ProfileController::new();
        let mut workers = InMemoryWorkerLifecycle::default();
        CompetitiveCoordinator::enter(&mut controller, &mut workers, TransitionCause::Cs2Process);
        assert_eq!(
            CompetitiveCoordinator::restore_if_safe(
                &mut controller,
                TriggerSnapshot {
                    cs2_running: true,
                    detection_healthy: true,
                    ..TriggerSnapshot::default()
                },
            ),
            Err(RestoreError::TriggersActive)
        );
    }

    #[test]
    fn new_trigger_cannot_clear_an_existing_restricted_fault() {
        let mut controller = ProfileController::new();
        let mut workers =
            InMemoryWorkerLifecycle::with_running([(WorkerId::new("voice-1"), WorkerKind::Voice)]);
        workers.mark_unresponsive("voice-1");
        CompetitiveCoordinator::enter(&mut controller, &mut workers, TransitionCause::Cs2Process);

        let result = CompetitiveCoordinator::enter(
            &mut controller,
            &mut workers,
            TransitionCause::FaceitProcess,
        );
        assert!(!result.clean_shutdown);
        assert_eq!(controller.transition(), TransitionState::RestrictedFault);
    }
}
