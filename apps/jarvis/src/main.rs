#![forbid(unsafe_code)]

//! CLI for the Jarvis supervisor foundation and fixed pre-match handoff.
//!
//! `--serve-stdin` keeps one serialized supervisor alive for explicitly typed
//! status and route requests. `--monitor-competitive` is the opt-in fixed-scope
//! process/service poller; the pre-match command is the sole user-app launcher.

mod supervisor;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use jarvis_gaming::{
    pre_match_setup_status, DetectionHealth, FailClosedTriggerDetector, PreMatchSetupStatus,
    TriggerSnapshot, WindowsCompetitiveActivitySource, WindowsPreMatchSetupSource,
};
use jarvis_profiles::{Profile, TransitionCause, TransitionState};
use jarvis_windows::{
    send_supervisor_control_request, InternalWorker, SupervisorControlCommand,
    SupervisorControlPipeServer,
};
use supervisor::{ActivityReconciliation, RouteOutcome, Supervisor};

const MAX_CONSOLE_COMMAND_BYTES: usize = 4 * 1024;
const COMPETITIVE_POLL_INTERVAL: Duration = Duration::from_millis(500);

fn main() -> ExitCode {
    match config_directory().and_then(|directory| run(env::args().skip(1).collect(), &directory)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Jarvis: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(arguments: Vec<String>, configuration_directory: &Path) -> Result<(), String> {
    match arguments.as_slice() {
        [flag] if flag == "--prepare-faceit-cs2" => return prepare_faceit_cs2(),
        [flag] if flag == "--worker-self-check" => return worker_self_check_client(),
        [flag] if flag == "--supervisor-status" => return supervisor_status(),
        _ => {}
    }

    let mut supervisor =
        Supervisor::bootstrap(configuration_directory).map_err(|error| error.to_string())?;
    match arguments.as_slice() {
        [] => self_check(&supervisor),
        [flag] if flag == "--self-check" => self_check(&supervisor),
        [flag, command] if flag == "--route" => route(command, &mut supervisor),
        [flag] if flag == "--serve-stdin" => serve_stdin(&mut supervisor),
        [flag] if flag == "--competitive-self-check" => competitive_self_check(&mut supervisor),
        [flag] if flag == "--detection-self-check" => detection_self_check(&mut supervisor),
        [flag] if flag == "--monitor-competitive" => monitor_competitive(&mut supervisor),
        [flag] if flag == "--pre-match-setup-self-check" => pre_match_setup_self_check(),
        _ => Err(
            "usage: jarvis [--self-check | --route <command> | --serve-stdin | --worker-self-check | --competitive-self-check | --detection-self-check | --monitor-competitive | --supervisor-status | --pre-match-setup-self-check | --prepare-faceit-cs2]"
                .to_owned(),
        ),
    }
}

fn self_check(supervisor: &Supervisor) -> Result<(), String> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    write_self_check(supervisor, &mut output)
}

fn write_self_check<W: Write>(supervisor: &Supervisor, output: &mut W) -> Result<(), String> {
    let status = supervisor.status().map_err(|error| error.to_string())?;
    writeln!(output, "Jarvis V1 foundation self-check").map_err(io_error)?;
    writeln!(output, "configuration: validated").map_err(io_error)?;
    writeln!(output, "runtime supervisor: active for this process").map_err(io_error)?;
    writeln!(
        output,
        "fixed-identity activity monitoring: authoritative resident poller available through --monitor-competitive; --detection-self-check reconciles once"
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "machine-wide policy: this invocation is process-local; query the resident owner with --supervisor-status"
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "fixed-worker lifecycle: available only through --worker-self-check; general multi-worker lifecycle: not implemented"
    )
    .map_err(io_error)?;
    writeln!(output, "supervisor policy state: {:?}", status.profile).map_err(io_error)?;
    writeln!(output, "profile generation: {}", status.generation.value()).map_err(io_error)?;
    writeln!(output, "action gate: {}", status.action_gate_open).map_err(io_error)?;
    writeln!(
        output,
        "worker-start gate: {}",
        status.worker_start_gate_open
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "owned Job Object: empty; active processes: {}",
        status.active_owned_worker_processes
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "fixed internal worker manifest: {} entry; start available only through --worker-self-check",
        status.configured_internal_worker_count
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "FACEIT identity verification (not a certification): {:?}; use --pre-match-setup-self-check",
        status.faceit_setup_status,
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "execution: desktop actions remain disabled; no configured application launch was performed"
    )
    .map_err(io_error)?;
    Ok(())
}

fn route(command: &str, supervisor: &mut Supervisor) -> Result<(), String> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    write_route(command, supervisor, &mut output)
}

