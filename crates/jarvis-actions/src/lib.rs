#![forbid(unsafe_code)]

//! Typed action-plan validation. This crate deliberately has no production
//! executor, so plans cannot yet cause a Windows side effect.

use std::collections::BTreeSet;

use jarvis_core::RequestContext;
use jarvis_profiles::{GateError, ProfileController};

pub const ACTION_PLAN_SCHEMA_VERSION: u16 = 1;
pub const MAX_ACTIONS_PER_PLAN: usize = 8;
const PRE_MATCH_HANDOFF_APPLICATIONS: [&str; 3] = ["faceit_anticheat", "chrome", "cs2"];

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Risk {
    Passive,
    Low,
    Medium,
    Dangerous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Action {
    /// The fixed, user-confirmed sequence: start the verified FACEIT
    /// Anti-Cheat Launcher, open `https://www.faceit.com/` in verified Chrome
    /// without authentication, then launch CS2 through Steam app 730. This is
    /// not three general launch capabilities.
    StartCompetitiveHandoff,
    LaunchApplication {
        application_id: String,
    },
    CloseApplication {
        application_id: String,
    },
    SetMasterVolume {
        percent: u8,
    },
    LockScreen,
}

impl Action {
    #[must_use]
    pub const fn risk(&self) -> Risk {
        match self {
            Self::LaunchApplication { .. } | Self::SetMasterVolume { .. } => Risk::Low,
            Self::StartCompetitiveHandoff => Risk::Medium,
            Self::CloseApplication { .. } => Risk::Medium,
            Self::LockScreen => Risk::Medium,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionPlan {
    pub schema_version: u16,
    pub actions: Vec<Action>,
}

impl ActionPlan {
    #[must_use]
    pub fn single(action: Action) -> Self {
        Self {
            schema_version: ACTION_PLAN_SCHEMA_VERSION,
            actions: vec![action],
        }
    }
}

#[derive(Debug, Default)]
pub struct ActionRegistry {
    configured_applications: BTreeSet<String>,
    protected_close_targets: BTreeSet<String>,
}

impl ActionRegistry {
    #[must_use]
    pub fn new(
        configured_applications: impl IntoIterator<Item = impl Into<String>>,
        protected_close_targets: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            configured_applications: configured_applications
                .into_iter()
                .map(Into::into)
                .collect(),
            protected_close_targets: protected_close_targets
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }

    pub fn validate(
        &self,
        plan: ActionPlan,
        request: RequestContext,
        profile: &ProfileController,
    ) -> Result<ValidatedPlan, ValidationError> {
        profile
            .authorize_action(request)
            .map_err(ValidationError::ProfileGate)?;

        if plan.schema_version != ACTION_PLAN_SCHEMA_VERSION {
            return Err(ValidationError::UnsupportedSchemaVersion(
                plan.schema_version,
            ));
        }
        if plan.actions.is_empty() || plan.actions.len() > MAX_ACTIONS_PER_PLAN {
            return Err(ValidationError::InvalidActionCount(plan.actions.len()));
        }
        for action in &plan.actions {
            self.validate_action(action)?;
        }

        let requires_confirmation = plan
            .actions
            .iter()
            .any(|action| action.risk() >= Risk::Medium);
        Ok(ValidatedPlan {
            request,
            plan,
            requires_confirmation,
        })
    }

    fn validate_action(&self, action: &Action) -> Result<(), ValidationError> {
        match action {
            Action::StartCompetitiveHandoff => {
                for application_id in PRE_MATCH_HANDOFF_APPLICATIONS {
                    if !self.configured_applications.contains(application_id) {
                        return Err(ValidationError::PreMatchHandoffUnavailable {
                            missing_application: application_id.to_owned(),
                        });
                    }
                }
            }
            Action::LaunchApplication { application_id }
            | Action::CloseApplication { application_id } => {
                if application_id.trim().is_empty() {
                    return Err(ValidationError::EmptyApplicationId);
                }
                if !self.configured_applications.contains(application_id) {
                    return Err(ValidationError::UnregisteredApplication(
                        application_id.clone(),
                    ));
                }
                if matches!(action, Action::CloseApplication { .. })
                    && self.protected_close_targets.contains(application_id)
                {
                    return Err(ValidationError::ProtectedCloseTarget(
                        application_id.clone(),
                    ));
                }
            }
            Action::SetMasterVolume { percent } if *percent > 100 => {
                return Err(ValidationError::InvalidVolume(*percent));
            }
            Action::SetMasterVolume { .. } | Action::LockScreen => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedPlan {
    request: RequestContext,
    plan: ActionPlan,
    requires_confirmation: bool,
}

impl ValidatedPlan {
    #[must_use]
    pub const fn request(&self) -> RequestContext {
        self.request
    }

    #[must_use]
    pub fn actions(&self) -> &[Action] {
        &self.plan.actions
    }

    #[must_use]
    pub const fn requires_confirmation(&self) -> bool {
        self.requires_confirmation
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    ProfileGate(GateError),
    UnsupportedSchemaVersion(u16),
    InvalidActionCount(usize),
    EmptyApplicationId,
    UnregisteredApplication(String),
    ProtectedCloseTarget(String),
    InvalidVolume(u8),
    PreMatchHandoffUnavailable { missing_application: String },
}

/// The implementation boundary for future Windows effects. A real executor
/// must recheck the request generation immediately before every side effect.
pub trait ActionExecutor {
    type Error;

    fn execute(&mut self, plan: &ValidatedPlan) -> Result<ExecutionReceipt, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionReceipt {
    pub request: RequestContext,
    pub dispatched_actions: usize,
}

/// Test-only style executor which records dispatches but never performs an OS
/// operation. It is useful for the first vertical slice and integration tests.
#[derive(Debug, Default)]
pub struct RecordingExecutor {
    receipts: Vec<ExecutionReceipt>,
}

impl RecordingExecutor {
    #[must_use]
    pub fn receipts(&self) -> &[ExecutionReceipt] {
        &self.receipts
    }
}

impl ActionExecutor for RecordingExecutor {
    type Error = std::convert::Infallible;

    fn execute(&mut self, plan: &ValidatedPlan) -> Result<ExecutionReceipt, Self::Error> {
        let receipt = ExecutionReceipt {
            request: plan.request(),
            dispatched_actions: plan.actions().len(),
        };
        self.receipts.push(receipt);
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::RequestId;
    use jarvis_profiles::TransitionCause;

    fn request(profile: &ProfileController) -> RequestContext {
        RequestContext::new(RequestId::next(), profile.generation())
    }

    #[test]
    fn protected_game_cannot_be_a_close_target() {
        let registry = ActionRegistry::new(["discord", "cs2"], ["cs2"]);
        let profile = ProfileController::new();
        let result = registry.validate(
            ActionPlan::single(Action::CloseApplication {
                application_id: "cs2".to_owned(),
            }),
            request(&profile),
            &profile,
        );
        assert_eq!(
            result,
            Err(ValidationError::ProtectedCloseTarget("cs2".to_owned()))
        );
    }

    #[test]
    fn validated_request_is_rejected_after_generation_change() {
        let registry = ActionRegistry::new(["discord", "cs2"], ["cs2"]);
        let mut profile = ProfileController::new();
        let old_request = request(&profile);
        profile.enter_competitive(TransitionCause::Cs2Process);
        let result = registry.validate(
            ActionPlan::single(Action::SetMasterVolume { percent: 50 }),
            old_request,
            &profile,
        );
        assert_eq!(
            result,
            Err(ValidationError::ProfileGate(GateError::StaleGeneration))
        );
    }

    #[test]
    fn pre_match_handoff_is_fixed_and_requires_confirmation() {
        let registry = ActionRegistry::new(["faceit_anticheat", "chrome", "cs2"], ["cs2"]);
        let profile = ProfileController::new();
        let plan = registry
            .validate(
                ActionPlan::single(Action::StartCompetitiveHandoff),
                request(&profile),
                &profile,
            )
            .expect("the complete fixed handoff catalog should validate");

        assert_eq!(plan.actions(), &[Action::StartCompetitiveHandoff]);
        assert!(plan.requires_confirmation());
    }

    #[test]
    fn pre_match_handoff_rejects_an_incomplete_catalog() {
        let registry = ActionRegistry::new(["chrome", "cs2"], ["cs2"]);
        let profile = ProfileController::new();
        let result = registry.validate(
            ActionPlan::single(Action::StartCompetitiveHandoff),
            request(&profile),
            &profile,
        );

        assert_eq!(
            result,
            Err(ValidationError::PreMatchHandoffUnavailable {
                missing_application: "faceit_anticheat".to_owned(),
            })
        );
    }
}
