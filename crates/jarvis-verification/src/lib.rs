#![forbid(unsafe_code)]

//! Pure result-verification contracts. This crate observes no operating-system
//! state and grants no action, profile, storage, or skill-promotion authority.
//! A trusted native observer must be added inside this crate before production
//! code can construct machine evidence; dispatch and user reports alone cannot
//! produce a machine-verified success.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use jarvis_core::{ProfileGeneration, RequestContext};

const MAX_EVIDENCE: usize = 64;
const MAX_FEEDBACK: usize = 32;
const FEEDBACK_WINDOW: Duration = Duration::from_secs(5 * 60);
static NEXT_EXECUTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ExecutionId(u64);

impl ExecutionId {
    /// Allocate a fresh identity when a supervisor starts an execution.
    #[must_use]
    pub fn next() -> Self {
        Self(NEXT_EXECUTION_ID.fetch_add(1, Ordering::Relaxed))
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionAnchor {
    pub request: RequestContext,
    pub execution: ExecutionId,
}

impl ExecutionAnchor {
    #[must_use]
    pub const fn new(request: RequestContext, execution: ExecutionId) -> Self {
        Self { request, execution }
    }
}

/// Dispatch reports only whether an action was accepted for execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchState {
    Accepted,
    Rejected,
}

/// Machine outcome is independent of dispatch and user feedback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineOutcome {
    Pending,
    Blocked,
    Unknown,
    VerifiedSuccess,
    VerifiedFailure,
    Partial,
    Canceled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackKind {
    Worked,
    Failed,
}

/// An explicit user report. Its identifier supports retry deduplication.
/// It never counts as machine verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserFeedback {
    pub id: u64,
    pub anchor: ExecutionAnchor,
    pub kind: FeedbackKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordResult {
    Recorded,
    AlreadyRecorded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationError {
    InvalidDeadline,
    WrongExecution,
    StaleGeneration,
    DispatchAlreadyRecorded,
    DispatchMissing,
    ObservationBeforeDispatch,
    ObservationAfterDeadline,
    ObservationFromFuture,
    ObservationOutOfOrder,
    DispatchOutsideWindow,
    FeedbackOutsideWindow,
    Finalized,
    ConflictingDuplicate,
    CapacityReached,
}

impl fmt::Display for VerificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for VerificationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Only a future reviewed observer may construct these findings.
enum ObservationFinding {
    Satisfied,
    Failed,
    Partial,
}

/// Fields and constructor are private. A future reviewed native observer can
/// construct these within this crate; callers cannot promote model output,
/// launch receipts, or user feedback into trusted machine evidence.
#[derive(Clone, Copy, Debug)]
pub struct TrustedObservation {
    id: u64,
    anchor: ExecutionAnchor,
    observed_at: Instant,
    finding: ObservationFinding,
}

#[derive(Clone, Copy, Debug)]
struct RecordedObservation {
    id: u64,
    observed_at: Instant,
    received_at: Instant,
    finding: ObservationFinding,
}

/// A user report and its supervisor-supplied receipt time, retained separately
/// from machine evidence.
#[derive(Clone, Copy, Debug)]
pub struct RecordedFeedback {
    pub report: UserFeedback,
    pub received_at: Instant,
}

/// One bounded execution ledger. An observation contract's target and expected
/// state will be fixed by a future reviewed observer, not by arbitrary text.
#[derive(Debug)]
pub struct VerificationCase {
    anchor: ExecutionAnchor,
    created_at: Instant,
    deadline_at: Instant,
    dispatch: Option<(DispatchState, Instant)>,
    observations: Vec<RecordedObservation>,
    feedback: Vec<RecordedFeedback>,
    outcome: MachineOutcome,
}

impl VerificationCase {
    pub fn new(
        anchor: ExecutionAnchor,
        created_at: Instant,
        deadline: Duration,
    ) -> Result<Self, VerificationError> {
        if deadline.is_zero() {
            return Err(VerificationError::InvalidDeadline);
        }
        let deadline_at = created_at
            .checked_add(deadline)
            .ok_or(VerificationError::InvalidDeadline)?;
        Ok(Self {
            anchor,
            created_at,
            deadline_at,
            dispatch: None,
            observations: Vec::new(),
            feedback: Vec::new(),
            outcome: MachineOutcome::Pending,
        })
    }

    #[must_use]
    pub const fn anchor(&self) -> ExecutionAnchor {
        self.anchor
    }

    #[must_use]
    pub const fn machine_outcome(&self) -> MachineOutcome {
        self.outcome
    }

    #[must_use]
    pub fn feedback(&self) -> &[RecordedFeedback] {
        &self.feedback
    }

    pub fn record_dispatch(
        &mut self,
        anchor: ExecutionAnchor,
        state: DispatchState,
        at: Instant,
        current_generation: ProfileGeneration,
    ) -> Result<(), VerificationError> {
        self.check_anchor(anchor, current_generation)?;
        if self.dispatch.is_some() {
            return Err(VerificationError::DispatchAlreadyRecorded);
        }
        if self.outcome != MachineOutcome::Pending {
            return Err(VerificationError::Finalized);
        }
        if at < self.created_at || at > self.deadline_at {
            return Err(VerificationError::DispatchOutsideWindow);
        }
        self.dispatch = Some((state, at));
        self.outcome = match state {
            DispatchState::Accepted => MachineOutcome::Pending,
            DispatchState::Rejected => MachineOutcome::Blocked,
        };
        Ok(())
    }

    /// Only a trusted observer inside this crate can construct the argument.
    pub fn record_observation(
        &mut self,
        observation: TrustedObservation,
        received_at: Instant,
        current_generation: ProfileGeneration,
    ) -> Result<RecordResult, VerificationError> {
        self.check_anchor(observation.anchor, current_generation)?;
        let Some((dispatch, dispatched_at)) = self.dispatch else {
            return Err(VerificationError::DispatchMissing);
        };
        if dispatch == DispatchState::Rejected {
            return Err(VerificationError::DispatchMissing);
        }
        if observation.observed_at > received_at {
            return Err(VerificationError::ObservationFromFuture);
        }
        if observation.observed_at < dispatched_at || received_at < dispatched_at {
            return Err(VerificationError::ObservationBeforeDispatch);
        }
        if observation.observed_at > self.deadline_at || received_at > self.deadline_at {
            return Err(VerificationError::ObservationAfterDeadline);
        }
        if let Some(existing) = self
            .observations
            .iter()
            .find(|item| item.id == observation.id)
        {
            return if existing.observed_at == observation.observed_at
                && existing.finding == observation.finding
            {
                Ok(RecordResult::AlreadyRecorded)
            } else {
                Err(VerificationError::ConflictingDuplicate)
            };
        }
        if matches!(
            self.outcome,
            MachineOutcome::VerifiedSuccess
                | MachineOutcome::VerifiedFailure
                | MachineOutcome::Unknown
                | MachineOutcome::Canceled
                | MachineOutcome::Blocked
        ) {
            return Err(VerificationError::Finalized);
        }
        if self.observations.last().is_some_and(|previous| {
            observation.observed_at <= previous.observed_at || received_at < previous.received_at
        }) {
            return Err(VerificationError::ObservationOutOfOrder);
        }
        if self.observations.len() == MAX_EVIDENCE {
            return Err(VerificationError::CapacityReached);
        }
        self.observations.push(RecordedObservation {
            id: observation.id,
            observed_at: observation.observed_at,
            received_at,
            finding: observation.finding,
        });
        self.outcome = match observation.finding {
            ObservationFinding::Satisfied => MachineOutcome::VerifiedSuccess,
            ObservationFinding::Failed => MachineOutcome::VerifiedFailure,
            ObservationFinding::Partial => MachineOutcome::Partial,
        };
        Ok(RecordResult::Recorded)
    }

    pub fn record_feedback(
        &mut self,
        feedback: UserFeedback,
        received_at: Instant,
        current_generation: ProfileGeneration,
    ) -> Result<RecordResult, VerificationError> {
        self.check_anchor(feedback.anchor, current_generation)?;
        let Some((_, dispatched_at)) = self.dispatch else {
            return Err(VerificationError::DispatchMissing);
        };
        if !received_at
            .checked_duration_since(dispatched_at)
            .is_some_and(|age| age <= FEEDBACK_WINDOW)
            || self.outcome == MachineOutcome::Canceled
        {
            return Err(VerificationError::FeedbackOutsideWindow);
        }
        if let Some(existing) = self
            .feedback
            .iter()
            .find(|item| item.report.id == feedback.id)
        {
            return if existing.report == feedback {
                Ok(RecordResult::AlreadyRecorded)
            } else {
                Err(VerificationError::ConflictingDuplicate)
            };
        }
        if self.feedback.len() == MAX_FEEDBACK {
            return Err(VerificationError::CapacityReached);
        }
        self.feedback.push(RecordedFeedback {
            report: feedback,
            received_at,
        });
        Ok(RecordResult::Recorded)
    }

    /// Pending means no trustworthy outcome is available yet; after the
    /// deadline, expiration is explicitly unknown rather than success.
    pub fn expire(&mut self, now: Instant) {
        if now > self.deadline_at && self.outcome == MachineOutcome::Pending {
            self.outcome = MachineOutcome::Unknown;
        }
    }

    /// A policy-generation change invalidates pending work.
    pub fn cancel_for_generation_change(&mut self, current_generation: ProfileGeneration) {
        if current_generation != self.anchor.request.generation
            && matches!(
                self.outcome,
                MachineOutcome::Pending | MachineOutcome::Partial
            )
        {
            self.outcome = MachineOutcome::Canceled;
        }
    }

    fn check_anchor(
        &self,
        anchor: ExecutionAnchor,
        current_generation: ProfileGeneration,
    ) -> Result<(), VerificationError> {
        if anchor != self.anchor {
            return Err(VerificationError::WrongExecution);
        }
        if current_generation != self.anchor.request.generation {
            return Err(VerificationError::StaleGeneration);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jarvis_core::RequestId;

    fn fixture() -> (VerificationCase, Instant, ExecutionAnchor) {
        let now = Instant::now();
        let request = RequestContext::new(RequestId::next(), ProfileGeneration::initial());
        let anchor = ExecutionAnchor::new(request, ExecutionId::next());
        let case = VerificationCase::new(anchor, now, Duration::from_secs(5)).unwrap();
        (case, now, anchor)
    }

    fn evidence(
        id: u64,
        anchor: ExecutionAnchor,
        at: Instant,
        finding: ObservationFinding,
    ) -> TrustedObservation {
        TrustedObservation {
            id,
            anchor,
            observed_at: at,
            finding,
        }
    }

    #[test]
    fn accepted_launch_and_positive_feedback_do_not_verify_machine_success() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        case.record_feedback(
            UserFeedback {
                id: 1,
                anchor,
                kind: FeedbackKind::Worked,
            },
            now,
            anchor.request.generation,
        )
        .unwrap();
        assert_eq!(case.machine_outcome(), MachineOutcome::Pending);
        case.expire(now + Duration::from_secs(6));
        assert_eq!(case.machine_outcome(), MachineOutcome::Unknown);
    }

    #[test]
    fn only_fresh_exactly_anchored_trusted_observation_verifies() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let observed = evidence(
            1,
            anchor,
            now + Duration::from_secs(1),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(observed, observed.observed_at, anchor.request.generation),
            Ok(RecordResult::Recorded)
        );
        assert_eq!(
            case.record_observation(observed, observed.observed_at, anchor.request.generation),
            Ok(RecordResult::AlreadyRecorded)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::VerifiedSuccess);
    }

    #[test]
    fn stale_generation_wrong_execution_and_late_observation_are_rejected() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let observed = evidence(
            1,
            anchor,
            now + Duration::from_secs(1),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(
                observed,
                observed.observed_at,
                anchor.request.generation.next()
            ),
            Err(VerificationError::StaleGeneration)
        );
        let wrong = evidence(
            2,
            ExecutionAnchor::new(anchor.request, ExecutionId::next()),
            now + Duration::from_secs(1),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(wrong, wrong.observed_at, anchor.request.generation),
            Err(VerificationError::WrongExecution)
        );
        let late = evidence(
            3,
            anchor,
            now + Duration::from_secs(6),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(late, late.observed_at, anchor.request.generation),
            Err(VerificationError::ObservationAfterDeadline)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::Pending);
    }

    #[test]
    fn pre_dispatch_and_conflicting_duplicate_evidence_cannot_change_outcome() {
        let (mut case, now, anchor) = fixture();
        let before_dispatch = evidence(1, anchor, now, ObservationFinding::Satisfied);
        assert_eq!(
            case.record_observation(before_dispatch, now, anchor.request.generation),
            Err(VerificationError::DispatchMissing)
        );
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now + Duration::from_secs(1),
            anchor.request.generation,
        )
        .unwrap();
        assert_eq!(
            case.record_observation(before_dispatch, now, anchor.request.generation),
            Err(VerificationError::ObservationBeforeDispatch)
        );
        let valid = evidence(
            1,
            anchor,
            now + Duration::from_secs(2),
            ObservationFinding::Partial,
        );
        case.record_observation(valid, valid.observed_at, anchor.request.generation)
            .unwrap();
        assert_eq!(
            case.record_observation(
                evidence(
                    1,
                    anchor,
                    now + Duration::from_secs(2),
                    ObservationFinding::Satisfied
                ),
                now + Duration::from_secs(2),
                anchor.request.generation
            ),
            Err(VerificationError::ConflictingDuplicate)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::Partial);
    }