fn write_route<W: Write>(
    command: &str,
    supervisor: &mut Supervisor,
    output: &mut W,
) -> Result<(), String> {
    match supervisor
        .route(command)
        .map_err(|error| format!("plan rejected: {error:?}"))?
    {
        RouteOutcome::Validated(validated) => {
            writeln!(output, "request: {}", validated.request().request_id).map_err(io_error)?;
            writeln!(output, "validated actions: {:?}", validated.actions()).map_err(io_error)?;
            writeln!(
                output,
                "confirmation required: {}",
                validated.requires_confirmation()
            )
            .map_err(io_error)?;
            writeln!(
                output,
                "runtime: validation-only; this invocation did not run the authoritative monitor, and no configured desktop action was performed"
            )
            .map_err(io_error)?;
            Ok(())
        }
        RouteOutcome::InvalidPattern(reason) => Err(reason),
        RouteOutcome::Unknown => {
            Err("no deterministic route; model fallback is not implemented".to_owned())
        }
    }
}

fn serve_stdin(supervisor: &mut Supervisor) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    serve(supervisor, &mut input, &mut output)
}

/// Runs the intentionally small, explicit console protocol for a resident
/// supervisor process. It never accepts shell input, file paths, PIDs, or an
/// action-execution command.
fn serve<R: BufRead, W: Write>(
    supervisor: &mut Supervisor,
    input: &mut R,
    output: &mut W,
) -> Result<(), String> {
    writeln!(output, "Jarvis validation supervisor stdin console").map_err(io_error)?;
    writeln!(output, "commands: status | route <command> | quit").map_err(io_error)?;

    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = input.read_line(&mut line).map_err(io_error)?;
        if bytes_read == 0 {
            return Ok(());
        }
        if line.len() > MAX_CONSOLE_COMMAND_BYTES {
            writeln!(output, "rejected: console command exceeds 4096 bytes").map_err(io_error)?;
            continue;
        }

        let command = line.trim();
        if command.is_empty() {
            continue;
        }
        if command == "quit" {
            writeln!(output, "supervisor console stopped").map_err(io_error)?;
            return Ok(());
        }
        if command == "status" {
            write_self_check(supervisor, output)?;
            continue;
        }
        if let Some(request) = command.strip_prefix("route ") {
            write_route(request, supervisor, output)?;
            continue;
        }
        writeln!(
            output,
            "rejected: supported commands are status, route <command>, or quit"
        )
        .map_err(io_error)?;
    }
}

fn io_error(error: io::Error) -> String {
    format!("console I/O failed: {error}")
}

fn worker_self_check_client() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::WorkerSelfCheck)
        .map_err(|error| {
            format!(
                "authoritative supervisor unavailable; start --monitor-competitive first ({error})"
            )
        })?;
    if response.starts_with("OK WORKER_SELF_CHECK") {
        println!("Jarvis authoritative worker containment self-check");
        println!("{response}");
        println!("worker start and shutdown were serialized by the resident policy owner");
        Ok(())
    } else if response == "ERR POLICY" {
        Err("worker self-check denied by the authoritative Competitive Mode policy".to_owned())
    } else {
        Err("authoritative supervisor could not complete the worker self-check".to_owned())
    }
}

fn checked_worker_cycle(
    supervisor: &mut Supervisor,
    installation_directory: &Path,
) -> Result<jarvis_windows::WorkerProcessIdentity, String> {
    let identity = supervisor
        .start_internal_worker(InternalWorker::Session, installation_directory)
        .map_err(|error| format!("worker self-check failed: {error}"))?;
    let shutdown = supervisor
        .shutdown_internal_workers()
        .map_err(|error| format!("worker self-check shutdown failed: {error}"))?;
    if shutdown.used_forced_termination || !shutdown.all_workers_exited_successfully {
        return Err("worker self-check did not observe a clean cooperative worker exit".to_owned());
    }
    Ok(identity)
}

