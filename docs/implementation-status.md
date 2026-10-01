# Implementation status

## Implemented foundation

The workspace currently provides a side-effect-free vertical slice:

1. `jarvis-core` creates request IDs, profile generations, and cancellation
   tokens.
2. `jarvis-profiles` owns God Mode/Competitive Mode transitions and the action
   and worker-start gates.
3. `jarvis-actions` accepts only typed, configured plans and exposes an
   executor interface with no production executor yet.
4. `jarvis-router` performs conservative normalization, exact aliases, and a
   bounded `set volume to <0..100>` pattern.
5. `jarvis-windows` is an in-memory worker-lifecycle test adapter; it does not
   inspect or manipulate Windows processes yet.
6. `jarvis-gaming` coordinates a competitive transition against that testable
   lifecycle boundary.

## Explicitly not implemented

- Windows process/service event subscriptions or Job Objects
- actual application, audio, browser, file, service, or system actions
- SQLite, configuration parsing, IPC, tray UI, microphone, STT, TTS, Qwen, or
  learning
- any CS2/FACEIT interaction beyond abstract trigger names used in tests

## Next milestone

Implement configuration validation and a real Windows-owned-worker adapter,
then verify Competitive Mode containment before introducing a single action
executor or AI/voice worker.
