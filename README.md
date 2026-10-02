# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its current V1 foundation has no general desktop-action executor. It includes
one user-confirmed fixed pre-match handoff for FACEIT AC, Chrome, and CS2:

- a profile/generation safety model, exercised through unit tests;
- a serialized supervisor foundation with one explicit resident monitor as the
  policy owner and owner of the internal-worker Job Object;
- a fixed same-user control pipe for authoritative status, the confirmed
  pre-match handoff, and the internal-worker containment check;
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
scaffold. There is no installed Windows service, automatic startup, general
multi-worker lifecycle, or arbitrary desktop action executor. The explicit
`--monitor-competitive` process owns resident policy and monitors only the fixed
CS2/FACEIT triggers described below. Commands that start an internal worker or
launch the fixed pre-match sequence require that owner; they fail closed when it
is absent. Validation-only CLI commands remain process-local and make no
machine-wide protection claim.

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
cargo run -p jarvis -- --competitive-self-check
cargo run -p jarvis -- --detection-self-check
cargo run -p jarvis -- --pre-match-setup-self-check
cargo run -p jarvis -- --route "Jarvis, open Discord"
cargo run -p jarvis -- --serve-stdin
```

`--self-check` validates the fixed configuration bundle and initializes a
serialized supervisor for that validation command. It owns an empty unnamed
Jarvis Job Object and a one-entry internal worker manifest, but does not start
a process monitor or worker. The standalone local setup check is separate and
does not certify compatibility.

Start the authoritative monitor in one terminal:

```powershell
cargo run -p jarvis -- --monitor-competitive
```

In another terminal, `--supervisor-status` queries the resident owner's current
profile and latest fixed-trigger snapshot. The pipe is local, owner-only,
rejects remote clients, permits only three fixed request types, and accepts a
client only when its executable path matches the Jarvis server executable. A
second monitor cannot claim the same endpoint. If the resident process exits,
clients cannot claim that protection is active.

For opt-in timing diagnostics, start the monitor with
`--monitor-competitive --diagnostics`; it reports monotonic reconciliation
intervals and cycle durations. `--supervisor-status --diagnostics` reports the
status request round-trip, including the owner's reconciliation and response.
Diagnostics are disabled by default and do not change policy or capabilities.

`--serve-stdin` keeps the same supervisor alive until `quit` or end-of-input.
It accepts only `status` and `route <command>` requests, each of which remains
validation-only. It is not a Windows service, competitive monitor, or action
executor.

`--worker-self-check` requires the resident monitor and asks that same owner to
start the checked-in session-worker binary beside its `jarvis.exe`. The owner
creates it suspended, assigns it to its Job Object, verifies membership, and
only then resumes it. It signals only the worker's fixed cooperative-exit
event, verifies the Job Object becomes empty, and repeats the cycle to verify
restart readiness. A 500 ms bound precedes a Job-Object-only forced-termination
fallback. The request accepts no user-provided executable path, PID, or command
line, and launches no configured desktop application.

`--competitive-self-check` enters Competitive Mode through the real owned
worker manager and verifies the empty Job Object before reporting closed action
and worker-start gates. `--detection-self-check` performs one fixed-identity
process/service snapshot and startup reconciliation, then exits. It may close
Jarvis's own gates and stop only workers owned by that supervisor; it never
starts, stops, or modifies a game, FACEIT component, or Windows service.

The monitor takes an immediate startup snapshot, then polls the fixed CS2
process, FACEIT
Anti-Cheat Launcher process, and `FACEITService` every 500 ms. Setup signatures
and the fixed installation are rechecked every 30 seconds (every 5 seconds
while unverified). Unknown identities or failed observations are unhealthy
and keep Competitive Mode restricted. God Mode is restored only after a
healthy snapshot confirms all fixed triggers are inactive. This is an opt-in
console process, not an installed Windows service, OS event subscription, or
automatic startup monitor. It is the sole authority for the current Windows
account while running; supported worker-start and pre-match clients
consult it over the fixed control pipe. The polling policy and Job Object end
when the process exits. Other Windows accounts are unsupported by this
single-owner endpoint.

`Jarvis, prepare FACEIT CS2` requests one fixed, confirmation-required
**pre-match handoff** from the resident owner. Start the monitor first, run
`--prepare-faceit-cs2`, and type `START` at its prompt. The owner starts the
verified FACEIT Anti-Cheat Launcher (`faceitclient.exe`), opens
`https://www.faceit.com/` in verified Chrome, then asks Steam to launch app 730
(CS2). It does not start `FACEITService` directly or type credentials. If a
step fails, later steps are skipped; the monitor console records partial
progress. The monitor serializes the sequence with profile updates, then
immediately reconciles the newly started FACEIT/CS2 triggers.

`--pre-match-setup-self-check` verifies that exact local handoff setup without
starting anything: fixed Program Files locations, valid Windows Authenticode
trust for the FACEIT Anti-Cheat Launcher/Service, Chrome, and Steam binaries,
the registered `FACEITService` binary, and Steam's local CS2 app-730 manifest.
The handoff repeats this verification before every launch step. A successful
check is not FACEIT approval or a reusable permission.

Runtime data will eventually live under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