/// Exercises the serialized Competitive Mode gate and real owned-worker
/// manager without observing a game, FACEIT, service, or desktop application.
fn competitive_self_check(supervisor: &mut Supervisor) -> Result<(), String> {
    let result = supervisor
        .enter_competitive(TransitionCause::Manual)
        .map_err(|error| format!("competitive self-check failed: {error}"))?;
    if !result.clean_shutdown || result.used_forced_termination {
        return Err(
            "competitive self-check did not verify a clean owned-worker shutdown".to_owned(),
        );
    }
    let status = supervisor.status().map_err(|error| error.to_string())?;
    println!("Jarvis Competitive Mode containment self-check");
    println!("profile: {:?}", status.profile);
    println!("transition: {:?}", status.transition);
    println!("action gate: {}", status.action_gate_open);
    println!("worker-start gate: {}", status.worker_start_gate_open);
    println!(
        "owned Job Object: verified empty; active processes: {}",
        status.active_owned_worker_processes
    );
    println!("execution: no game, FACEIT component, service, or desktop application was observed or changed");
    Ok(())
}

/// Reconciles one fixed-identity process/service snapshot and exits. It may
/// close Jarvis's own gates and stop only workers owned by this supervisor.
fn detection_self_check(supervisor: &mut Supervisor) -> Result<(), String> {
    let mut detector = FailClosedTriggerDetector::new(WindowsCompetitiveActivitySource::new());
    let observation = detector.observe();
    let action = supervisor
        .reconcile_activity(observation)
        .map_err(|error| format!("detection self-check containment failed: {error}"))?;
    let status = supervisor.status().map_err(|error| error.to_string())?;
    println!("Jarvis trigger-detection self-check");
    println!("cs2 trigger active: {}", observation.snapshot.cs2_running);
    println!(
        "FACEIT Anti-Cheat launcher active: {}",
        observation.snapshot.verified_faceit_process_running
    );
    println!(
        "FACEIT service active: {}",
        observation.snapshot.verified_faceit_service_active
    );
    println!("detection health: {:?}", observation.health);
    println!("reconciliation: {action:?}");
    println!("profile after fail-closed evaluation: {:?}", status.profile);
    println!("worker-start gate: {}", status.worker_start_gate_open);
    println!("execution: no process, service, game, FACEIT component, or desktop application was modified");
    Ok(())
}

