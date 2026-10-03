use std::path::Path;

use jarvis_windows::{InternalWorker, WindowsWorkerManager};

#[test]
fn fixed_model_shell_starts_in_owned_job_and_exits_cooperatively() {
    let executable = Path::new(env!("CARGO_BIN_EXE_jarvis-model-worker"));
    let installation_directory = executable.parent().expect("binary parent directory");
    let mut manager = WindowsWorkerManager::create().expect("create owned Job Object");

    for _ in 0..2 {
        let identity = manager
            .start(InternalWorker::Model, installation_directory)
            .expect("start only the fixed model shell");
        assert_eq!(identity.worker(), InternalWorker::Model);
        assert_eq!(manager.active_process_count().expect("query owned job"), 1);
        let report = manager.shutdown().expect("stop owned model shell");
        assert!(!report.used_forced_termination);
        assert!(report.all_workers_exited_successfully);
        assert_eq!(manager.active_process_count().expect("query empty job"), 0);
    }
}
