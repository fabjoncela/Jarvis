#![forbid(unsafe_code)]

//! CLI for the Jarvis supervisor foundation and fixed pre-match handoff.
//!
//! `--serve-stdin` keeps one serialized supervisor alive for explicitly typed
//! status and route requests. It starts no process monitor or general action
//! executor; the pre-match command is the sole user-application launch path.

mod supervisor;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use jarvis_gaming::{
    pre_match_setup_status, FailClosedTriggerDetector, PreMatchSetupStatus,
    WindowsCs2ProcessSource, WindowsPreMatchSetupSource,
};
use jarvis_profiles::TransitionCause;
use jarvis_windows::InternalWorker;
use supervisor::{RouteOutcome, Supervisor};

const MAX_CONSOLE_COMMAND_BYTES: usize = 4 * 1024;

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
    let mut supervisor =
        Supervisor::bootstrap(configuration_directory).map_err(|error| error.to_string())?;
    match arguments.as_slice() {
        [] => self_check(&supervisor),
        [flag] if flag == "--self-check" => self_check(&supervisor),
        [flag, command] if flag == "--route" => route(command, &mut supervisor),
        [flag] if flag == "--serve-stdin" => serve_stdin(&mut supervisor),
        [flag] if flag == "--worker-self-check" => worker_self_check(&mut supervisor),
        [flag] if flag == "--competitive-self-check" => competitive_self_check(&mut supervisor),
        [flag] if flag == "--detection-self-check" => detection_self_check(&mut supervisor),
        [flag] if flag == "--pre-match-setup-self-check" => pre_match_setup_self_check(),
        [flag] if flag == "--prepare-faceit-cs2" => prepare_faceit_cs2(&mut supervisor),
        _ => Err(
            "usage: jarvis [--self-check | --route <command> | --serve-stdin | --worker-self-check | --competitive-self-check | --detection-self-check | --pre-match-setup-self-check | --prepare-faceit-cs2]"
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
        "competitive process/service monitoring: not implemented; read-only CS2 snapshot: --detection-self-check"
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
                "runtime: validation-only; no worker lifecycle or competitive monitor is running, and no configured desktop action was performed"
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

/// Deliberately starts only the checked-in session worker from beside the
/// current executable. This is a containment self-check, not a general worker
/// launch command or a desktop action.
fn worker_self_check(supervisor: &mut Supervisor) -> Result<(), String> {
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let installation_directory = executable.parent().ok_or_else(|| {
        "could not determine the current executable's installation directory".to_owned()
    })?;
    let identity = checked_worker_cycle(supervisor, installation_directory)?;
    let restarted_identity = checked_worker_cycle(supervisor, installation_directory)?;
    if identity == restarted_identity {
        return Err("worker restart self-check retained a stale process identity".to_owned());
    }
    println!("Jarvis owned-worker containment self-check");
    println!("worker: {:?}", identity.worker());
    println!("process id: {}", identity.process_id());
    println!("creation identity: {}", identity.creation_time_filetime());
    println!(
        "sequence: created suspended, assigned to the owned Job Object, membership verified, then resumed"
    );
    println!("shutdown: fixed cooperative exit observed; Job Object verified empty");
    println!(
        "restart: second fixed worker started only after the empty Job Object verification (process id: {})",
        restarted_identity.process_id()
    );
    println!("execution: no configured desktop action or user application was launched");
    Ok(())
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

/// Runs the ordinary read-only CS2 snapshot once. FACEIT setup is deliberately
/// unverified in this milestone, so this self-check must enter a restricted
/// state instead of claiming detection is healthy.
fn detection_self_check(supervisor: &mut Supervisor) -> Result<(), String> {
    let mut detector = FailClosedTriggerDetector::new(WindowsCs2ProcessSource, false);
    let observation = detector.observe();
    let cause = observation
        .snapshot
        .first_cause()
        .unwrap_or(TransitionCause::DetectionUnhealthy);
    let transition = supervisor
        .enter_competitive(cause)
        .map_err(|error| format!("detection self-check containment failed: {error}"))?;
    if observation.snapshot.detection_healthy || !transition.clean_shutdown {
        return Err("detection self-check did not fail closed".to_owned());
    }
    let status = supervisor.status().map_err(|error| error.to_string())?;
    println!("Jarvis trigger-detection self-check");
    println!("cs2 trigger active: {}", observation.snapshot.cs2_running);
    println!("detection health: {:?}", observation.health);
    println!("profile after fail-closed evaluation: {:?}", status.profile);
    println!("worker-start gate: {}", status.worker_start_gate_open);
    println!("execution: no process, service, game, FACEIT component, or desktop application was modified");
    Ok(())
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

/// Starts the fixed FACEIT AC → Chrome → Steam sequence only after an exact
/// interactive confirmation. Authentication is left entirely to the user.
fn prepare_faceit_cs2(supervisor: &mut Supervisor) -> Result<(), String> {
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

    let report = supervisor
        .prepare_faceit_cs2(true)
        .map_err(|error| format!("pre-match handoff failed: {error}"))?;
    println!(
        "FACEIT Anti-Cheat Launcher started: {}",
        report.faceit_ac_launcher_started
    );
    println!(
        "Chrome faceit.com URL request accepted: {}",
        report.chrome_faceit_url_requested
    );
    println!(
        "Steam CS2 launch request accepted: {}",
        report.steam_cs2_launch_requested
    );
    println!("Jarvis did not sign in or interact with CS2.");
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
