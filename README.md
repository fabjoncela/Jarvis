# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its current V1 foundation has no general desktop-action executor. It includes
one user-confirmed fixed pre-match handoff for FACEIT AC, Chrome, and CS2:

- a profile/generation safety model, exercised through unit tests;
- a serialized supervisor foundation that owns policy, routing, validation, and
  an empty worker Job Object for each current CLI invocation;
- an explicit stdin supervisor loop for repeated validation requests, with a
  fixed `status | route <command> | quit` protocol;
- strict, all-or-nothing TOML configuration validation;
- typed, allowlisted action plans;
- deterministic alias and volume-pattern routing;
- competitive-mode policy logic with testable in-memory worker lifecycles;
- a fixed manifest for the checked-in `jarvis-session-worker.exe` binary, with
  a containment self-check that starts it suspended, assigns and verifies its
  Job Object membership, then resumes it; it also verifies a bounded
  cooperative exit and a safe restart after the Job Object is empty.

No microphone capture, model loading, or game interaction is active in this
scaffold. There is no resident event loop, CS2/FACEIT monitor, general
multi-worker lifecycle, or arbitrary desktop action executor.
The supervisor created for a CLI command owns its Job Object and is dropped when
that command exits. The only user-app launch path is the explicit, confirmed
fixed pre-match command documented below; it cannot launch arbitrary targets.

## Project status

See [docs/implementation-status.md](docs/implementation-status.md). The full
architecture specification is maintained at
`C:\Users\Admin\codex-memory\JARVIS.md`; this repository implements it in
small, testable milestones.

## Prerequisites

- Rust stable with the `x86_64-pc-windows-msvc` target
- Visual C++ Build Tools and a Windows SDK before linking native Windows builds
- Git

## First commands

```powershell
cd C:\Users\Admin\Documents\Jarvis
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p jarvis-session-worker
cargo run -p jarvis -- --self-check
cargo run -p jarvis -- --worker-self-check
cargo run -p jarvis -- --competitive-self-check
cargo run -p jarvis -- --detection-self-check
cargo run -p jarvis -- --pre-match-setup-self-check
cargo run -p jarvis -- --prepare-faceit-cs2
cargo run -p jarvis -- --route "Jarvis, open Discord"
cargo run -p jarvis -- --serve-stdin
```

`--self-check` validates the fixed configuration bundle and initializes a
serialized supervisor for that validation command. It owns an empty unnamed
Jarvis Job Object and a one-entry internal worker manifest, but does not start
a resident event loop, monitor a game or FACEIT, or start a worker. The
standalone local setup check is separate and does not certify compatibility.

`--serve-stdin` keeps the same supervisor alive until `quit` or end-of-input.
It accepts only `status` and `route <command>` requests, each of which remains
validation-only. It is not a Windows service, process monitor, or action
executor.

`--worker-self-check` starts only the checked-in session-worker binary beside
the current `jarvis.exe`. It creates that worker suspended, assigns it to the
owned Job Object, verifies membership, and only then resumes it. It signals
only the worker's fixed cooperative-exit event, verifies the Job Object becomes
empty, and repeats the cycle to verify restart readiness. A 500 ms bound
precedes a Job-Object-only forced-termination fallback. The command does not
accept a user-provided executable path, PID, or command line, and it does not
launch a configured desktop application.

`--competitive-self-check` enters Competitive Mode through the real owned
worker manager and verifies the empty Job Object before reporting closed action
and worker-start gates. `--detection-self-check` takes one ordinary read-only
snapshot for `cs2.exe`, keeping PID plus creation-time identity for any match.
FACEIT setup is not yet verified, so that command must fail closed into
Competitive Mode. Neither command starts a resident monitor or changes an
external process or service.

`Jarvis, prepare FACEIT CS2` validates one fixed, confirmation-required
**pre-match handoff**. To execute it from the current CLI, run
`--prepare-faceit-cs2` and type `START` at its prompt. Jarvis starts the
verified FACEIT Anti-Cheat Launcher (`faceitclient.exe`), opens
`https://www.faceit.com/` in verified Chrome, then asks Steam to launch app 730
(CS2). It does not start `FACEITService` directly or type credentials. If a
step fails, later steps are skipped and the error reports which earlier steps
were started. The successful CLI exits after sending the three launch requests.

`--pre-match-setup-self-check` verifies that exact local handoff setup without
starting anything: fixed Program Files locations, valid Windows Authenticode
trust for the FACEIT Anti-Cheat Launcher/Service, Chrome, and Steam binaries,
the registered `FACEITService` binary, and Steam's local CS2 app-730 manifest.
The handoff repeats this verification before every launch step. A successful
check is not FACEIT approval or a reusable permission.

Runtime data will eventually live under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
