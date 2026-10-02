#![forbid(unsafe_code)]

//! Non-executing CLI for the Jarvis supervisor foundation.
//!
//! `--serve-stdin` keeps one serialized supervisor alive for explicitly typed
//! status and route requests. It intentionally starts no process monitor,
//! worker, or action executor.

mod supervisor;

use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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
        _ => Err("usage: jarvis [--self-check | --route <command> | --serve-stdin]".to_owned()),
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
        "competitive process/service monitoring: not implemented"
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "worker launch, Job assignment, and containment: not implemented"
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
        "fixed internal worker manifest: {} entry; launch disabled",
        status.configured_internal_worker_count
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "FACEIT identity verification (not a certification): {:?}",
        status.faceit_setup_status
    )
    .map_err(io_error)?;
    writeln!(
        output,
        "execution: disabled; no configured desktop action, application launch, or worker launch was performed"
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
