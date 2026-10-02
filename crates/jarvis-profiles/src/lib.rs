#![forbid(unsafe_code)]

//! Trusted profile policy. The supervisor owns the only mutable instance.

use jarvis_core::{ProfileGeneration, RequestContext};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    GodMode,
    Competitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityState {
    Disabled,
    Sleeping,
    Listening,
    Transcribing,
    Routing,
    Planning,
    AwaitingConfirmation,
    Executing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionState {
    Stable,
    EnteringCompetitive,
    RestoringGodMode,
    RestrictedFault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionCause {
    Manual,
    Cs2Process,
    FaceitProcess,
    FaceitService,
    DetectionUnhealthy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShutdownVerification {
    Clean,
    Incomplete { reason: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateError {
    StaleGeneration,
    ProfileRestricted,
    ActionGateClosed,
    WorkerStartGateClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreError {
    TriggersActive,
    DetectionUnhealthy,
    ManualLatchActive,
    RestrictedFault,
    NotCompetitive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionStart {
    pub generation: ProfileGeneration,
    pub was_already_restricted: bool,
}

/// Policy state deliberately has no public mutation APIs for gates.
#[derive(Debug)]
pub struct ProfileController {
    profile: Profile,
    activity: ActivityState,
    transition: TransitionState,
    generation: ProfileGeneration,
    action_gate_open: bool,
    worker_start_gate_open: bool,
    manual_competitive_latch: bool,
}

impl Default for ProfileController {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileController {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            profile: Profile::GodMode,
            activity: ActivityState::Sleeping,
            transition: TransitionState::Stable,
            generation: ProfileGeneration::initial(),
            action_gate_open: true,
            worker_start_gate_open: true,
            manual_competitive_latch: false,
        }
    }

    #[must_use]
    pub const fn profile(&self) -> Profile {
        self.profile
    }

    #[must_use]
    pub const fn activity(&self) -> ActivityState {
        self.activity
    }

    #[must_use]
    pub const fn transition(&self) -> TransitionState {
        self.transition
    }

    #[must_use]
    pub const fn generation(&self) -> ProfileGeneration {
        self.generation
    }

    #[must_use]
    pub const fn action_gate_open(&self) -> bool {
        self.action_gate_open
    }

    #[must_use]
    pub const fn worker_start_gate_open(&self) -> bool {
        self.worker_start_gate_open
    }

    pub fn set_activity(&mut self, activity: ActivityState) -> Result<(), GateError> {
        if self.profile != Profile::GodMode || !self.action_gate_open {
            return Err(GateError::ProfileRestricted);
        }
        self.activity = activity;
        Ok(())
    }

    /// Atomically closes both admission gates and advances the generation on
    /// the first transition into a restricted state.
    pub fn enter_competitive(&mut self, cause: TransitionCause) -> TransitionStart {
        let already_restricted = !self.action_gate_open && !self.worker_start_gate_open;
        if !already_restricted {
            self.generation = self.generation.next();
            self.action_gate_open = false;
            self.worker_start_gate_open = false;
            self.profile = Profile::Competitive;
            self.activity = ActivityState::Disabled;
            self.transition = TransitionState::EnteringCompetitive;
        }
        if cause == TransitionCause::Manual {
            self.manual_competitive_latch = true;
        }
        TransitionStart {
            generation: self.generation,
            was_already_restricted: already_restricted,
        }
    }

    pub fn finish_competitive_shutdown(&mut self, verification: ShutdownVerification) {
        self.profile = Profile::Competitive;
        self.activity = ActivityState::Disabled;
        self.action_gate_open = false;
        self.worker_start_gate_open = false;
        self.transition = match verification {
            ShutdownVerification::Clean => TransitionState::Stable,
            ShutdownVerification::Incomplete { .. } => TransitionState::RestrictedFault,
        };
    }

    pub fn release_manual_competitive_latch(&mut self) {
        self.manual_competitive_latch = false;
    }

    pub fn restore_god_mode(
        &mut self,
        triggers_active: bool,
        detection_healthy: bool,
    ) -> Result<(), RestoreError> {
        if self.profile != Profile::Competitive {
            return Err(RestoreError::NotCompetitive);
        }
        if self.transition == TransitionState::RestrictedFault {
            return Err(RestoreError::RestrictedFault);
        }
        if triggers_active {
            return Err(RestoreError::TriggersActive);
        }
        if !detection_healthy {
            return Err(RestoreError::DetectionUnhealthy);
        }
        if self.manual_competitive_latch {
            return Err(RestoreError::ManualLatchActive);
        }

        self.transition = TransitionState::RestoringGodMode;
        self.profile = Profile::GodMode;
        self.activity = ActivityState::Sleeping;
        self.action_gate_open = true;
        self.worker_start_gate_open = true;
        self.transition = TransitionState::Stable;
        Ok(())
    }

    pub fn authorize_action(&self, request: RequestContext) -> Result<(), GateError> {
        self.authorize(request, self.action_gate_open, GateError::ActionGateClosed)
    }

    pub fn authorize_worker_start(&self, request: RequestContext) -> Result<(), GateError> {
        self.authorize(
            request,
            self.worker_start_gate_open,
            GateError::WorkerStartGateClosed,
        )
    }

    fn authorize(
        &self,
        request: RequestContext,
        gate_open: bool,
        closed_error: GateError,
    ) -> Result<(), GateError> {
        if request.generation != self.generation {
            return Err(GateError::StaleGeneration);
        }
        if self.profile != Profile::GodMode {
            return Err(GateError::ProfileRestricted);
        }
        if !gate_open {
            return Err(closed_error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::RequestId;

    fn request(controller: &ProfileController) -> RequestContext {
        RequestContext::new(RequestId::next(), controller.generation())
    }

    #[test]
    fn stale_request_cannot_cross_competitive_boundary() {
        let mut controller = ProfileController::new();
        let old_request = request(&controller);
        controller.enter_competitive(TransitionCause::Cs2Process);
        assert_eq!(
            controller.authorize_action(old_request),
            Err(GateError::StaleGeneration)
        );
    }

    #[test]
    fn incomplete_shutdown_stays_restricted() {
        let mut controller = ProfileController::new();
        controller.enter_competitive(TransitionCause::Cs2Process);
        controller.finish_competitive_shutdown(ShutdownVerification::Incomplete {
            reason: "voice worker did not exit".to_owned(),
        });
        assert_eq!(controller.transition(), TransitionState::RestrictedFault);
        assert_eq!(
            controller.restore_god_mode(false, true),
            Err(RestoreError::RestrictedFault)
        );
    }

    #[test]
    fn manual_latch_requires_explicit_release() {
        let mut controller = ProfileController::new();
        controller.enter_competitive(TransitionCause::Manual);
        controller.finish_competitive_shutdown(ShutdownVerification::Clean);
        assert_eq!(
            controller.restore_god_mode(false, true),
            Err(RestoreError::ManualLatchActive)
        );
        controller.release_manual_competitive_latch();
        assert_eq!(controller.restore_god_mode(false, true), Ok(()));
        assert!(controller.action_gate_open());
    }
}
