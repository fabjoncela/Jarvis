# Architecture implementation guide

The authoritative design specification is
`C:\Users\Admin\codex-memory\JARVIS.md` (revision 1.1, 1 October 2026).

The workspace begins with the V1 sequence defined there:

1. supervisor, policy, and typed actions;
2. competitive detection, containment, verification, and restoration;
3. configuration, registry, and memory schema;
4. voice and feedback;
5. confirmation and verification;
6. on-demand local Qwen fallback;
7. versioned data-skill learning.

The current implementation validates the complete source-controlled
configuration bundle before it constructs the router or registry. The current
executable is validation-only: it creates a serialized `Supervisor` for one
CLI invocation, or keeps one alive through the explicit `--serve-stdin` loop.
It has no Windows service, process monitor, or worker. That supervisor owns an
empty opaque reviewed Win32 Job Object and a fixed manifest for the checked-in
session worker. Worker launch is not implemented; the planned worker manager
must use the suspended-create, assign, verify, resume sequence before a
manifest worker can run.

V2 embeddings and inferred routines, and V3 generated code/build sandboxing,
are intentionally absent until the V1 safety and lifecycle tests pass.
