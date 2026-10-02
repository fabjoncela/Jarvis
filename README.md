# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its current V1 foundation has no general desktop-action executor. It includes
one user-confirmed fixed pre-match handoff for FACEIT AC, Chrome, and CS2,
plus one fixed Discord launcher:

- a profile/generation safety model, exercised through unit tests;
- a serialized supervisor foundation with one explicit resident monitor as the
  policy owner and owner of the internal-worker Job Object;
- a fixed same-user control pipe for authoritative status, the confirmed
  pre-match handoff, the internal-worker containment check, and bounded
  preferred-app memory commands;
- an explicit stdin supervisor loop for repeated validation requests, with a
  fixed `status | route <command> | quit` protocol;
- strict, all-or-nothing TOML configuration validation;
- typed, allowlisted action plans;
- deterministic alias and volume-pattern routing;
- bounded RAM-only session-context and result-verification contracts (not yet
  wired to live action execution);
- a private SQLite store for explicit preferred-app memory, revision-bound
  clear/restore, and backup before an existing database migration;
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
launch the fixed pre-match sequence or Discord require that owner; they fail closed when it
is absent. Validation-only CLI commands remain process-local and make no
machine-wide protection claim.

## Project status

See [docs/implementation-status.md](docs/implementation-status.md). The full
architecture specification is maintained at
`C:\Users\Admin\codex-memory\JARVIS.md`; this repository implements it in
small, testable milestones.
The [future architecture compatibility plan](docs/future-architecture-compatibility.md)
records proposed post-current-version upgrades without enabling them.

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
rejects remote clients, permits only fixed typed request forms, and accepts a
client only when its executable path matches the Jarvis server executable. A
second monitor cannot claim the same endpoint. If the resident process exits,
clients cannot claim that protection is active. Run clients from the same
binary path as the monitor; an isolated build and `cargo run` from the default
target are different identities and cannot talk to each other.

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
process, FACEIT Anti-Cheat Launcher process, and `FACEITService` every 500 ms.
It attempts fixed-scope lifecycle notifications where permissions allow; WMI
access is denied for the current account, so this workstation uses the
authoritative polling fallback. Setup signatures
and the fixed installation are rechecked every 30 seconds (every 5 seconds
while unverified). Unknown identities or failed observations are unhealthy
and keep Competitive Mode restricted. God Mode is restored only after a
healthy snapshot confirms all fixed triggers are inactive. This is an opt-in
process, not an installed Windows service or automatic startup monitor. It is
the sole authority for the current Windows
account while running; supported worker-start, Discord, and pre-match clients
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

`--last-handoff` reads the resident supervisor's most recent handoff attempt.
It reports which fixed launch steps were requested, including partial failures,
but never treats dispatch as proof that FACEIT, the browser page, or CS2 is ready.
The record is in memory and disappears when the monitor exits.

`--open-discord` asks the resident supervisor to launch only the fixed,
signature-checked Discord executable under this account's Local AppData.
It is denied when Competitive Mode closes the action gate. The result is a
launch request, not proof that Discord finished starting. The existing
`--route "Jarvis, open Discord"` command remains validation-only.

`--execute "Jarvis, open Discord"` uses the deterministic local router, but
accepts only its configured Discord aliases for execution. It sends the same
fixed `OPEN_DISCORD` request to the resident owner; command text never enters
the control pipe. All other routes—including CS2, pre-match, and volume—are
rejected without an effect. Run this client from the exact same executable
path as the resident monitor; `cargo run` from a different target directory
will fail the peer-identity check.

`--discord-status` makes one on-demand, read-only check for signed Discord
processes under the fixed versioned local installation. It is denied during
Competitive Mode and reports unknown if a Discord-named candidate cannot be
verified. A process count does not prove a ready window, login, or that a
specific Jarvis request started it.

`--pre-match-setup-self-check` verifies that exact local handoff setup without
starting anything: fixed Program Files locations, valid Windows Authenticode
trust for the FACEIT Anti-Cheat Launcher/Service, Chrome, and Steam binaries,
the registered `FACEITService` binary, and Steam's local CS2 app-730 manifest.
The handoff repeats this verification before every launch step. A successful
check is not FACEIT approval or a reusable permission.

In stable God Mode, `--remember-preferred-app chrome` or `discord` asks for an
exact `SAVE` confirmation. `--show-preferred-app` displays the active value.
`--clear-preferred-app` asks for exact `CLEAR` and revokes only the value the
client just observed. A later confirmed save restores it using the current
tombstone revision, so a delayed old request cannot bring it back. Clearing
does not physically erase the local audit history. These preferences do not
change routing or launch any application. The resident supervisor owns all
storage access, and Competitive Mode closes the memory gate.

Runtime data lives under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
