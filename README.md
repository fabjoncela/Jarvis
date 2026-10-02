# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its current V1 foundation has no desktop-action execution:

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
  Job Object membership, then resumes it.

No microphone capture, model loading, game interaction, or desktop automation
is active in this scaffold. There is no resident event loop, CS2/FACEIT
monitor, general worker lifecycle, or desktop action executor.
The supervisor created for a CLI command owns its Job Object and is dropped when
that command exits. The sole real process start is the fixed-worker containment
self-check; it cannot launch a configured desktop application or arbitrary
executable.

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
cargo run -p jarvis -- --route "Jarvis, open Discord"
cargo run -p jarvis -- --serve-stdin
```

`--self-check` validates the fixed configuration bundle and initializes a
serialized supervisor for that validation command. It owns an empty unnamed
Jarvis Job Object and a one-entry internal worker manifest, but does not start
a resident event loop, monitor a game or FACEIT, or start a worker. FACEIT
identity verification is not yet implemented; this is not a compatibility
certification.

`--serve-stdin` keeps the same supervisor alive until `quit` or end-of-input.
It accepts only `status` and `route <command>` requests, each of which remains
validation-only. It is not a Windows service, process monitor, or action
executor.

`--worker-self-check` starts only the checked-in session-worker binary beside
the current `jarvis.exe`. It creates that worker suspended, assigns it to the
owned Job Object, verifies membership, and only then resumes it. The command
does not accept a user-provided executable path, PID, or command line, and it
does not launch a configured desktop application.

Runtime data will eventually live under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