/// Keeps the supervisor alive, reconciling fixed process/service snapshots at
/// startup and every 500 ms until the user closes the console with Ctrl+C. It
/// owns the exclusive same-user control pipe used by launch/worker clients.
fn monitor_competitive(supervisor: &mut Supervisor) -> Result<(), String> {
    let mut detector = FailClosedTriggerDetector::new(WindowsCompetitiveActivitySource::new());
    let mut last_report = None;
    println!("Jarvis authoritative competitive supervisor; press Ctrl+C to stop.");
    monitor_cycle(supervisor, &mut detector, &mut last_report)?;

    let (event_sender, event_receiver) = mpsc::sync_channel(1);
    start_supervisor_control_pipe(event_sender)?;
    println!("authoritative same-user control pipe: ready");

    let mut next_poll = Instant::now() + COMPETITIVE_POLL_INTERVAL;
    loop {
        let timeout = next_poll.saturating_duration_since(Instant::now());
        match event_receiver.recv_timeout(timeout) {
            Ok(MonitorControlEvent::Request { command, reply }) => {
                let response =
                    handle_control_request(command, supervisor, &mut detector, &mut last_report)?;
                let _ = reply.send(response);
                next_poll = Instant::now() + COMPETITIVE_POLL_INTERVAL;
            }
            Ok(MonitorControlEvent::Failed(reason)) => {
                let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
                return Err(format!(
                    "authoritative control pipe failed; policy was forced closed: {reason}"
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let cycle_started = Instant::now();
                monitor_cycle(supervisor, &mut detector, &mut last_report)?;
                next_poll = cycle_started + COMPETITIVE_POLL_INTERVAL;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
                return Err(
                    "authoritative control pipe stopped; policy was forced closed".to_owned(),
                );
            }
        }
    }
}

#[derive(Debug)]
enum MonitorControlEvent {
    Request {
        command: SupervisorControlCommand,
        reply: SyncSender<String>,
    },
    Failed(String),
}

fn start_supervisor_control_pipe(
    event_sender: SyncSender<MonitorControlEvent>,
) -> Result<(), String> {
    let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("jarvis-supervisor-pipe".to_owned())
        .spawn(move || {
            let mut server = match SupervisorControlPipeServer::bind() {
                Ok(server) => {
                    let _ = ready_sender.send(Ok(()));
                    server
                }
                Err(error) => {
                    let _ = ready_sender.send(Err(error.to_string()));
                    return;
                }
            };

            loop {
                let serving = server.serve_one(|command| {
                    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
                    if event_sender
                        .send(MonitorControlEvent::Request {
                            command,
                            reply: reply_sender,
                        })
                        .is_err()
                    {
                        return "ERR SUPERVISOR_UNAVAILABLE".to_owned();
                    }
                    reply_receiver
                        .recv()
                        .unwrap_or_else(|_| "ERR SUPERVISOR_UNAVAILABLE".to_owned())
                });
                if let Err(error) = serving {
                    let _ = event_sender.send(MonitorControlEvent::Failed(error.to_string()));
                    return;
                }
            }
        })
        .map_err(|error| format!("could not start supervisor control thread: {error}"))?;

    ready_receiver
        .recv()
        .map_err(|_| "supervisor control thread stopped during startup".to_owned())?
}

fn monitor_cycle(
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<WindowsCompetitiveActivitySource>,
    last_report: &mut Option<(
        TriggerSnapshot,
        DetectionHealth,
        Profile,
        TransitionState,
        ActivityReconciliation,
    )>,
) -> Result<jarvis_gaming::TriggerObservation, String> {
    let observation = detector.observe();
    let action = match supervisor.reconcile_activity(observation) {
        Ok(action) => action,
        Err(error) => {
            let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
            return Err(format!("competitive monitor containment failed: {error}"));
        }
    };
    let status = match supervisor.status() {
        Ok(status) => status,
        Err(error) => {
            let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
            return Err(format!("competitive monitor status unavailable: {error}"));
        }
    };
    let report = (
        observation.snapshot,
        observation.health,
        status.profile,
        status.transition,
        action,
    );
    if *last_report != Some(report) {
        println!(
            "profile: {:?} ({:?}); detection: {:?}; CS2: {}; FACEIT AC launcher: {}; FACEIT service: {}; action: {:?}",
            status.profile,
            status.transition,
            observation.health,
            observation.snapshot.cs2_running,
            observation.snapshot.verified_faceit_process_running,
            observation.snapshot.verified_faceit_service_active,
            action,
        );
        *last_report = Some(report);
    }
    Ok(observation)
}

fn handle_control_request(
    command: SupervisorControlCommand,
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<WindowsCompetitiveActivitySource>,
    last_report: &mut Option<(
        TriggerSnapshot,
        DetectionHealth,
        Profile,
        TransitionState,
        ActivityReconciliation,
    )>,
) -> Result<String, String> {
    let observation = monitor_cycle(supervisor, detector, last_report)?;
    let status = supervisor.status().map_err(|error| error.to_string())?;
    match command {
        SupervisorControlCommand::Status => Ok(format!(
            "OK STATUS PROFILE={:?} TRANSITION={:?} GENERATION={} ACTION_GATE={} WORKER_GATE={} DETECTION={:?} CS2={} FACEIT_AC={} FACEIT_SERVICE={}",
            status.profile,
            status.transition,
            status.generation.value(),
            status.action_gate_open,
            status.worker_start_gate_open,
            observation.health,
            observation.snapshot.cs2_running,
            observation.snapshot.verified_faceit_process_running,
            observation.snapshot.verified_faceit_service_active,
        )),
        SupervisorControlCommand::PrepareFaceitCs2 => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            let report = match supervisor.prepare_faceit_cs2(true) {
                Ok(report) => report,
                Err(error) => {
                    eprintln!("authoritative pre-match handoff denied or incomplete: {error}");
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    return Ok("ERR HANDOFF".to_owned());
                }
            };
            let after = monitor_cycle(supervisor, detector, last_report)?;
            let final_status = supervisor.status().map_err(|error| error.to_string())?;
            Ok(format!(
                "OK HANDOFF FACEIT={} CHROME={} STEAM={} PROFILE={:?} DETECTION={:?}",
                report.faceit_ac_launcher_started,
                report.chrome_faceit_url_requested,
                report.steam_cs2_launch_requested,
                final_status.profile,
                after.health,
            ))
        }
        SupervisorControlCommand::WorkerSelfCheck => {
            if !status.worker_start_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            let executable = env::current_exe().map_err(|error| error.to_string())?;
            let installation_directory = executable.parent().ok_or_else(|| {
                "could not determine the supervisor executable directory".to_owned()
            })?;
            let worker_check = checked_worker_cycle(supervisor, installation_directory).and_then(
                |first| {
                    let second = checked_worker_cycle(supervisor, installation_directory)?;
                    if first == second {
                        return Err("worker restart retained a stale process identity".to_owned());
                    }
                    Ok((first, second))
                },
            );
            match worker_check {
                Ok((first, second)) => {
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok(format!(
                        "OK WORKER_SELF_CHECK FIRST_PID={} SECOND_PID={}",
                        first.process_id(),
                        second.process_id(),
                    ))
                }
                Err(error) => {
                    eprintln!("authoritative worker containment self-check failed: {error}");
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok("ERR WORKER_SELF_CHECK".to_owned())
                }
            }
        }
    }
}

/// Performs one native read-only verification of the fixed FACEIT, Chrome,
/// Steam, and CS2 handoff identities. It never starts an app or service and a
/// later executor must perform this verification again immediately before use.
fn pre_match_setup_self_check() -> Result<(), String> {
    let mut source = WindowsPreMatchSetupSource;
    let status = pre_match_setup_status(&mut source).map_err(|error| error.to_string())?;
    if status != PreMatchSetupStatus::VerifiedForCurrentCheck {
        return Err("pre-match setup was not verified for this check".to_owned());
    }
    println!("Jarvis pre-match setup self-check");
    println!("fixed FACEIT Anti-Cheat Launcher and registered service: verified");
    println!("fixed Chrome and Steam installations: verified");
    println!("Steam CS2 app 730 manifest: verified");
    println!(
        "execution: no FACEIT service, application, browser, Steam game, or login was started"
    );
    Ok(())
}

fn supervisor_status() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::Status).map_err(
        |error| {
            format!(
                "authoritative supervisor unavailable; no resident machine-wide policy is active ({error})"
            )
        },
    )?;
    if !response.starts_with("OK STATUS ") {
        return Err("authoritative supervisor returned an invalid status response".to_owned());
    }
    println!("Jarvis authoritative supervisor status");
    println!("{response}");
    Ok(())
}

