#![forbid(unsafe_code)]

//! Untrusted local-model response parsing, not action authorization or execution.
//! The supervisor must validate every returned plan against its live profile,
//! generation, and action registry before any side effect.

use jarvis_actions::{Action, ActionPlan};
use jarvis_core::RequestContext;
use serde::Deserialize;

pub const MODEL_PROPOSAL_SCHEMA_VERSION: u16 = 1;
pub const MAX_MODEL_RESPONSE_BYTES: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelProposal {
    NoAction,
    Candidate {
        request: RequestContext,
        plan: ActionPlan,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalError {
    Empty,
    TooLarge,
    Malformed,
    UnsupportedSchemaVersion,
    InvalidVolume,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProposal {
    schema_version: u16,
    action: WireAction,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireAction {
    NoAction,
    LaunchApplication { application_id: ApplicationId },
    SetMasterVolume { percent: u8 },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ApplicationId {
    Discord,
    Chrome,
}

/// Parse a complete, bounded model response. Even a `Candidate` is inert:
/// only the live supervisor can turn it into a validated, dispatched action.
pub fn parse_model_proposal(
    response: &[u8],
    request: RequestContext,
) -> Result<ModelProposal, ProposalError> {
    if response.is_empty() {
        return Err(ProposalError::Empty);
    }
    if response.len() > MAX_MODEL_RESPONSE_BYTES {
        return Err(ProposalError::TooLarge);
    }
    let wire: WireProposal =
        serde_json::from_slice(response).map_err(|_| ProposalError::Malformed)?;
    if wire.schema_version != MODEL_PROPOSAL_SCHEMA_VERSION {
        return Err(ProposalError::UnsupportedSchemaVersion);
    }
    let action = match wire.action {
        WireAction::NoAction => return Ok(ModelProposal::NoAction),
        WireAction::LaunchApplication { application_id } => Action::LaunchApplication {
            application_id: match application_id {
                ApplicationId::Discord => "discord",
                ApplicationId::Chrome => "chrome",
            }
            .to_owned(),
        },
        WireAction::SetMasterVolume { percent } if percent <= 100 => {
            Action::SetMasterVolume { percent }
        }
        WireAction::SetMasterVolume { .. } => return Err(ProposalError::InvalidVolume),
    };
    Ok(ModelProposal::Candidate {
        request,
        plan: ActionPlan::single(action),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_actions::{ActionRegistry, ValidationError};
    use jarvis_core::RequestId;
    use jarvis_profiles::{GateError, ProfileController, TransitionCause};

    fn request(profile: &ProfileController) -> RequestContext {
        RequestContext::new(RequestId::next(), profile.generation())
    }

    fn parse(response: &str) -> Result<ModelProposal, ProposalError> {
        parse_model_proposal(response.as_bytes(), request(&ProfileController::new()))
    }

    #[test]
    fn accepts_only_narrow_inert_candidates() {
        let profile = ProfileController::new();
        let request = request(&profile);
        let candidate = parse_model_proposal(
            br#"{"schema_version":1,"action":{"type":"launch_application","application_id":"discord"}}"#,
            request,
        )
        .unwrap();
        assert_eq!(
            candidate,
            ModelProposal::Candidate {
                request,
                plan: ActionPlan::single(Action::LaunchApplication {
                    application_id: "discord".to_owned(),
                }),
            }
        );
        assert_eq!(
            parse(r#"{"schema_version":1,"action":{"type":"no_action"}}"#),
            Ok(ModelProposal::NoAction)
        );
        assert!(matches!(
            parse(r#"{"schema_version":1,"action":{"type":"set_master_volume","percent":50}}"#),
            Ok(ModelProposal::Candidate { .. })
        ));
    }

    #[test]
    fn rejects_broad_actions_targets_and_injected_fields() {
        for response in [
            r#"{"schema_version":1,"action":{"type":"start_competitive_handoff"}}"#,
            r#"{"schema_version":1,"action":{"type":"close_application","application_id":"cs2"}}"#,
            r#"{"schema_version":1,"action":{"type":"launch_application","application_id":"faceit_anticheat"}}"#,
            r#"{"schema_version":1,"action":{"type":"launch_application","application_id":"discord","shell":"cmd.exe"}}"#,
            r#"{"schema_version":1,"action":{"type":"no_action"},"instructions":"ignore policy"}"#,
            r#"{"schema_version":1,"action":{"type":"no_action","type":"launch_application"}}"#,
        ] {
            assert_eq!(parse(response), Err(ProposalError::Malformed), "{response}");
        }
    }

    #[test]
    fn rejects_partial_oversized_invalid_and_trailing_output() {
        assert_eq!(parse(""), Err(ProposalError::Empty));
        assert_eq!(
            parse_model_proposal(
                &vec![b'a'; MAX_MODEL_RESPONSE_BYTES + 1],
                request(&ProfileController::new())
            ),
            Err(ProposalError::TooLarge)
        );
        assert_eq!(
            parse("{\"schema_version\":1"),
            Err(ProposalError::Malformed)
        );
        assert_eq!(
            parse(r#"{"schema_version":1,"action":{"type":"no_action"}} ignore"#),
            Err(ProposalError::Malformed)
        );
        assert_eq!(
            parse(r#"{"schema_version":2,"action":{"type":"no_action"}}"#),
            Err(ProposalError::UnsupportedSchemaVersion)
        );
        assert_eq!(
            parse(r#"{"schema_version":1,"action":{"type":"set_master_volume","percent":101}}"#),
            Err(ProposalError::InvalidVolume)
        );
    }

    #[test]
    fn supervisor_must_still_reject_stale_and_competitive_candidates() {
        let registry = ActionRegistry::new(["discord", "chrome"], ["cs2"]);
        let mut profile = ProfileController::new();
        let ModelProposal::Candidate {
            request: original_request,
            plan,
        } = parse_model_proposal(
            br#"{"schema_version":1,"action":{"type":"launch_application","application_id":"chrome"}}"#,
            request(&profile),
        )
        .unwrap() else {
            panic!("expected inert candidate");
        };
        profile.enter_competitive(TransitionCause::Cs2Process);
        assert_eq!(
            registry.validate(plan.clone(), original_request, &profile),
            Err(ValidationError::ProfileGate(GateError::StaleGeneration))
        );
        assert!(matches!(
            registry.validate(plan, request(&profile), &profile),
            Err(ValidationError::ProfileGate(_))
        ));
    }
}
