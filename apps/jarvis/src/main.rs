#![forbid(unsafe_code)]

//! CLI for the Jarvis supervisor foundation and fixed desktop launch paths.
//!
//! `--serve-stdin` keeps one serialized supervisor alive for explicitly typed
//! status and route requests. `--monitor-competitive` is the opt-in fixed-scope
//! process/service poller; the pre-match and Discord commands are the only
//! user-app launchers.

mod supervisor;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jarvis_actions::Action;
use jarvis_config::ValidatedConfiguration;
use jarvis_router::{RouteResult, Router};
use jarvis_storage::{EventId, PreferenceKey, ReferenceId};
use jarvis_verification::{FeedbackKind, RecordResult};

use jarvis_gaming::{
    pre_match_setup_status, DetectionHealth, FailClosedTriggerDetector, PreMatchSetupStatus,
    TriggerSnapshot, WindowsCompetitiveActivitySource, WindowsPreMatchSetupSource,
};
use jarvis_profiles::{Profile, TransitionCause, TransitionState};
use jarvis_windows::{
    observe_competitive_activity, send_supervisor_control_request, verify_default_pre_match_setup,
    CompetitiveLifecycleNotifications, DiscordFeedback, InternalWorker, PreferredApp,
    SupervisorControlCommand, SupervisorControlPipeServer,
};
use supervisor::{ActivityReconciliation, RouteOutcome, Supervisor};

