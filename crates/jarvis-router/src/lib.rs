#![forbid(unsafe_code)]

//! Deterministic routing only. There is intentionally no fuzzy matching or
//! model fallback in this initial milestone.

use std::collections::BTreeMap;

use jarvis_actions::{Action, ActionPlan};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RouteResult {
    Plan(ActionPlan),
    InvalidPattern { reason: String },
    Unknown,
}

#[derive(Debug, Default)]
pub struct Router {
    aliases: BTreeMap<String, Action>,
}

impl Router {
    #[must_use]
    pub fn starter_router() -> Self {
        let mut aliases = BTreeMap::new();
        aliases.insert(
            "open discord".to_owned(),
            Action::LaunchApplication {
                application_id: "discord".to_owned(),
            },
        );
        Self { aliases }
    }

    #[must_use]
    pub fn route(&self, utterance: &str) -> RouteResult {
        let normalized = normalize(utterance);
        if normalized.is_empty() {
            return RouteResult::Unknown;
        }
        if let Some(action) = self.aliases.get(&normalized) {
            return RouteResult::Plan(ActionPlan::single(action.clone()));
        }
        if let Some(value) = normalized.strip_prefix("set volume to ") {
            return match value.parse::<u8>() {
                Ok(percent) if percent <= 100 => {
                    RouteResult::Plan(ActionPlan::single(Action::SetMasterVolume { percent }))
                }
                _ => RouteResult::InvalidPattern {
                    reason: "volume must be an integer from 0 through 100".to_owned(),
                },
            };
        }
        RouteResult::Unknown
    }
}

#[must_use]
pub fn normalize(utterance: &str) -> String {
    let compact = utterance
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    compact
        .strip_prefix("jarvis, ")
        .or_else(|| compact.strip_prefix("jarvis "))
        .unwrap_or(&compact)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_alias_routes_without_a_model() {
        let router = Router::starter_router();
        assert!(matches!(
            router.route("Jarvis,   OPEN Discord"),
            RouteResult::Plan(_)
        ));
    }

    #[test]
    fn invalid_typed_pattern_does_not_become_a_plan() {
        let router = Router::starter_router();
        assert!(matches!(
            router.route("set volume to 101"),
            RouteResult::InvalidPattern { .. }
        ));
    }
}