    #[test]
    fn partial_and_failed_results_are_distinct_from_dispatch_and_feedback() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        case.record_observation(
            evidence(
                1,
                anchor,
                now + Duration::from_secs(1),
                ObservationFinding::Partial,
            ),
            now + Duration::from_secs(1),
            anchor.request.generation,
        )
        .unwrap();
        assert_eq!(case.machine_outcome(), MachineOutcome::Partial);
        case.record_observation(
            evidence(
                2,
                anchor,
                now + Duration::from_secs(2),
                ObservationFinding::Failed,
            ),
            now + Duration::from_secs(2),
            anchor.request.generation,
        )
        .unwrap();
        assert_eq!(case.machine_outcome(), MachineOutcome::VerifiedFailure);
    }

    #[test]
    fn feedback_is_anchored_and_idempotent_but_cannot_change_machine_outcome() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let good = UserFeedback {
            id: 42,
            anchor,
            kind: FeedbackKind::Worked,
        };
        assert_eq!(
            case.record_feedback(good, now, anchor.request.generation),
            Ok(RecordResult::Recorded)
        );
        assert_eq!(
            case.record_feedback(good, now, anchor.request.generation),
            Ok(RecordResult::AlreadyRecorded)
        );
        assert_eq!(
            case.record_feedback(
                UserFeedback {
                    kind: FeedbackKind::Failed,
                    ..good
                },
                now,
                anchor.request.generation
            ),
            Err(VerificationError::ConflictingDuplicate)
        );
        assert_eq!(
            case.record_feedback(
                UserFeedback {
                    anchor: ExecutionAnchor::new(anchor.request, ExecutionId::next()),
                    ..good
                },
                now,
                anchor.request.generation
            ),
            Err(VerificationError::WrongExecution)
        );
        assert_eq!(case.feedback().len(), 1);
        assert_eq!(case.machine_outcome(), MachineOutcome::Pending);
    }

    #[test]
    fn rejected_dispatch_is_blocked_and_generation_change_cancels_pending() {
        let (mut rejected, now, anchor) = fixture();
        rejected
            .record_dispatch(
                anchor,
                DispatchState::Rejected,
                now,
                anchor.request.generation,
            )
            .unwrap();
        assert_eq!(rejected.machine_outcome(), MachineOutcome::Blocked);
        assert_eq!(
            rejected.record_observation(
                evidence(1, anchor, now, ObservationFinding::Satisfied),
                now,
                anchor.request.generation
            ),
            Err(VerificationError::DispatchMissing)
        );
        let (mut pending, _, anchor) = fixture();
        pending.cancel_for_generation_change(anchor.request.generation.next());
        assert_eq!(pending.machine_outcome(), MachineOutcome::Canceled);
    }

    #[test]
    fn late_or_stale_feedback_is_rejected_without_machine_verification() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let feedback = UserFeedback {
            id: 10,
            anchor,
            kind: FeedbackKind::Worked,
        };
        assert_eq!(
            case.record_feedback(feedback, now, anchor.request.generation.next()),
            Err(VerificationError::StaleGeneration)
        );
        assert_eq!(
            case.record_feedback(
                feedback,
                now + FEEDBACK_WINDOW + Duration::from_secs(1),
                anchor.request.generation
            ),
            Err(VerificationError::FeedbackOutsideWindow)
        );
        assert_eq!(
            case.record_feedback(
                feedback,
                now - Duration::from_secs(1),
                anchor.request.generation
            ),
            Err(VerificationError::FeedbackOutsideWindow)
        );
        assert!(case.feedback().is_empty());
        assert_eq!(case.machine_outcome(), MachineOutcome::Pending);
    }

    #[test]
    fn backdated_or_out_of_order_observation_cannot_rewrite_result() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let partial = evidence(
            1,
            anchor,
            now + Duration::from_secs(2),
            ObservationFinding::Partial,
        );
        case.record_observation(
            partial,
            now + Duration::from_secs(3),
            anchor.request.generation,
        )
        .unwrap();
        assert_eq!(
            case.record_observation(
                evidence(
                    2,
                    anchor,
                    now + Duration::from_secs(1),
                    ObservationFinding::Satisfied,
                ),
                now + Duration::from_secs(4),
                anchor.request.generation,
            ),
            Err(VerificationError::ObservationOutOfOrder)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::Partial);
        let failure = evidence(
            3,
            anchor,
            now + Duration::from_secs(4),
            ObservationFinding::Failed,
        );
        case.record_observation(failure, failure.observed_at, anchor.request.generation)
            .unwrap();
        assert_eq!(
            case.record_observation(
                evidence(
                    4,
                    anchor,
                    now + Duration::from_secs(5),
                    ObservationFinding::Satisfied,
                ),
                now + Duration::from_secs(5),
                anchor.request.generation,
            ),
            Err(VerificationError::Finalized)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::VerifiedFailure);
    }

    #[test]
    fn expired_unknown_cannot_become_success_from_backdated_evidence() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        case.expire(now + Duration::from_secs(6));
        assert_eq!(case.machine_outcome(), MachineOutcome::Unknown);
        let backdated = evidence(
            1,
            anchor,
            now + Duration::from_secs(1),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(
                backdated,
                now + Duration::from_secs(2),
                anchor.request.generation
            ),
            Err(VerificationError::Finalized)
        );
        assert_eq!(
            case.record_observation(
                backdated,
                now + Duration::from_secs(6),
                anchor.request.generation
            ),
            Err(VerificationError::ObservationAfterDeadline)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::Unknown);
    }

    #[test]
    fn observation_timestamp_must_not_be_ahead_of_receipt() {
        let (mut case, now, anchor) = fixture();
        case.record_dispatch(
            anchor,
            DispatchState::Accepted,
            now,
            anchor.request.generation,
        )
        .unwrap();
        let future = evidence(
            1,
            anchor,
            now + Duration::from_secs(2),
            ObservationFinding::Satisfied,
        );
        assert_eq!(
            case.record_observation(
                future,
                now + Duration::from_secs(1),
                anchor.request.generation
            ),
            Err(VerificationError::ObservationFromFuture)
        );
        assert_eq!(case.machine_outcome(), MachineOutcome::Pending);
    }
}
