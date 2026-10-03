#![forbid(unsafe_code)]

//! Untrusted local-model response parsing, not action authorization or execution.
//! The supervisor must validate every returned plan against its live profile,
//! generation, and action registry before any side effect.

use jarvis_actions::{Action, ActionPlan, ActionRegistry, ValidatedPlan, ValidationError};
use jarvis_core::RequestContext;
use jarvis_profiles::ProfileController;
use serde::{Deserialize, Serialize};

pub const MODEL_PROPOSAL_SCHEMA_VERSION: u16 = 1;
pub const MAX_MODEL_RESPONSE_BYTES: usize = 4096;
pub const MAX_MODEL_UTTERANCE_BYTES: usize = 1024;
const ALLOWED_ACTIONS: [&str; 3] = [
    "launch_application:discord",
    "launch_application:chrome",
    "set_master_volume",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    Empty,
    TooLarge,
    ControlCharacter,
}

#[derive(Serialize)]
struct WirePlanningRequest<'a> {
    schema_version: u16,
    request_id: u64,
    profile_generation: u64,
    utterance: &'a str,
    allowed_actions: &'static [&'static str],
}

/// Build a minimal, ephemeral request for the future fixed model worker.
/// The utterance is never logged or stored here, and the capability list is
/// compile-time fixed rather than supplied by a model or caller.
pub fn encode_planning_request(
    utterance: &str,
    request: RequestContext,
) -> Result<Vec<u8>, RequestError> {
    let utterance = utterance.trim();
    if utterance.is_empty() {
        return Err(RequestError::Empty);
    }
    if utterance.len() > MAX_MODEL_UTTERANCE_BYTES {
        return Err(RequestError::TooLarge);
    }
    if utterance.chars().any(char::is_control) {
        return Err(RequestError::ControlCharacter);
    }
    let wire = WirePlanningRequest {
        schema_version: MODEL_PROPOSAL_SCHEMA_VERSION,
        request_id: request.request_id.value(),
        profile_generation: request.generation.value(),
        utterance,
        allowed_actions: &ALLOWED_ACTIONS,
    };
    serde_json::to_vec(&wire).map_err(|_| RequestError::TooLarge)
}

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
    MismatchedRequest,
    InvalidVolume,
}