const MAX_CONSOLE_COMMAND_BYTES: usize = 4 * 1024;
const MAX_EXECUTE_COMMAND_BYTES: usize = 256;
const COMPETITIVE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const CONTROL_EVENT_CHECK_INTERVAL: Duration = Duration::from_millis(50);

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
        [flag] if flag == "--supervisor-status" => return supervisor_status(false),
        [flag] if flag == "--last-handoff" => return last_handoff(),
        [flag] if flag == "--open-discord" => return open_discord(),
        [flag] if flag == "--discord-status" => return discord_status(),
        [flag] if flag == "--volume-status" => return volume_status(),
        [flag, percent] if flag == "--set-volume" => return set_volume_cli(percent),
        [flag, reference, kind] if flag == "--feedback-discord" => {
            return feedback_discord(reference, kind);
        }
        [flag, kind] if flag == "--feedback-last-discord" => {
            return feedback_last_discord(kind);
        }
        [flag, command] if flag == "--execute" => {
            return execute_text_command(command, configuration_directory);
        }
        [flag] if flag == "--show-preferred-app" => return show_preferred_app(),
        [flag] if flag == "--clear-preferred-app" => return clear_preferred_app(),
        [flag, app] if flag == "--remember-preferred-app" => {
            return remember_preferred_app(app);
        }
        [flag, diagnostics] if flag == "--supervisor-status" && diagnostics == "--diagnostics" => {
            return supervisor_status(true);
        }
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
        [flag] if flag == "--monitor-competitive" => monitor_competitive(&mut supervisor, false),
        [flag, diagnostics]
            if flag == "--monitor-competitive" && diagnostics == "--diagnostics" =>
        {
            monitor_competitive(&mut supervisor, true)
        }
        [flag] if flag == "--pre-match-setup-self-check" => pre_match_setup_self_check(),
        [flag] if flag == "--lifecycle-notification-self-check" => {
            lifecycle_notification_self_check()
        }
        _ => Err(
            "usage: jarvis [--self-check | --route <command> | --execute <command> | --serve-stdin | --worker-self-check | --competitive-self-check | --detection-self-check | --monitor-competitive [--diagnostics] | --supervisor-status [--diagnostics] | --last-handoff | --open-discord | --discord-status | --volume-status | --set-volume <0..100> | --feedback-discord <reference> <worked|failed> | --feedback-last-discord <worked|failed> | --pre-match-setup-self-check | --lifecycle-notification-self-check | --prepare-faceit-cs2 | --remember-preferred-app <chrome|discord> | --show-preferred-app | --clear-preferred-app]"
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
        "fixed-identity monitoring: --monitor-competitive uses lifecycle hints when available plus a 500 ms identity snapshot watchdog; --detection-self-check reconciles once"
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
        "execution: only the resident owner can run fixed pre-match or Discord launch commands; this self-check performed no application launch"
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

/// Keeps the supervisor alive, subscribing before startup reconciliation,
/// reconciling promptly on fixed lifecycle hints, and retaining a 500 ms
/// snapshot watchdog. It owns the exclusive same-user control pipe used by
/// launch/worker clients.
fn monitor_competitive(supervisor: &mut Supervisor, diagnostics: bool) -> Result<(), String> {
    let mut lifecycle_notifications = match verify_default_pre_match_setup() {
        Ok(setup) => match CompetitiveLifecycleNotifications::start(&setup) {
            Ok(notifications) => {
                println!("fixed process/service lifecycle subscriptions: ready");
                Some(notifications)
            }
            Err(error) => {
                eprintln!(
                    "fixed lifecycle subscriptions unavailable; authoritative 500 ms snapshot polling remains active: {error}"
                );
                None
            }
        },
        Err(error) => {
            eprintln!(
                "pre-match setup is currently unverified; event subscriptions are unavailable, and the fail-closed snapshot source will retry setup while keeping policy restricted: {error}"
            );
            None
        }
    };
    let mut detector = FailClosedTriggerDetector::new(WindowsCompetitiveActivitySource::new());
    let mut last_report = None;
    println!("Jarvis authoritative competitive supervisor; press Ctrl+C to stop.");
    let startup_cycle_started_at = Instant::now();
    monitor_cycle(supervisor, &mut detector, &mut last_report)?;
    if diagnostics {
        eprintln!(
            "JARVIS_METRIC kind=startup_reconciliation duration_us={}",
            startup_cycle_started_at.elapsed().as_micros()
        );
    }

    let (event_sender, event_receiver) = mpsc::sync_channel(1);
    if let Err(error) = start_supervisor_control_pipe(event_sender) {
        let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
        return Err(format!(
            "control pipe unavailable; policy was restricted: {error}"
        ));
    }
    println!("authoritative same-user control pipe: ready");

    let mut next_poll = Instant::now() + COMPETITIVE_POLL_INTERVAL;
    let mut last_reconciliation_started_at = Some(Instant::now());
    loop {
        match event_receiver.try_recv() {
            Ok(MonitorControlEvent::Request { command, reply }) => {
                let request_started_at = Instant::now();
                let response =
                    handle_control_request(command, supervisor, &mut detector, &mut last_report)?;
                if diagnostics {
                    eprintln!(
                        "JARVIS_METRIC kind=control_request command={command:?} handler_us={}",
                        request_started_at.elapsed().as_micros()
                    );
                }
                let _ = reply.send(response);
                last_reconciliation_started_at = Some(request_started_at);
                next_poll = Instant::now() + COMPETITIVE_POLL_INTERVAL;
                continue;
            }
            Ok(MonitorControlEvent::Failed(reason)) => {
                let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
                return Err(format!(
                    "authoritative control pipe failed; policy was forced closed: {reason}"
                ));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let _ = supervisor.enter_competitive(TransitionCause::DetectionUnhealthy);
                return Err(
                    "authoritative control pipe stopped; policy was forced closed".to_owned(),
                );
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        let wait_duration = monitor_wait_slice(next_poll, Instant::now());
        let notification_result = match lifecycle_notifications.as_ref() {
            Some(notifications) => notifications.wait_for_change(wait_duration),
            None => {
                thread::sleep(wait_duration);
                Ok(false)
            }
        };
        match notification_result {
            Ok(true) => {
                // Event contents are intentionally opaque: every hint is
                // reconciled by the existing complete identity-verifying
                // process/service snapshot path.
                let cycle_started = Instant::now();
                monitor_after_lifecycle_hint(supervisor, &mut detector, &mut last_report)?;
                if diagnostics {
                    eprintln!(
                        "JARVIS_METRIC kind=monitor_lifecycle_reconcile cycle_us={}",
                        cycle_started.elapsed().as_micros()
                    );
                }
                last_reconciliation_started_at = Some(cycle_started);
                // Do not move `next_poll`: event bursts cannot defer the
                // periodic watchdog snapshot.
            }
            Ok(false) if Instant::now() >= next_poll => {
                let cycle_started = Instant::now();
                let interval_us = last_reconciliation_started_at
                    .map(|previous| cycle_started.duration_since(previous).as_micros())
                    .unwrap_or_default();
                monitor_cycle(supervisor, &mut detector, &mut last_report)?;
                if diagnostics {
                    eprintln!(
                        "JARVIS_METRIC kind=monitor_poll interval_us={interval_us} cycle_us={}",
                        cycle_started.elapsed().as_micros()
                    );
                }
                last_reconciliation_started_at = Some(cycle_started);
                next_poll = cycle_started + COMPETITIVE_POLL_INTERVAL;
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!(
                    "fixed lifecycle notification source failed; dropping it and continuing with the authoritative 500 ms snapshot watchdog: {error}"
                );
                lifecycle_notifications = None;
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

fn monitor_cycle<S: jarvis_gaming::TriggerSnapshotSource>(
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<S>,
    last_report: &mut Option<(
        TriggerSnapshot,
        DetectionHealth,
        Profile,
        TransitionState,
        ActivityReconciliation,
    )>,
) -> Result<jarvis_gaming::TriggerObservation, String> {
    monitor_cycle_at(supervisor, detector, last_report, Instant::now())
}

/// A lifecycle event is only permission to take a fresh fixed snapshot; it
/// carries no process identity or trigger state into policy.
fn monitor_after_lifecycle_hint<S: jarvis_gaming::TriggerSnapshotSource>(
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<S>,
    last_report: &mut Option<(
        TriggerSnapshot,
        DetectionHealth,
        Profile,
        TransitionState,
        ActivityReconciliation,
    )>,
) -> Result<jarvis_gaming::TriggerObservation, String> {
    monitor_cycle(supervisor, detector, last_report)
}

fn monitor_wait_slice(next_poll: Instant, now: Instant) -> Duration {
    next_poll
        .saturating_duration_since(now)
        .min(CONTROL_EVENT_CHECK_INTERVAL)
}

fn monitor_cycle_at<S: jarvis_gaming::TriggerSnapshotSource>(
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<S>,
    last_report: &mut Option<(
        TriggerSnapshot,
        DetectionHealth,
        Profile,
        TransitionState,
        ActivityReconciliation,
    )>,
    now: Instant,
) -> Result<jarvis_gaming::TriggerObservation, String> {
    let observation = detector.observe();
    let action = match supervisor.reconcile_activity_at(observation, now) {
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

fn handle_control_request<S: jarvis_gaming::TriggerSnapshotSource>(
    command: SupervisorControlCommand,
    supervisor: &mut Supervisor,
    detector: &mut FailClosedTriggerDetector<S>,
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
        SupervisorControlCommand::LastHandoff => Ok(match supervisor.last_handoff_attempt() {
            None => "OK LAST_HANDOFF NONE".to_owned(),
            Some(attempt) => format!(
                "OK LAST_HANDOFF REQUEST={} GENERATION={} STATE={:?} FACEIT_LAUNCH_REQUESTED={} CHROME_URL_REQUESTED={} FACEIT_TITLE_MATCH={} STEAM_CS2_LAUNCH_REQUESTED={} OUTCOME=UNVERIFIED",
                attempt.request.request_id,
                attempt.request.generation.value(),
                attempt.state,
                attempt.report.faceit_ac_launcher_started,
                attempt.report.chrome_faceit_url_requested,
                attempt.report.chrome_faceit_title_match_detected,
                attempt.report.steam_cs2_launch_requested,
            ),
        }),
        SupervisorControlCommand::OpenDiscord => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            match supervisor.open_discord() {
                Ok((request, reference)) => {
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok(format!("OK DISCORD_LAUNCH_REQUESTED REQUEST={request} FEEDBACK_REF={reference:032x} OUTCOME=UNVERIFIED"))
                }
                Err(error) => {
                    eprintln!("authoritative Discord launch denied or failed: {error}");
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok("ERR DISCORD_LAUNCH".to_owned())
                }
            }
        }
        SupervisorControlCommand::DiscordStatus => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            match supervisor.discord_process_count() {
                Ok(count) => {
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok(format!("OK DISCORD_STATUS PROCESS_COUNT={count} READINESS=UNVERIFIED"))
                }
                Err(error) => {
                    eprintln!("Discord status observation unavailable: {error}");
                    let _ = monitor_cycle(supervisor, detector, last_report)?;
                    Ok("ERR DISCORD_STATUS_UNKNOWN".to_owned())
                }
            }
        }
        SupervisorControlCommand::SetMasterVolume { percent } => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            match supervisor.set_master_volume(percent) {
                Ok((request, observed)) => Ok(format!(
                    "OK MASTER_VOLUME REQUEST={request} TARGET={percent} OBSERVED_SCALAR={observed:.4}"
                )),
                Err(error) => {
                    eprintln!("master volume request denied or failed: {error}");
                    Ok("ERR MASTER_VOLUME".to_owned())
                }
            }
        }
        SupervisorControlCommand::MasterVolumeStatus => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            match supervisor.master_volume_status() {
                Ok(observed) => Ok(format!(
                    "OK MASTER_VOLUME_STATUS OBSERVED_SCALAR={observed:.4}"
                )),
                Err(error) => {
                    eprintln!("master volume observation failed: {error}");
                    Ok("ERR MASTER_VOLUME_STATUS".to_owned())
                }
            }
        }
        SupervisorControlCommand::FeedbackDiscord { reference, kind } => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            let feedback = match kind {
                DiscordFeedback::Worked => FeedbackKind::Worked,
                DiscordFeedback::Failed => FeedbackKind::Failed,
            };
            match supervisor.record_discord_feedback(reference, feedback) {
                Ok(RecordResult::Recorded) => Ok("OK DISCORD_FEEDBACK RECORDED OUTCOME=UNVERIFIED".to_owned()),
                Ok(RecordResult::AlreadyRecorded) => Ok("OK DISCORD_FEEDBACK ALREADY_RECORDED OUTCOME=UNVERIFIED".to_owned()),
                Err(error) => {
                    eprintln!("Discord feedback rejected: {error:?}");
                    Ok("ERR DISCORD_FEEDBACK".to_owned())
                }
            }
        }
        SupervisorControlCommand::FeedbackLatestDiscord { kind } => {
            if !status.action_gate_open {
                return Ok("ERR POLICY".to_owned());
            }
            let feedback = match kind {
                DiscordFeedback::Worked => FeedbackKind::Worked,
                DiscordFeedback::Failed => FeedbackKind::Failed,
            };
            match supervisor.record_latest_discord_feedback(feedback) {
                Ok(RecordResult::Recorded) => Ok("OK DISCORD_FEEDBACK RECORDED OUTCOME=UNVERIFIED".to_owned()),
                Ok(RecordResult::AlreadyRecorded) => Ok("OK DISCORD_FEEDBACK ALREADY_RECORDED OUTCOME=UNVERIFIED".to_owned()),
                Err(error) => {
                    eprintln!("latest Discord feedback rejected: {error:?}");
                    Ok("ERR DISCORD_FEEDBACK".to_owned())
                }
            }
        }
        SupervisorControlCommand::RememberPreferredApp { event_id, expected_generation, app } => {
            let key = PreferenceKey::new("preferred_app").map_err(|error| error.to_string())?;
            let id = EventId::new(format!("{event_id:032x}"))
                .map_err(|error| error.to_string())?;
            let target = ReferenceId::new(app.application_id())
                .map_err(|error| error.to_string())?;
            match supervisor.record_explicit_preference(expected_generation, &id, &key, &target) {
                Ok(outcome) => Ok(format!("OK PREFERRED_APP APP={} RESULT={outcome:?}", app.application_id())),
                Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                Err(error) => {
                    eprintln!("preferred app write failed: {error}");
                    Ok("ERR STORAGE".to_owned())
                }
            }
        }
        SupervisorControlCommand::ShowPreferredApp => {
            let key = PreferenceKey::new("preferred_app").map_err(|error| error.to_string())?;
            match supervisor.read_explicit_preference(status.generation.value(), &key) {
                Ok(None) => Ok("OK PREFERRED_APP NONE".to_owned()),
                Ok(Some(stored)) => match stored.target.as_str() {
                    "chrome" => Ok("OK PREFERRED_APP APP=chrome".to_owned()),
                    "discord" => Ok("OK PREFERRED_APP APP=discord".to_owned()),
                    _ => Ok("ERR STORED_VALUE".to_owned()),
                },
                Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                Err(error) => {
                    eprintln!("preferred app read failed: {error}");
                    Ok("ERR STORAGE".to_owned())
                }
            }
        }
        SupervisorControlCommand::PreferredAppState => {
            let key = PreferenceKey::new("preferred_app").map_err(|error| error.to_string())?;
            match supervisor.read_explicit_preference(status.generation.value(), &key) {
                Ok(Some(stored)) if matches!(stored.target.as_str(), "chrome" | "discord") => {
                    Ok(format!("OK PREF_STATE ACTIVE APP={} EVENT={}", stored.target.as_str(), stored.event_id.as_str()))
                }
                Ok(Some(_)) => Ok("ERR STORED_VALUE".to_owned()),
                Ok(None) => match supervisor.read_explicit_preference_tombstone(status.generation.value(), &key) {
                    Ok(Some(tombstone)) => Ok(format!("OK PREF_STATE FORGOTTEN TOMBSTONE={}", tombstone.event_id.as_str())),
                    Ok(None) => Ok("OK PREF_STATE NONE".to_owned()),
                    Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                    Err(error) => { eprintln!("preferred app state read failed: {error}"); Ok("ERR STORAGE".to_owned()) }
                },
                Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                Err(error) => { eprintln!("preferred app state read failed: {error}"); Ok("ERR STORAGE".to_owned()) }
            }
        }
        SupervisorControlCommand::ForgetPreferredApp { event_id, expected_generation, expected_event_id } => {
            let key = PreferenceKey::new("preferred_app").map_err(|error| error.to_string())?;
            let id = EventId::new(format!("{event_id:032x}")).map_err(|error| error.to_string())?;
            let expected = EventId::new(format!("{expected_event_id:032x}")).map_err(|error| error.to_string())?;
            match supervisor.forget_explicit_preference(expected_generation, &id, &key, &expected) {
                Ok(outcome) => Ok(format!("OK PREFERRED_APP CLEARED RESULT={outcome:?}")),
                Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                Err(error) => { eprintln!("preferred app clear failed: {error}"); Ok("ERR STORAGE".to_owned()) }
            }
        }
        SupervisorControlCommand::RestorePreferredApp { event_id, expected_generation, app, expected_tombstone_id } => {
            let key = PreferenceKey::new("preferred_app").map_err(|error| error.to_string())?;
            let id = EventId::new(format!("{event_id:032x}")).map_err(|error| error.to_string())?;
            let expected = EventId::new(format!("{expected_tombstone_id:032x}")).map_err(|error| error.to_string())?;
            let target = ReferenceId::new(app.application_id()).map_err(|error| error.to_string())?;
            match supervisor.restore_explicit_preference(expected_generation, &id, &key, &target, &expected) {
                Ok(outcome) => Ok(format!("OK PREFERRED_APP APP={} RESULT={outcome:?}", app.application_id())),
                Err(supervisor::PersistentMemoryError::ProfileGateClosed | supervisor::PersistentMemoryError::StaleGeneration) => Ok("ERR POLICY".to_owned()),
                Err(error) => { eprintln!("preferred app restore failed: {error}"); Ok("ERR STORAGE".to_owned()) }
            }
        }
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
                "OK HANDOFF FACEIT={} CHROME_URL_REQUESTED={} FACEIT_TITLE_MATCH={} STEAM={} PROFILE={:?} DETECTION={:?}",
                report.faceit_ac_launcher_started,
                report.chrome_faceit_url_requested,
                report.chrome_faceit_title_match_detected,
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

/// Verifies that this user can establish and read from Jarvis's temporary,
/// fixed-scope process/service notifications. It starts no process or service;
/// any received event is followed by a fresh read-only identity snapshot.
fn lifecycle_notification_self_check() -> Result<(), String> {
    let setup = verify_default_pre_match_setup().map_err(|error| error.to_string())?;
    let notifications =
        CompetitiveLifecycleNotifications::start(&setup).map_err(|error| error.to_string())?;
    let initial = observe_competitive_activity(&setup).map_err(|error| error.to_string())?;
    let hint_received = notifications
        .wait_for_change(Duration::from_secs(1))
        .map_err(|error| error.to_string())?;
    let current = if hint_received {
        observe_competitive_activity(&setup).map_err(|error| error.to_string())?
    } else {
        initial
    };
    println!("Jarvis fixed lifecycle-notification self-check");
    println!("process start/stop subscriptions: ready (CS2 and fixed FACEIT images)");
    println!("FACEITService status subscription: ready (query-only)");
    println!("event hint received during one-second check: {hint_received}");
    println!(
        "identity-verified snapshot: CS2={} FACEIT_AC={} FACEIT_SERVICE={}",
        current.cs2_running,
        current.verified_faceit_process_running,
        current.verified_faceit_service_active,
    );
    println!("execution: no process, service, browser, game, or login was started or changed");
    Ok(())
}

fn supervisor_status(diagnostics: bool) -> Result<(), String> {
    let request_started_at = Instant::now();
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
    if diagnostics {
        println!(
            "JARVIS_METRIC kind=client_admission_round_trip duration_us={}",
            request_started_at.elapsed().as_micros()
        );
    }
    Ok(())
}

fn last_handoff() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::LastHandoff)
        .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    if !response.starts_with("OK LAST_HANDOFF ") {
        return Err(
            "authoritative supervisor returned an invalid handoff status response".to_owned(),
        );
    }
    println!("{response}");
    println!("Dispatch history only; this does not verify a login, browser URL, anti-cheat readiness, or game state.");
    Ok(())
}

fn open_discord() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::OpenDiscord)
        .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    if !response.starts_with("OK DISCORD_LAUNCH_REQUESTED ") {
        return Err(match response.as_str() {
            "ERR POLICY" => "Discord launch denied by the authoritative profile policy".to_owned(),
            "ERR DISCORD_LAUNCH" => {
                "Discord launcher could not be started; check the monitor console".to_owned()
            }
            _ => "authoritative supervisor returned an invalid Discord response".to_owned(),
        });
    }
    println!("{response}");
    println!("Launch request accepted; Discord startup or window state was not verified.");
    println!("You can report this exact request with --feedback-discord <FEEDBACK_REF> worked|failed within five minutes.");
    Ok(())
}

fn feedback_discord(reference: &str, kind: &str) -> Result<(), String> {
    if reference.len() != 32
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("feedback reference must be the 32-character lowercase hex value from the launch receipt".to_owned());
    }
    let reference =
        u128::from_str_radix(reference, 16).map_err(|_| "invalid feedback reference".to_owned())?;
    send_discord_feedback(SupervisorControlCommand::FeedbackDiscord {
        reference,
        kind: parse_discord_feedback_kind(kind)?,
    })
}

