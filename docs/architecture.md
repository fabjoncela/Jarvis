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
configuration bundle before it constructs the router or registry. The explicit
`--monitor-competitive` process is the sole resident policy owner for the
current Windows account. It owns an opaque reviewed Win32 Job Object and a fixed
manifest for the checked-in session worker. A same-user local named pipe with a
strict three-command protocol carries status, the fixed confirmed pre-match
handoff, and the worker containment self-check. Its DACL is owner-only, remote
clients are rejected, one pipe instance is exclusive, and peers must resolve
to the same Jarvis executable image. Action and worker-start requests are
serialized on the owner; supported clients fail closed when it is absent. The
stdin validation loop and other validation-only CLI commands remain
process-local and do not claim machine-wide protection. There is no installed
Windows service, automatic startup, or OS event subscription. The worker
containment self-check creates the fixed worker suspended, assigns and verifies
its Job Object membership, then resumes it. The check accepts neither a user
path nor a PID. The one session worker can receive only its manager-created
cooperative exit event; the manager observes its exact process handle, bounds
cooperative exit, verifies Job Object emptiness after any forced fallback, and
allows a restart only from an empty Job Object. General multi-worker lifecycle
management and additional worker types are not implemented.

The opt-in activity monitor reconciles state immediately at startup, then takes
read-only process/service snapshots every 500 ms. Its fixed process set is
`cs2.exe`, the exact verified FACEIT Anti-Cheat Launcher path, and the exact
verified FACEIT service path; it queries only limited process identity and the
read-only Service Control Manager configuration/status. It records no target
arguments, reads no game memory, injects nothing, and changes no external
process or service. FACEIT setup signatures and fixed installation identities
are rechecked every 30 seconds (every five seconds while unverified). Any
unavailable snapshot, service-path mismatch, or unverified setup is unhealthy
and keeps both profile gates closed. God Mode can return only after a healthy
snapshot confirms all triggers have cleared. This is a polling implementation;
native event subscriptions are a later optimization. The owner coordinates
supported client invocations for its current account while it remains alive.
It is not a Windows service or automatic startup guard, and there is no
protection after the owner exits. Other Windows accounts are unsupported by
the single-owner endpoint.

The pre-match workflow is deliberately singular: the explicit
`--prepare-faceit-cs2` client requires the user to type `START`, then requests
the fixed handoff through the resident owner. That owner launches the verified
FACEIT Anti-Cheat Launcher, opens `https://www.faceit.com/` in verified Chrome
without handling authentication, and requests CS2 through Steam app 730. It is
represented as one typed action rather than three arbitrary launch APIs. The
supervisor rechecks profile admission before each step, and each fixed launch
repeats local identity verification. The monitor serializes the handoff with
polling and immediately reconciles after it. Jarvis does not start
`FACEITService` directly; the official launcher manages its own dependencies.
If a step fails, later steps stop and the partial completion is reported in the
monitor console.

The native setup check is read-only: it accepts only FACEIT's fixed
Anti-Cheat Launcher/service locations under Program Files, verifies their Windows trust,
checks that the registered `FACEITService` has exactly that service binary,
verifies fixed Chrome and Steam installations, and confirms Steam's local
app-730 manifest. It starts no application or service, carries no durable
launch permission, and is not an anti-cheat compatibility certification.

V2 embeddings and inferred routines, and V3 generated code/build sandboxing,
are intentionally absent until the V1 safety and lifecycle tests pass.
