# Jarvis project instructions

Read `C:\Users\Admin\codex-memory\JARVIS.md` before changing architecture or
security behavior. It is the current design contract; this repository is its
implementation.

- Keep the Rust supervisor authoritative for policy, permissions, profile
  generation, and action admission.
- Use typed, registered capabilities only. Never add arbitrary shell execution,
  unrestricted executable paths, generic elevation, or a model-controlled
  native-code path.
- Preserve the Competitive Mode boundary: no Jarvis voice, LLM, learning,
  automation, build, plugin, or sandbox worker may run while it is active.
- Do not implement gameplay input, hooks, process injection, game-memory
  access, anti-cheat interaction, or in-game overlays.
- Runtime data belongs in `%LOCALAPPDATA%\Jarvis`; do not commit models,
  databases, recordings, credentials, or generated artifacts.
- Add tests for policy and generation changes before enabling a new capability.