fn feedback_last_discord(kind: &str) -> Result<(), String> {
    send_discord_feedback(SupervisorControlCommand::FeedbackLatestDiscord {
        kind: parse_discord_feedback_kind(kind)?,
    })
}

fn parse_discord_feedback_kind(kind: &str) -> Result<DiscordFeedback, String> {
    match kind {
        "worked" => Ok(DiscordFeedback::Worked),
        "failed" => Ok(DiscordFeedback::Failed),
        _ => Err("feedback must be worked or failed".to_owned()),
    }
}

fn send_discord_feedback(command: SupervisorControlCommand) -> Result<(), String> {
    let response = send_supervisor_control_request(command)
        .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    match response.as_str() {
        "OK DISCORD_FEEDBACK RECORDED OUTCOME=UNVERIFIED" => {
            println!("Feedback recorded for that Discord launch. This is a user report, not machine verification.");
            Ok(())
        }
        "OK DISCORD_FEEDBACK ALREADY_RECORDED OUTCOME=UNVERIFIED" => {
            println!("That feedback was already recorded; it was not counted twice.");
            Ok(())
        }
        "ERR POLICY" => Err("Discord feedback is disabled in Competitive Mode".to_owned()),
        "ERR DISCORD_FEEDBACK" => Err("feedback rejected: reference is stale, unknown, expired, or conflicts with an earlier report".to_owned()),
        _ => Err("authoritative supervisor returned an invalid feedback response".to_owned()),
    }
}

