#![forbid(unsafe_code)]

//! Competitive-mode orchestration. Trigger observations are abstract so this
//! crate can be tested without looking into a game or anti-cheat process.

use jarvis_profiles::{
    ProfileController, RestoreError, ShutdownVerification, TransitionCause, TransitionStart,
    TransitionState,
};
use jarvis_windows::{WorkerLifecycle, WorkerState};

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
    use jarvis_windows::{InMemoryWorkerLifecycle, WorkerId, WorkerKind};

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
