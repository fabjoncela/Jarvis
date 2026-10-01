#![forbid(unsafe_code)]

//! Windows integration boundary.
//!
//! The in-memory lifecycle adapter supports deterministic policy tests. The
//! `WindowsJobObjectProbe` checks the reviewed Win32 containment primitive,
//! but is not a worker lifecycle or a resident supervisor. It deliberately
//! cannot launch or adopt any process.

use jarvis_core::ProfileGeneration;
use jarvis_windows_ffi::{KillOnCloseJob, Win32Error};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerId(String);

impl WorkerId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerKind {
    Session,
    Voice,
    Stt,
    Llm,
    Tts,
    Learning,
    Embedding,
    Build,
    Sandbox,
    Extension,
    Broker,
    Script,
    Ui,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerState {
    Running,
    Stopped,
    Unresponsive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedWorker {
    pub id: WorkerId,
    pub kind: WorkerKind,
    pub state: WorkerState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownReport {
    pub generation: ProfileGeneration,
    pub remaining_workers: Vec<WorkerId>,
}

impl ShutdownReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.remaining_workers.is_empty()
    }
}

pub trait WorkerLifecycle {
    fn shutdown_owned_workers(&mut self, generation: ProfileGeneration) -> ShutdownReport;
    fn owned_workers(&self) -> &[OwnedWorker];
}

/// A controlled test double. An unresponsive worker remains visible in the
/// shutdown report, which lets callers test restricted-fault behavior without
/// killing a process.
#[derive(Debug, Default)]
pub struct InMemoryWorkerLifecycle {
    workers: Vec<OwnedWorker>,
}

impl InMemoryWorkerLifecycle {
    #[must_use]
    pub fn with_running(workers: impl IntoIterator<Item = (WorkerId, WorkerKind)>) -> Self {
        Self {
            workers: workers
                .into_iter()
                .map(|(id, kind)| OwnedWorker {
                    id,
                    kind,
                    state: WorkerState::Running,
                })
                .collect(),
        }
    }

    pub fn mark_unresponsive(&mut self, id: &str) {
        if let Some(worker) = self
            .workers
            .iter_mut()
            .find(|worker| worker.id.as_str() == id)
        {
            worker.state = WorkerState::Unresponsive;
        }
    }
}

impl WorkerLifecycle for InMemoryWorkerLifecycle {
    fn shutdown_owned_workers(&mut self, generation: ProfileGeneration) -> ShutdownReport {
        for worker in &mut self.workers {
            if worker.state == WorkerState::Running {
                worker.state = WorkerState::Stopped;
            }
        }
        ShutdownReport {
            generation,
            remaining_workers: self
                .workers
                .iter()
                .filter(|worker| worker.state != WorkerState::Stopped)
                .map(|worker| worker.id.clone())
                .collect(),
        }
    }

    fn owned_workers(&self) -> &[OwnedWorker] {
        &self.workers
    }
}

/// A short-lived capability probe for the reviewed Jarvis Job Object primitive.
///
/// The unnamed Job Object and its raw handle remain opaque. This type does not
/// track workers, participate in a competitive transition, or expose process
/// launch/assignment APIs. A future long-lived supervisor must implement the
/// fixed suspended-create, assign, verify, resume sequence separately before
/// any Jarvis worker is allowed to run.
#[derive(Debug)]
pub struct WindowsJobObjectProbe {
    job: KillOnCloseJob,
}

impl WindowsJobObjectProbe {
    pub fn create() -> Result<Self, Win32Error> {
        Ok(Self {
            job: KillOnCloseJob::create()?,
        })
    }

    pub fn active_process_count(&self) -> Result<u32, Win32Error> {
        self.job.active_process_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_reports_unresponsive_owned_workers() {
        let mut lifecycle =
            InMemoryWorkerLifecycle::with_running([(WorkerId::new("voice-1"), WorkerKind::Voice)]);
        lifecycle.mark_unresponsive("voice-1");
        let report = lifecycle.shutdown_owned_workers(ProfileGeneration::initial());
        assert_eq!(report.remaining_workers, vec![WorkerId::new("voice-1")]);
    }

    #[test]
    fn native_job_probe_verifies_an_empty_job() {
        let lifecycle = WindowsJobObjectProbe::create().expect("create empty Job Object");
        assert_eq!(
            lifecycle
                .active_process_count()
                .expect("query empty Job Object"),
            0
        );
    }
}