fn set_volume_cli(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 3
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("volume must be an integer from 0 through 100".to_owned());
    }
    let percent = value
        .parse::<u8>()
        .ok()
        .filter(|percent| *percent <= 100)
        .ok_or_else(|| "volume must be an integer from 0 through 100".to_owned())?;
    let response =
        send_supervisor_control_request(SupervisorControlCommand::SetMasterVolume { percent })
            .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    if response.starts_with("OK MASTER_VOLUME REQUEST=") {
        println!("{response}");
        return Ok(());
    }
    Err(match response.as_str() {
        "ERR POLICY" => "volume action denied by Competitive Mode policy".to_owned(),
        "ERR MASTER_VOLUME" => "volume action failed; check monitor console".to_owned(),
        _ => "authoritative supervisor returned an invalid volume response".to_owned(),
    })
}

fn volume_status() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::MasterVolumeStatus)
        .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    if let Some(value) = response.strip_prefix("OK MASTER_VOLUME_STATUS OBSERVED_SCALAR=") {
        let scalar = value
            .parse::<f32>()
            .ok()
            .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
            .ok_or_else(|| {
                "authoritative supervisor returned an invalid volume scalar".to_owned()
            })?;
        println!(
            "Current default output master volume: approximately {:.0}%",
            scalar * 100.0
        );
        return Ok(());
    }
    Err(match response.as_str() {
        "ERR POLICY" => "volume observation is disabled in Competitive Mode".to_owned(),
        "ERR MASTER_VOLUME_STATUS" => "could not read the default output volume".to_owned(),
        _ => "authoritative supervisor returned an invalid volume response".to_owned(),
    })
}