/// Requests the fixed FACEIT AC → Chrome → Steam sequence from the resident
/// policy owner only after exact interactive confirmation. Authentication is
/// left entirely to the user.
fn prepare_faceit_cs2() -> Result<(), String> {
    println!("This will start the FACEIT Anti-Cheat Launcher, open https://www.faceit.com/ in Chrome, and request CS2 (Steam app 730).");
    println!("Jarvis will not type credentials or start the FACEIT service directly.");
    print!("Type START to continue, or press Enter to cancel: ");
    io::stdout().flush().map_err(io_error)?;

    let mut response = String::new();
    io::stdin().read_line(&mut response).map_err(io_error)?;
    if response.trim() != "START" {
        println!("Pre-match sequence cancelled; nothing was launched.");
        return Ok(());
    }

    let response = send_supervisor_control_request(SupervisorControlCommand::PrepareFaceitCs2)
        .map_err(|error| {
            format!(
                "authoritative supervisor unavailable; start --monitor-competitive first ({error}); nothing was launched"
            )
        })?;
    if !response.starts_with("OK HANDOFF ") {
        return Err(match response.as_str() {
            "ERR POLICY" => "pre-match handoff denied by the authoritative Competitive Mode policy; nothing was launched".to_owned(),
            "ERR HANDOFF" => "authoritative supervisor could not complete the handoff; check its console for partial progress".to_owned(),
            _ => "authoritative supervisor returned an invalid handoff response; check its console before retrying".to_owned(),
        });
    }
    println!("{response}");
    println!("The resident supervisor performed the fixed handoff; Jarvis did not sign in or interact with CS2.");
    Ok(())
}

fn config_directory() -> Result<PathBuf, String> {
    let current_directory = env::current_dir().map_err(|error| error.to_string())?;
    current_directory
        .ancestors()
        .map(|ancestor| ancestor.join("config"))
        .find(|candidate| candidate.is_dir())
        .ok_or_else(|| {
            "could not find a config directory; run from the Jarvis repository root".to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
    }

    #[test]
    fn self_check_is_available_without_arguments() {
        assert_eq!(run(Vec::new(), &test_config_directory()), Ok(()));
    }

    #[test]
    fn known_route_does_not_execute() {
        assert_eq!(
            run(
                vec!["--route".to_owned(), "Jarvis, open Discord".to_owned()],
                &test_config_directory(),
            ),
            Ok(())
        );
    }

    #[test]
    fn stdin_supervisor_keeps_one_owner_for_multiple_validation_requests() {
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and Job Object should initialize");
        let mut input = io::Cursor::new(b"status\nroute Jarvis, open Discord\nquit\n");
        let mut output = Vec::new();

        assert_eq!(serve(&mut supervisor, &mut input, &mut output), Ok(()));

        let output = String::from_utf8(output).expect("console output is UTF-8");
        assert!(output.contains("Jarvis validation supervisor stdin console"));
        assert!(output.contains("validated actions: [LaunchApplication"));
        assert!(output.contains("supervisor console stopped"));
    }
}
