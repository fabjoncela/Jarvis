# Security boundary

Jarvis starts from a deny-by-default design.

- The supervisor is the only component allowed to authorize an action.
- Every request carries an immutable request ID and profile generation.
- Competitive Mode closes action and worker-start gates before worker shutdown.
- An old request or queued worker response is rejected after a generation change.
- The model-facing plan format is typed and allowlisted; arbitrary shell,
  arbitrary executable paths, elevation, game input, process injection, game
  memory access, overlays, drivers, and anti-cheat interference are out of
  scope.
- No secrets, raw recordings, or command transcripts are persisted by default.

Report a potential vulnerability privately to the project owner. Do not publish
proof-of-concept code that bypasses these boundaries.