fn discord_status() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::DiscordStatus)
        .map_err(|error| format!("authoritative supervisor unavailable ({error})"))?;
    if let Some(count) = response.strip_prefix("OK DISCORD_STATUS PROCESS_COUNT=") {
        let count = count
            .strip_suffix(" READINESS=UNVERIFIED")
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| {
                "authoritative supervisor returned an invalid Discord count".to_owned()
            })?;
        if count > 64 {
            return Err("authoritative supervisor returned an invalid Discord count".to_owned());
        }
        println!("Verified Discord processes currently observed: {count}");
        println!(
            "This does not verify a ready window, login, or which launch request started them."
        );
        return Ok(());
    }
    Err(match response.as_str() {
        "ERR POLICY" => "Discord process observation is disabled in Competitive Mode".to_owned(),
        "ERR DISCORD_STATUS_UNKNOWN" => {
            "Discord process identity could not be verified; state is unknown".to_owned()
        }
        _ => "authoritative supervisor returned an invalid Discord status response".to_owned(),
    })
}

/// Converts conservative local routing into one closed, typed resident
/// request. The text itself never crosses the control pipe. The resident
/// owner independently validates the action and current profile generation.
fn executable_route(
    utterance: &str,
    configuration_directory: &Path,
) -> Result<SupervisorControlCommand, String> {
    if utterance.is_empty() || utterance.len() > MAX_EXECUTE_COMMAND_BYTES {
        return Err("text command must contain 1 to 256 UTF-8 bytes".to_owned());
    }
    let configuration = ValidatedConfiguration::load_from_dir(configuration_directory)
        .map_err(|error| format!("configuration rejected: {error}"))?;
    let router = Router::from_catalog(configuration.applications());
    match router.route(utterance) {
        RouteResult::Plan(plan)
            if matches!(
                plan.actions.as_slice(),
                [Action::LaunchApplication { application_id }] if application_id == "discord"
            ) =>
        {
            Ok(SupervisorControlCommand::OpenDiscord)
        }
        RouteResult::Plan(plan) if matches!(plan.actions.as_slice(), [Action::SetMasterVolume { .. }]) => {
            let [Action::SetMasterVolume { percent }] = plan.actions.as_slice() else {
                unreachable!("guard ensures one volume action")
            };
            Ok(SupervisorControlCommand::SetMasterVolume { percent: *percent })
        }
        RouteResult::Plan(_) => Err("this routed action has no execution adapter; use --prepare-faceit-cs2 for the confirmed pre-match handoff".to_owned()),
        RouteResult::InvalidPattern { reason } => Err(format!("invalid command: {reason}")),
        RouteResult::Unknown => Err("unknown command; nothing was executed".to_owned()),
    }
}

fn execute_text_command(utterance: &str, configuration_directory: &Path) -> Result<(), String> {
    match executable_route(utterance, configuration_directory)? {
        SupervisorControlCommand::OpenDiscord => open_discord(),
        SupervisorControlCommand::SetMasterVolume { percent } => {
            set_volume_cli(&percent.to_string())
        }
        _ => Err("unsupported execution route".to_owned()),
    }
}

fn show_preferred_app() -> Result<(), String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::ShowPreferredApp)
        .map_err(|error| format!("resident supervisor unavailable: {error}"))?;
    if response == "OK PREFERRED_APP NONE" {
        println!("No preferred app is saved.");
    } else if let Some(app) = response.strip_prefix("OK PREFERRED_APP APP=") {
        if !matches!(app, "chrome" | "discord") {
            return Err("resident supervisor returned an invalid preference".to_owned());
        }
        println!("Saved preferred app: {app}");
    } else {
        return Err(format!("preferred app unavailable: {response}"));
    }
    Ok(())
}

