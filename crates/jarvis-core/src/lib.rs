#![forbid(unsafe_code)]

//! Small trusted types shared by the Jarvis supervisor and workers.
//! These types do not authorize Windows actions by themselves.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable identifier allocated by the supervisor for one request.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RequestId(u64);

impl RequestId {
    #[must_use]
    pub fn next() -> Self {
        Self(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "req-{}", self.0)
    }
}

/// Monotonically increasing epoch for policy-sensitive work.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProfileGeneration(u64);

impl ProfileGeneration {
    #[must_use]
    pub const fn initial() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Context bound to every action or worker-start request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestContext {
    pub request_id: RequestId,
    pub generation: ProfileGeneration,
}

impl RequestContext {
    #[must_use]
    pub const fn new(request_id: RequestId, generation: ProfileGeneration) -> Self {
        Self {
            request_id,
            generation,
        }
    }
}

/// Cooperative cancellation signal for owned work. It has no authority to
/// reopen policy gates or execute an action.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generations_are_monotonic() {
        let initial = ProfileGeneration::initial();
        assert!(initial.next() > initial);
    }

    #[test]
    fn cancellation_is_shared_between_clones() {
        let token = CancellationToken::default();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }
}
