# Implementation status

## Implemented foundation

The workspace currently provides a non-executing validation slice, not a
running desktop assistant:

1. `jarvis-core` creates request IDs, profile generations, and cancellation
   tokens.
2. `apps/jarvis::Supervisor` is the sole mutable owner of configuration,
   profile policy, deterministic routing, action validation, the fixed worker
   manifest, and an empty owned Job Object for a single CLI invocation.
   `--serve-stdin` can keep that same serialized owner alive for the explicit
   `status`, `route <command>`, and `quit` console protocol.
3. `jarvis-profiles` owns God Mode/Competitive Mode transitions and the action
   and worker-start gates.
4. `jarvis-config` validates the fixed TOML bundle all-or-nothing and rejects unknown
   fields, unsafe storage paths, raw executable paths, policy weakening, alias
   collisions, and external cleanup targets. It does not certify FACEIT
   compatibility or a local FACEIT identity.
5. `jarvis-actions` accepts only typed, configured plans and exposes an
   executor interface with no production executor yet.
6. `jarvis-router` performs conservative normalization, config-derived exact
   aliases, and a bounded `set volume to <0..100>` pattern.
7. `jarvis-windows-ffi` owns the only reviewed raw Win32 calls. It creates an
   unnamed Job Object with `KILL_ON_JOB_CLOSE`, can query its active process
   count, and can terminate only members that future trusted startup assigns.
8. `jarvis-windows` retains the in-memory adapter for policy tests, owns the
   fixed one-entry internal-worker manifest, and adds an opaque empty Job
   Object. It deliberately exposes no API to open, adopt, or launch a user
   process.
9. `jarvis-gaming` models competitive transitions against testable in-memory
   lifecycle boundaries; it is not wired to a live detector or runtime.

## Explicitly not implemented

- trusted suspended worker creation, Job Object assignment, or worker IPC
- a Windows service, Windows process/service event subscriptions, or FACEIT
  identity verification
- actual application, audio, browser, file, service, or system actions
- SQLite, IPC, tray UI, microphone, STT, TTS, Qwen, or learning
- any CS2/FACEIT interaction beyond abstract trigger names used in tests

## Next milestone

Implement a long-lived serialized supervisor and a fixed internal worker
manifest. It must create every worker suspended, assign and verify it in its
own Job Object, then resume it. Add ordinary process/service lifecycle
observation and verify Competitive Mode containment before introducing a
single action executor or AI/voice worker.
