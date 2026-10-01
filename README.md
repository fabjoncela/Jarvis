# Jarvis

Jarvis is an offline-first Windows desktop assistant written primarily in Rust.
Its first implemented milestone is a safe, side-effect-free V1 foundation:

- a supervisor-owned profile and generation gate;
- typed, allowlisted action plans;
- deterministic alias and volume-pattern routing;
- competitive-mode containment logic with a testable worker-lifecycle boundary.

No microphone capture, model loading, game interaction, Windows process control,
or desktop automation is active in this scaffold. The executable only validates
and displays plans; it cannot execute them.

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

Runtime data will eventually live under `%LOCALAPPDATA%\Jarvis`, never in the
repository. Model files, recordings, credentials, and live databases must not
be committed.