#[derive(Debug, Eq, PartialEq)]
pub enum AdmissionError {
    Protocol(ProposalError),
    Policy(ValidationError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ModelAdmission {
    NoAction,
    Validated(ValidatedPlan),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProposal {
    schema_version: u16,
    request_id: u64,
    profile_generation: u64,
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
    if wire.request_id != request.request_id.value()
        || wire.profile_generation != request.generation.value()
    {
        return Err(ProposalError::MismatchedRequest);
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

/// Admit an untrusted response only through the live Rust policy owner.
/// A caller must still reauthorize immediately before every later effect.
pub fn admit_model_proposal(
    response: &[u8],
    request: RequestContext,
    registry: &ActionRegistry,
    profile: &ProfileController,
) -> Result<ModelAdmission, AdmissionError> {
    // Even an abstention is not accepted across a profile generation change.
    profile
        .authorize_action(request)
        .map_err(ValidationError::ProfileGate)
        .map_err(AdmissionError::Policy)?;
    match parse_model_proposal(response, request).map_err(AdmissionError::Protocol)? {
        ModelProposal::NoAction => Ok(ModelAdmission::NoAction),
        ModelProposal::Candidate { request, plan } => registry
            .validate(plan, request, profile)
            .map(ModelAdmission::Validated)
            .map_err(AdmissionError::Policy),
    }
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
        let request = request(&ProfileController::new());
        parse_model_proposal(stamped(response, request).as_bytes(), request)
    }

    fn stamped(response: &str, request: RequestContext) -> String {
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(response) else {
            return response.to_owned();
        };
        let Some(object) = value.as_object_mut() else {
            return response.to_owned();
        };
        object.insert("request_id".to_owned(), request.request_id.value().into());
        object.insert(
            "profile_generation".to_owned(),
            request.generation.value().into(),
        );
        value.to_string()
    }

    #[test]
    fn accepts_only_narrow_inert_candidates() {
        let profile = ProfileController::new();
        let request = request(&profile);
        let response = stamped(
            r#"{"schema_version":1,"action":{"type":"launch_application","application_id":"discord"}}"#,
            request,
        );
        let candidate = parse_model_proposal(response.as_bytes(), request).unwrap();
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
    fn request_envelope_is_bounded_and_contains_only_fixed_fields() {
        let profile = ProfileController::new();
        let request = request(&profile);
        let encoded = encode_planning_request("  Open Discord  ", request).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["request_id"], request.request_id.value());
        assert_eq!(json["profile_generation"], request.generation.value());
        assert_eq!(json["utterance"], "Open Discord");
        assert_eq!(
            json["allowed_actions"],
            serde_json::json!([
                "launch_application:discord",
                "launch_application:chrome",
                "set_master_volume"
            ])
        );
        assert_eq!(
            encode_planning_request(" ", request),
            Err(RequestError::Empty)
        );
        assert_eq!(
            encode_planning_request("x\ny", request),
            Err(RequestError::ControlCharacter)
        );
        assert_eq!(
            encode_planning_request(&"x".repeat(MAX_MODEL_UTTERANCE_BYTES + 1), request),
            Err(RequestError::TooLarge)
        );
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
    fn rejects_reply_for_another_request_or_profile_generation() {
        let mut profile = ProfileController::new();
        let original = request(&profile);
        let response = stamped(
            r#"{"schema_version":1,"action":{"type":"no_action"}}"#,
            original,
        );
        assert_eq!(
            parse_model_proposal(response.as_bytes(), request(&profile)),
            Err(ProposalError::MismatchedRequest)
        );
        profile.enter_competitive(TransitionCause::Cs2Process);
        assert_eq!(
            parse_model_proposal(
                response.as_bytes(),
                RequestContext::new(original.request_id, profile.generation())
            ),
            Err(ProposalError::MismatchedRequest)
        );
    }

    #[test]
    fn supervisor_must_still_reject_stale_and_competitive_candidates() {
        let registry = ActionRegistry::new(["discord", "chrome"], ["cs2"]);
        let mut profile = ProfileController::new();
        let original_request = request(&profile);
        let response = stamped(
            r#"{"schema_version":1,"action":{"type":"launch_application","application_id":"chrome"}}"#,
            original_request,
        );
        let ModelProposal::Candidate {
            request: original_request,
            plan,
        } = parse_model_proposal(response.as_bytes(), original_request).unwrap()
        else {
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

    #[test]
    fn admission_applies_live_policy_to_model_output_and_abstention() {
        let registry = ActionRegistry::new(["discord"], ["cs2"]);
        let mut profile = ProfileController::new();
        let stale_request = request(&profile);
        let candidate = stamped(
            r#"{"schema_version":1,"action":{"type":"launch_application","application_id":"chrome"}}"#,
            stale_request,
        );
        assert_eq!(
            admit_model_proposal(candidate.as_bytes(), stale_request, &registry, &profile),
            Err(AdmissionError::Policy(
                ValidationError::UnregisteredApplication("chrome".to_owned())
            ))
        );
        let allowed_registry = ActionRegistry::new(["discord", "chrome"], ["cs2"]);
        let admitted = admit_model_proposal(
            candidate.as_bytes(),
            stale_request,
            &allowed_registry,
            &profile,
        )
        .unwrap();
        assert!(matches!(admitted, ModelAdmission::Validated(_)));
        assert_eq!(
            admit_model_proposal(
                stamped(
                    r#"{"schema_version":1,"action":{"type":"no_action"}}"#,
                    stale_request,
                )
                .as_bytes(),
                stale_request,
                &registry,
                &profile,
            ),
            Ok(ModelAdmission::NoAction)
        );
        profile.enter_competitive(TransitionCause::Cs2Process);
        assert_eq!(
            admit_model_proposal(candidate.as_bytes(), stale_request, &registry, &profile),
            Err(AdmissionError::Policy(ValidationError::ProfileGate(
                GateError::StaleGeneration
            )))
        );
        assert!(matches!(
            admit_model_proposal(
                stamped(
                    r#"{"schema_version":1,"action":{"type":"no_action"}}"#,
                    request(&profile),
                )
                .as_bytes(),
                request(&profile),
                &registry,
                &profile,
            ),
            Err(AdmissionError::Policy(ValidationError::ProfileGate(_)))
        ));
    }
}
