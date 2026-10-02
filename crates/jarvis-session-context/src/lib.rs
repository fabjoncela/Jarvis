#![forbid(unsafe_code)]

//! Bounded, RAM-only context for one user session and task. The supervisor
//! supplies current profile generation on every call. This crate stores no
//! utterance, transcript, window title, secret, or executable path. Its opaque
//! target IDs must be resolved and revalidated against the trusted registry
//! before an action; a context lookup is never action authorization.

use std::collections::VecDeque;
use std::mem::size_of;
use std::time::{Duration, Instant};

use jarvis_core::ProfileGeneration;
use jarvis_verification::ExecutionAnchor;

pub const MAX_RECORDS: usize = 100;
pub const MAX_SESSION_BYTES: usize = 8 * 1024;
pub const RECORD_TTL: Duration = Duration::from_secs(30 * 60);
pub const TARGET_TTL: Duration = Duration::from_secs(90);
pub const FEEDBACK_TTL: Duration = Duration::from_secs(5 * 60);
const FEEDBACK_AMBIGUITY_WINDOW: Duration = Duration::from_secs(15);

/// Opaque registry identity, not a display name, path, or user-supplied text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetId(u64);

impl TargetId {
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(Self(value))
        }
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    StaleGeneration,
    ConflictingDuplicate,
    OutOfOrder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordResult {
    Recorded,
    AlreadyRecorded,
}

#[derive(Clone, Copy, Debug)]
struct SessionRecord {
    anchor: ExecutionAnchor,
    target: Option<TargetId>,
    at: Instant,
}

/// One instance belongs to one session and task; drop or `clear` it on logout,
/// task completion, or restart. It is not persisted or shared with workers.
#[derive(Debug)]
pub struct SessionContext {
    generation: ProfileGeneration,
    records: VecDeque<SessionRecord>,
}

impl SessionContext {
    #[must_use]
    pub fn new(generation: ProfileGeneration) -> Self {
        Self {
            generation,
            records: VecDeque::new(),
        }
    }

    /// Record an execution identity, not a verified outcome. The caller must
    /// have established the execution through the supervisor. A repeated
    /// anchor is idempotent only when its target and timestamp also match.
    pub fn record_execution(
        &mut self,
        anchor: ExecutionAnchor,
        target: Option<TargetId>,
        at: Instant,
        current_generation: ProfileGeneration,
    ) -> Result<RecordResult, SessionError> {
        if !self.sync_generation(current_generation)
            || anchor.request.generation != current_generation
        {
            return Err(SessionError::StaleGeneration);
        }
        if let Some(existing) = self.records.iter().find(|record| record.anchor == anchor) {
            return if existing.target == target && existing.at == at {
                Ok(RecordResult::AlreadyRecorded)
            } else {
                Err(SessionError::ConflictingDuplicate)
            };
        }
        if self.records.iter().any(|record| record.at > at) {
            return Err(SessionError::OutOfOrder);
        }
        self.prune(at);
        let record_bytes = size_of::<SessionRecord>();
        while self.records.len() >= MAX_RECORDS
            || self.stored_bytes().saturating_add(record_bytes) > MAX_SESSION_BYTES
        {
            self.records.pop_front();
        }
        self.records.push_back(SessionRecord { anchor, target, at });
        Ok(RecordResult::Recorded)
    }

    /// Resolve a short-lived target reference only when every recent action
    /// names the same target. Missing or conflicting targets require explicit
    /// clarification. The registry and live target still need revalidation.
    #[must_use]
    pub fn resolve_target(
        &mut self,
        now: Instant,
        current_generation: ProfileGeneration,
    ) -> Option<TargetId> {
        if !self.sync_generation(current_generation) {
            return None;
        }
        self.prune(now);
        let mut resolved = None;
        for record in &self.records {
            if !is_fresh(now, record.at, TARGET_TTL) {
                continue;
            }
            match (resolved, record.target) {
                (_, None) => return None,
                (Some(previous), Some(target)) if previous != target => return None,
                (None, Some(target)) => resolved = Some(target),
                _ => {}
            }
        }
        resolved
    }