fn remember_preferred_app(app: &str) -> Result<(), String> {
    let app = match app {
        "chrome" => PreferredApp::Chrome,
        "discord" => PreferredApp::Discord,
        _ => return Err("choose exactly chrome or discord".to_owned()),
    };
    let status = send_supervisor_control_request(SupervisorControlCommand::Status)
        .map_err(|error| format!("resident supervisor unavailable: {error}"))?;
    let expected_generation = admitted_memory_generation(&status)?;
    let state = preferred_app_state()?;
    println!("Save {} as your preferred app? This is informational and will not change launches or routing.", app.application_id());
    if matches!(state, PreferredAppMemoryState::Forgotten { .. }) {
        println!("This will explicitly restore a previously cleared preference; its local audit history remains.");
    }
    print!("Type SAVE to continue, or press Enter to cancel: ");
    io::stdout().flush().map_err(io_error)?;
    if !read_save_confirmation(&mut io::stdin().lock())? {
        println!("Cancelled; no preference was written.");
        return Ok(());
    }
    let event_id = new_preference_event_id()?;
    let command = match state {
        PreferredAppMemoryState::Forgotten { tombstone_id } => {
            SupervisorControlCommand::RestorePreferredApp {
                event_id,
                expected_generation,
                app,
                expected_tombstone_id: tombstone_id,
            }
        }
        PreferredAppMemoryState::None | PreferredAppMemoryState::Active { .. } => {
            SupervisorControlCommand::RememberPreferredApp {
                event_id,
                expected_generation,
                app,
            }
        }
    };
    let response = send_supervisor_control_request(command).map_err(|error| {
        format!("resident supervisor unavailable; no preference was written: {error}")
    })?;
    if response
        == format!(
            "OK PREFERRED_APP APP={} RESULT=Inserted",
            app.application_id()
        )
        || response
            == format!(
                "OK PREFERRED_APP APP={} RESULT=AlreadyRecorded",
                app.application_id()
            )
    {
        println!(
            "Preferred app saved: {}. This does not launch it or change routing.",
            app.application_id()
        );
        Ok(())
    } else {
        Err(format!("preference was not saved: {response}"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreferredAppMemoryState {
    None,
    Active { app: PreferredApp, event_id: u128 },
    Forgotten { tombstone_id: u128 },
}

fn preferred_app_state() -> Result<PreferredAppMemoryState, String> {
    let response = send_supervisor_control_request(SupervisorControlCommand::PreferredAppState)
        .map_err(|error| format!("resident supervisor unavailable: {error}"))?;
    parse_preferred_app_state(&response)
}

fn parse_preferred_app_state(response: &str) -> Result<PreferredAppMemoryState, String> {
    let fields: Vec<_> = response.split_ascii_whitespace().collect();
    if fields.join(" ") != response {
        return Err("invalid preferred-app state".to_owned());
    }
    match fields.as_slice() {
        ["OK", "PREF_STATE", "NONE"] => Ok(PreferredAppMemoryState::None),
        ["OK", "PREF_STATE", "ACTIVE", app, event] => {
            let app = match *app {
                "APP=chrome" => PreferredApp::Chrome,
                "APP=discord" => PreferredApp::Discord,
                _ => return Err("invalid preferred-app state".to_owned()),
            };
            let event_id = parse_preference_event_field(event, "EVENT=")?;
            Ok(PreferredAppMemoryState::Active { app, event_id })
        }
        ["OK", "PREF_STATE", "FORGOTTEN", event] => {
            let tombstone_id = parse_preference_event_field(event, "TOMBSTONE=")?;
            Ok(PreferredAppMemoryState::Forgotten { tombstone_id })
        }
        _ => Err(format!("preferred-app state unavailable: {response}")),
    }
}

fn parse_preference_event_field(field: &str, prefix: &str) -> Result<u128, String> {
    let value = field
        .strip_prefix(prefix)
        .ok_or_else(|| "invalid preferred-app revision".to_owned())?;
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid preferred-app revision".to_owned());
    }
    u128::from_str_radix(value, 16).map_err(|_| "invalid preferred-app revision".to_owned())
}

fn new_preference_event_id() -> Result<u128, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    Ok(nanos ^ (u128::from(std::process::id()) << 96))
}

fn clear_preferred_app() -> Result<(), String> {
    let status = send_supervisor_control_request(SupervisorControlCommand::Status)
        .map_err(|error| format!("resident supervisor unavailable: {error}"))?;
    let expected_generation = admitted_memory_generation(&status)?;
    let state = preferred_app_state()?;
    let PreferredAppMemoryState::Active {
        app,
        event_id: expected_event_id,
    } = state
    else {
        println!("No active preferred app to clear.");
        return Ok(());
    };
    println!("Clear {} as your active preferred app? The old value remains in local audit history; this is not permanent erasure.", app.application_id());
    print!("Type CLEAR to continue, or press Enter to cancel: ");
    io::stdout().flush().map_err(io_error)?;
    if !read_exact_confirmation(&mut io::stdin().lock(), b"CLEAR\n", b"CLEAR\r\n")? {
        println!("Cancelled; the preference was not changed.");
        return Ok(());
    }
    let response = send_supervisor_control_request(SupervisorControlCommand::ForgetPreferredApp {
        event_id: new_preference_event_id()?,
        expected_generation,
        expected_event_id,
    })
    .map_err(|error| format!("resident supervisor unavailable; preference not cleared: {error}"))?;
    if matches!(
        response.as_str(),
        "OK PREFERRED_APP CLEARED RESULT=Inserted"
            | "OK PREFERRED_APP CLEARED RESULT=AlreadyRecorded"
    ) {
        println!("Active preferred app cleared. Its local audit history remains.");
        Ok(())
    } else {
        Err(format!("preference was not cleared: {response}"))
    }
}

fn admitted_memory_generation(status: &str) -> Result<u64, String> {
    let fields: Vec<_> = status.split_ascii_whitespace().collect();
    if fields.get(..2) != Some(&["OK", "STATUS"]) {
        return Err("resident supervisor returned an invalid status response".to_owned());
    }
    let unique_field = |name: &str| {
        let mut values = fields[2..]
            .iter()
            .filter_map(|field| field.strip_prefix(name));
        let value = values.next()?;
        values.next().is_none().then_some(value)
    };
    if unique_field("PROFILE=") != Some("GodMode")
        || unique_field("TRANSITION=") != Some("Stable")
        || unique_field("ACTION_GATE=") != Some("true")
        || unique_field("WORKER_GATE=") != Some("true")
    {
        return Err(
            "resident supervisor is not in stable God Mode; no preference was written".to_owned(),
        );
    }
    unique_field("GENERATION=")
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| "resident supervisor returned an invalid generation".to_owned())
}

/// Accept only one complete, exact console line. A bounded read prevents a
/// pasted or piped megabyte-sized line from allocating without limit.
fn read_save_confirmation<R: BufRead>(input: &mut R) -> Result<bool, String> {
    read_exact_confirmation(input, b"SAVE\n", b"SAVE\r\n")
}

fn read_exact_confirmation<R: BufRead>(
    input: &mut R,
    lf: &[u8],
    crlf: &[u8],
) -> Result<bool, String> {
    const MAX_CONFIRMATION_BYTES: usize = 8;
    let mut answer = Vec::with_capacity(MAX_CONFIRMATION_BYTES);
    loop {
        let available = input.fill_buf().map_err(io_error)?;
        if available.is_empty() {
            return Ok(false);
        }
        let line_bytes = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if answer.len() + line_bytes > MAX_CONFIRMATION_BYTES {
            return Ok(false);
        }
        answer.extend_from_slice(&available[..line_bytes]);
        input.consume(line_bytes);
        if answer.ends_with(b"\n") {
            return Ok(answer.as_slice() == lf || answer.as_slice() == crlf);
        }
    }
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

    #[test]
    fn preference_admission_requires_stable_god_mode_and_both_gates() {
        let valid = "OK STATUS PROFILE=GodMode TRANSITION=Stable GENERATION=7 ACTION_GATE=true WORKER_GATE=true DETECTION=Healthy";
        assert_eq!(admitted_memory_generation(valid), Ok(7));
        assert!(admitted_memory_generation(
            &valid.replace("WORKER_GATE=true", "WORKER_GATE=false")
        )
        .is_err());
        assert!(admitted_memory_generation(
            &valid.replace("PROFILE=GodMode", "PROFILE=CompetitiveMode")
        )
        .is_err());
        assert!(
            admitted_memory_generation(&valid.replace("GENERATION=7", "GENERATION=bad")).is_err()
        );
        assert!(admitted_memory_generation(
            &valid.replace("TRANSITION=Stable", "TRANSITION=EnteringCompetitive")
        )
        .is_err());
        assert!(admitted_memory_generation(
            &valid.replace("ACTION_GATE=true", "ACTION_GATE=false")
        )
        .is_err());
        assert!(admitted_memory_generation(&format!("{valid} GENERATION=8")).is_err());
        assert!(admitted_memory_generation(&format!("{valid} PROFILE=Competitive")).is_err());
        assert!(admitted_memory_generation("ERR POLICY").is_err());
    }

    #[test]
    fn preference_confirmation_accepts_only_a_complete_exact_bounded_line() {
        for answer in [b"SAVE\n".as_slice(), b"SAVE\r\n"] {
            assert_eq!(
                read_save_confirmation(&mut io::Cursor::new(answer)),
                Ok(true)
            );
        }
        for answer in [
            b"SAVE".as_slice(),
            b"save\n",
            b"SAVE \n",
            b" SAVE\n",
            b"SAVE\r\r\n",
            b"\n",
            b"SAV\n",
            b"SAVE\0\n",
            b"SAVE!!!!\n",
        ] {
            assert_eq!(
                read_save_confirmation(&mut io::Cursor::new(answer)),
                Ok(false)
            );
        }
        let oversized = vec![b'x'; 1024 * 1024];
        assert_eq!(
            read_save_confirmation(&mut io::Cursor::new(oversized)),
            Ok(false)
        );
        assert_eq!(
            read_exact_confirmation(&mut io::Cursor::new(b"CLEAR\n"), b"CLEAR\n", b"CLEAR\r\n"),
            Ok(true)
        );
        assert_eq!(
            read_exact_confirmation(&mut io::Cursor::new(b"CLEAR "), b"CLEAR\n", b"CLEAR\r\n"),
            Ok(false)
        );
    }

    #[test]
    fn preferred_app_state_parser_requires_exact_revision_and_closed_app() {
        assert_eq!(
            parse_preferred_app_state("OK PREF_STATE NONE"),
            Ok(PreferredAppMemoryState::None)
        );
        assert_eq!(
            parse_preferred_app_state(
                "OK PREF_STATE ACTIVE APP=chrome EVENT=000000000000000000000000000000ab"
            ),
            Ok(PreferredAppMemoryState::Active {
                app: PreferredApp::Chrome,
                event_id: 0xab
            })
        );
        assert_eq!(
            parse_preferred_app_state(
                "OK PREF_STATE FORGOTTEN TOMBSTONE=000000000000000000000000000000cd"
            ),
            Ok(PreferredAppMemoryState::Forgotten { tombstone_id: 0xcd })
        );
        for invalid in [
            "OK PREF_STATE ACTIVE APP=cs2 EVENT=000000000000000000000000000000ab",
            "OK PREF_STATE ACTIVE APP=chrome EVENT=000000000000000000000000000000AB",
            "OK PREF_STATE ACTIVE APP=chrome EVENT=ab",
            "OK PREF_STATE FORGOTTEN TOMBSTONE=000000000000000000000000000000cd EXTRA=1",
            "ERR POLICY",
        ] {
            assert!(parse_preferred_app_state(invalid).is_err());
        }
    }
    use std::collections::VecDeque;

    struct SequenceSnapshotSource(VecDeque<Result<TriggerSnapshot, DetectionHealth>>);

    impl jarvis_gaming::TriggerSnapshotSource for SequenceSnapshotSource {
        fn observe_trigger_snapshot(&mut self) -> Result<TriggerSnapshot, DetectionHealth> {
            self.0
                .pop_front()
                .expect("test fixture must provide a snapshot for every monitor cycle")
        }
    }

    fn test_config_directory() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
    }

    #[test]
    fn monitor_cycles_fail_closed_and_restore_only_after_all_fixture_triggers_clear() {
        let clear = TriggerSnapshot::default();
        let faceit = TriggerSnapshot {
            verified_faceit_process_running: true,
            ..TriggerSnapshot::default()
        };
        let faceit_and_cs2 = TriggerSnapshot {
            cs2_running: true,
            verified_faceit_process_running: true,
            ..TriggerSnapshot::default()
        };
        let cs2 = TriggerSnapshot {
            cs2_running: true,
            ..TriggerSnapshot::default()
        };
        let faceit_service = TriggerSnapshot {
            verified_faceit_service_active: true,
            ..TriggerSnapshot::default()
        };
        let fixture_snapshots = [
            clear,
            faceit,
            faceit_and_cs2,
            cs2,
            faceit_service,
            clear,
            clear,
        ];
        let fixture_results = fixture_snapshots.into_iter().map(Ok).collect();
        let mut detector = FailClosedTriggerDetector::new(SequenceSnapshotSource(fixture_results));
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and the empty Job Object should initialize");
        let mut last_report = None;
        let start = Instant::now();
        let expected_profiles = [
            Profile::GodMode,
            Profile::Competitive,
            Profile::Competitive,
            Profile::Competitive,
            Profile::Competitive,
            Profile::Competitive,
            Profile::GodMode,
        ];
        let fixture_times = [
            start,
            start + Duration::from_secs(1),
            start + Duration::from_secs(2),
            start + Duration::from_secs(3),
            start + Duration::from_secs(4),
            start + Duration::from_secs(5),
            start + Duration::from_secs(10),
        ];

        for ((fixture, expected_profile), now) in fixture_snapshots
            .into_iter()
            .zip(expected_profiles)
            .zip(fixture_times)
        {
            let observation =
                monitor_cycle_at(&mut supervisor, &mut detector, &mut last_report, now)
                    .expect("synthetic monitor cycle should reconcile without external processes");
            let status = supervisor.status().expect("query fixture profile state");

            assert!(observation.snapshot.detection_healthy);
            assert_eq!(
                observation.snapshot.any_automatic_trigger_active(),
                fixture.any_automatic_trigger_active()
            );
            assert_eq!(status.profile, expected_profile);
            assert_eq!(
                status.action_gate_open,
                expected_profile == Profile::GodMode
            );
            assert_eq!(
                status.worker_start_gate_open,
                expected_profile == Profile::GodMode
            );
        }
    }

    #[test]
    fn lifecycle_hint_causes_a_fresh_fixed_snapshot_before_policy_changes() {
        let clear = TriggerSnapshot::default();
        let faceit = TriggerSnapshot {
            verified_faceit_process_running: true,
            ..TriggerSnapshot::default()
        };
        let mut detector =
            FailClosedTriggerDetector::new(SequenceSnapshotSource([Ok(clear), Ok(faceit)].into()));
        let mut supervisor = Supervisor::bootstrap(&test_config_directory())
            .expect("checked-in configuration and the empty Job Object should initialize");
        let mut last_report = None;

        let startup = monitor_cycle(&mut supervisor, &mut detector, &mut last_report)
            .expect("startup reconciliation should take the first snapshot");
        assert!(!startup.snapshot.any_automatic_trigger_active());
        assert_eq!(
            supervisor.status().expect("query baseline profile").profile,
            Profile::GodMode
        );

        // The event supplies no PID, process name, or state; this cycle reads
        // the next authoritative fixture snapshot and closes both gates.
        let after_hint =
            monitor_after_lifecycle_hint(&mut supervisor, &mut detector, &mut last_report)
                .expect("lifecycle hint should force a fresh snapshot");
        assert!(after_hint.snapshot.verified_faceit_process_running);
        let status = supervisor.status().expect("query reconciled profile");
        assert_eq!(status.profile, Profile::Competitive);
        assert!(!status.action_gate_open);
        assert!(!status.worker_start_gate_open);
    }

    #[test]
    fn lifecycle_wait_slices_keep_the_periodic_watchdog_deadline() {
        let start = Instant::now();
        let next_poll = start + COMPETITIVE_POLL_INTERVAL;
        assert_eq!(
            monitor_wait_slice(next_poll, start),
            CONTROL_EVENT_CHECK_INTERVAL
        );
        assert_eq!(
            monitor_wait_slice(next_poll, next_poll - Duration::from_millis(20)),
            Duration::from_millis(20)
        );
        assert_eq!(monitor_wait_slice(next_poll, next_poll), Duration::ZERO);
        // The wait helper receives the deadline immutably, so lifecycle hints
        // cannot extend the 500 ms watchdog.
        assert_eq!(next_poll, start + COMPETITIVE_POLL_INTERVAL);
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
    fn text_execution_maps_only_configured_discord_aliases_to_fixed_pipe_command() {
        let configuration = test_config_directory();
        for utterance in ["Jarvis, open Discord", "open dc", "  OPEN   DISCORD  "] {
            assert_eq!(
                executable_route(utterance, &configuration),
                Ok(SupervisorControlCommand::OpenDiscord)
            );
        }
        assert_eq!(
            executable_route("set volume to 50", &configuration),
            Ok(SupervisorControlCommand::SetMasterVolume { percent: 50 })
        );
        for utterance in [
            "open cs2",
            "prepare faceit cs2",
            "set volume to 101",
            "open chrome",
            "run powershell",
            "",
        ] {
            assert!(executable_route(utterance, &configuration).is_err());
        }
        assert!(
            executable_route(&"x".repeat(MAX_EXECUTE_COMMAND_BYTES + 1), &configuration).is_err()
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
