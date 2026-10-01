# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its current V1 foundation is non-executing:

- a profile/generation safety model, exercised through unit tests;
- strict, all-or-nothing TOML configuration validation;
- typed, allowlisted action plans;
- deterministic alias and volume-pattern routing;
- competitive-mode policy logic with testable in-memory worker lifecycles;
- an ephemeral Windows Job Object capability check for future Jarvis-owned
  workers.

No microphone capture, model loading, game interaction, Windows process control,
or desktop automation is active in this scaffold. There is no resident
supervisor, CS2/FACEIT monitor, worker creation/assignment, or real-process
containment. The Job Object used by its self-check is empty and short-lived;
the executable only validates and displays plans, and cannot execute them.

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
cargo run -p jarvis -- --self-check
cargo run -p jarvis -- --route "Jarvis, open Discord"
```

`--self-check` validates the fixed configuration bundle and creates an empty,
unnamed Jarvis Job Object as an ephemeral capability check. It does not start a
supervisor, monitor a game or FACEIT, or contain a worker. FACEIT identity
verification is not yet implemented; this is not a compatibility certification.

Runtime data will eventually live under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