    /// Return only an exact execution anchor for implicit user feedback. Two
    /// executions within 15 seconds of the latest are ambiguous, so callers
    /// must ask which one. This returns no machine evidence or success claim.
    #[must_use]
    pub fn resolve_feedback_anchor(
        &mut self,
        now: Instant,
        current_generation: ProfileGeneration,
    ) -> Option<ExecutionAnchor> {
        if !self.sync_generation(current_generation) {
            return None;
        }
        self.prune(now);
        let latest = self.records.iter().max_by_key(|record| record.at)?;
        if !is_fresh(now, latest.at, FEEDBACK_TTL) {
            return None;
        }
        let ambiguous = self.records.iter().any(|record| {
            record.anchor != latest.anchor
                && latest
                    .at
                    .checked_duration_since(record.at)
                    .is_some_and(|age| age <= FEEDBACK_AMBIGUITY_WINDOW)
        });
        (!ambiguous).then_some(latest.anchor)
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub fn stored_bytes(&self) -> usize {
        self.records.len() * size_of::<SessionRecord>()
    }

    /// Returns false for a delayed old-generation caller without changing the
    /// newer context. Only forward transitions invalidate the current records.
    fn sync_generation(&mut self, current_generation: ProfileGeneration) -> bool {
        if current_generation < self.generation {
            return false;
        }
        if current_generation > self.generation {
            self.records.clear();
            self.generation = current_generation;
        }
        true
    }

    fn prune(&mut self, now: Instant) {
        self.records
            .retain(|record| is_fresh(now, record.at, RECORD_TTL));
    }
}

fn is_fresh(now: Instant, at: Instant, ttl: Duration) -> bool {
    now.checked_duration_since(at).is_some_and(|age| age <= ttl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::{RequestContext, RequestId};
    use jarvis_verification::ExecutionId;

    fn anchor(generation: ProfileGeneration) -> ExecutionAnchor {
        ExecutionAnchor::new(
            RequestContext::new(RequestId::next(), generation),
            ExecutionId::next(),
        )
    }

    #[test]
    fn records_are_bounded_by_count_and_bytes() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        for _ in 0..150 {
            context
                .record_execution(anchor(generation), TargetId::new(1), now, generation)
                .unwrap();
        }
        assert!(context.len() <= MAX_RECORDS);
        assert!(context.stored_bytes() <= MAX_SESSION_BYTES);
    }

    #[test]
    fn record_ttl_and_reference_ttl_are_independent() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        context
            .record_execution(anchor(generation), TargetId::new(1), now, generation)
            .unwrap();
        assert_eq!(context.resolve_target(now, generation), TargetId::new(1));
        assert_eq!(
            context.resolve_target(now + TARGET_TTL + Duration::from_secs(1), generation),
            None
        );
        assert_eq!(context.len(), 1);
        assert_eq!(
            context
                .resolve_feedback_anchor(now + FEEDBACK_TTL + Duration::from_secs(1), generation),
            None
        );
        let _ = context.resolve_target(now + RECORD_TTL + Duration::from_secs(1), generation);
        assert!(context.is_empty());
    }

