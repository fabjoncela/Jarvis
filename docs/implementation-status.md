# Implementation status

## Implemented foundation

The workspace provides a validation foundation and one explicit, confirmed
pre-match launch sequence, not a running desktop assistant:

1. `jarvis-core` creates request IDs, profile generations, and cancellation
   tokens.
2. `apps/jarvis::Supervisor` is the sole mutable owner of configuration,
   profile policy, deterministic routing, action validation, the fixed worker
   manifest, and an owned Job Object for a single CLI invocation.
   `--serve-stdin` can keep that same serialized owner alive for the explicit
   `status`, `route <command>`, and `quit` console protocol.
3. `jarvis-profiles` owns God Mode/Competitive Mode transitions and the action
   and worker-start gates.
4. `jarvis-config` validates the fixed TOML bundle all-or-nothing and rejects unknown
   fields, unsafe storage paths, raw executable paths, policy weakening, alias
   collisions, and external cleanup targets. It does not certify FACEIT
   compatibility or a local FACEIT identity.
5. `jarvis-actions` accepts only typed, configured plans. There is no general
   action executor; the pre-match capability has its own fixed supervisor path.
6. `jarvis-router` performs conservative normalization, config-derived exact
   aliases, and a bounded `set volume to <0..100>` pattern.
7. `jarvis-windows-ffi` owns the only reviewed raw Win32 calls. It creates an
   unnamed Job Object with `KILL_ON_JOB_CLOSE`, and can start only the fixed
   session-worker enum entry under an installation directory: create suspended,
   assign to the Job Object, verify membership, then resume. It creates that
   worker's fixed named exit event before resume, observes the exact process
   handle and exit code, and never accepts a general IPC payload.
8. `jarvis-windows` retains the in-memory adapter for policy tests, owns the
   fixed one-entry internal-worker manifest, and provides an opaque worker
   manager. It sends only the fixed cooperative-exit signal, waits a bounded
   500 ms, falls back to terminating only its Job Object, verifies emptiness,
   and permits restart only after that verified empty state. It exposes no API
   to open or adopt a PID, or launch a user-supplied executable.
9. `jarvis-gaming` models competitive transitions against testable in-memory
   lifecycle boundaries. It also has a read-only, fixed-name `cs2.exe` Tool
   Help snapshot source that records PID plus creation-time identity. Its
   detector fails closed when that snapshot is unavailable or when FACEIT setup
   remains unverified.
10. The supervisor can enter Competitive Mode through the real fixed-worker
    manager: it closes both gates first, verifies its owned Job Object is empty,
    and records a restricted fault if containment cannot be verified.
11. The router and action validator model one fixed, confirmation-required
    pre-match handoff: start the FACEIT Anti-Cheat Launcher, open `faceit.com`
    in Chrome without credential handling, then request Steam app 730. The
    explicit `--prepare-faceit-cs2` command prompts for `START` and runs only
    this sequence, repeating identity verification and profile admission
    before each step. It never starts the service directly.
12. `--pre-match-setup-self-check` performs the native read-only identity
    check for the fixed FACEIT Anti-Cheat Launcher and registered service,
    Chrome, Steam, and app 730. It does not start any component and is not a
    compatibility certification or reusable execution grant.

## Explicitly not implemented

- general worker IPC, a multi-worker lifecycle, or any worker capability beyond
  the fixed session worker's cooperative exit event
- a Windows service, process/service event subscriptions, or FACEIT process or
  service identity verification
- general desktop application, audio, browser, file, service, or system action
  execution; only the fixed confirmation-gated pre-match sequence can launch
  the FACEIT AC launcher, Chrome, and Steam app 730
- SQLite, IPC, tray UI, microphone, STT, TTS, Qwen, or learning
- any CS2/FACEIT interaction beyond abstract trigger names used in tests

## Next milestone

Add ordinary process/service lifecycle subscriptions plus startup
reconciliation, and connect verified FACEIT setup to the fail-closed detector.
Keep detection fail-closed and do not add a general executable- or PID-based
API.