    #[test]
    fn conflicting_or_missing_recent_targets_do_not_resolve() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        context
            .record_execution(anchor(generation), TargetId::new(1), now, generation)
            .unwrap();
        context
            .record_execution(
                anchor(generation),
                TargetId::new(2),
                now + Duration::from_secs(1),
                generation,
            )
            .unwrap();
        assert_eq!(
            context.resolve_target(now + Duration::from_secs(2), generation),
            None
        );
        context.clear();
        context
            .record_execution(anchor(generation), None, now, generation)
            .unwrap();
        assert_eq!(context.resolve_target(now, generation), None);
    }

    #[test]
    fn feedback_anchor_is_exact_recent_and_unambiguous() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        let first = anchor(generation);
        context
            .record_execution(first, TargetId::new(1), now, generation)
            .unwrap();
        assert_eq!(
            context.resolve_feedback_anchor(now, generation),
            Some(first)
        );
        let second = anchor(generation);
        context
            .record_execution(
                second,
                TargetId::new(1),
                now + Duration::from_secs(1),
                generation,
            )
            .unwrap();
        assert_eq!(
            context.resolve_feedback_anchor(now + Duration::from_secs(2), generation),
            None
        );
        let third = anchor(generation);
        context
            .record_execution(
                third,
                TargetId::new(1),
                now + Duration::from_secs(18),
                generation,
            )
            .unwrap();
        assert_eq!(
            context.resolve_feedback_anchor(now + Duration::from_secs(19), generation),
            Some(third)
        );
    }

    #[test]
    fn generation_change_clears_context_and_rejects_old_anchor() {
        let first = ProfileGeneration::initial();
        let second = first.next();
        let mut context = SessionContext::new(first);
        let now = Instant::now();
        let old = anchor(first);
        context
            .record_execution(old, TargetId::new(1), now, first)
            .unwrap();
        assert_eq!(context.resolve_feedback_anchor(now, second), None);
        assert!(context.is_empty());
        assert_eq!(
            context.record_execution(old, TargetId::new(1), now, second),
            Err(SessionError::StaleGeneration)
        );
        let new = anchor(second);
        context
            .record_execution(new, TargetId::new(2), now, second)
            .unwrap();
        assert_eq!(context.resolve_feedback_anchor(now, second), Some(new));
    }

    #[test]
    fn delayed_old_generation_cannot_clear_or_repopulate_newer_context() {
        let old = ProfileGeneration::initial();
        let new = old.next();
        let mut context = SessionContext::new(old);
        let now = Instant::now();
        let newer_execution = anchor(new);
        context
            .record_execution(newer_execution, TargetId::new(2), now, new)
            .unwrap();

        assert_eq!(context.resolve_target(now, old), None);
        assert_eq!(context.resolve_feedback_anchor(now, old), None);
        assert_eq!(
            context.record_execution(anchor(old), TargetId::new(1), now, old),
            Err(SessionError::StaleGeneration)
        );

        assert_eq!(context.len(), 1);
        assert_eq!(context.resolve_target(now, new), TargetId::new(2));
        assert_eq!(
            context.resolve_feedback_anchor(now, new),
            Some(newer_execution)
        );
    }

    #[test]
    fn duplicate_anchor_cannot_rebind_target_or_time() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        let execution = anchor(generation);
        assert_eq!(
            context.record_execution(execution, TargetId::new(1), now, generation),
            Ok(RecordResult::Recorded)
        );
        assert_eq!(
            context.record_execution(execution, TargetId::new(1), now, generation),
            Ok(RecordResult::AlreadyRecorded)
        );
        assert_eq!(
            context.record_execution(execution, TargetId::new(2), now, generation),
            Err(SessionError::ConflictingDuplicate)
        );
    }

    #[test]
    fn out_of_order_input_cannot_erase_newer_context() {
        let generation = ProfileGeneration::initial();
        let mut context = SessionContext::new(generation);
        let now = Instant::now();
        let newer = anchor(generation);
        context
            .record_execution(
                newer,
                TargetId::new(1),
                now + Duration::from_secs(10),
                generation,
            )
            .unwrap();
        assert_eq!(
            context.record_execution(anchor(generation), TargetId::new(2), now, generation),
            Err(SessionError::OutOfOrder)
        );
        assert_eq!(
            context.resolve_feedback_anchor(now + Duration::from_secs(10), generation),
            Some(newer)
        );
    }
}
