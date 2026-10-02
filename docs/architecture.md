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
strict fixed-command protocol carries status, the fixed confirmed pre-match
handoff, Discord launch, and the worker containment self-check. Its DACL is owner-only, remote
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

The opt-in activity monitor reconciles state immediately at startup and takes
authoritative read-only process/service snapshots every 500 ms. When the fixed
FACEIT setup identities verify, it attempts process lifecycle notifications
for the closed set (`cs2.exe`, the verified FACEIT Anti-Cheat Launcher image,
and the verified `FACEITService`) plus a query-only Service Control Manager
subscription. Subscriptions start before the first snapshot; event contents are
opaque hints only, and every hint triggers a fresh complete identity-verified
snapshot. The 500 ms watchdog remains active and is never postponed by event
bursts. If setup is unverified, subscriptions cannot be established, or a
source fails at runtime, the monitor falls back to the same authoritative
polling path without elevation or permission changes. Its fixed process set is
`cs2.exe`, the exact verified FACEIT Anti-Cheat Launcher path, and the exact
verified FACEIT service path; it queries only limited process identity and the
read-only Service Control Manager configuration/status. It records no target
arguments, reads no game memory, injects nothing, and changes no external
process or service. FACEIT setup signatures and fixed installation identities
are rechecked every 30 seconds (every five seconds while unverified). Any
unavailable snapshot, service-path mismatch, or unverified setup is unhealthy
and keeps both profile gates closed. God Mode can return only after a healthy
snapshot confirms all triggers have cleared. The owner coordinates supported
client invocations for its current account while it remains alive. It is not a
Windows service or automatic startup guard, and there is no protection after
the owner exits. Other Windows accounts are unsupported by the single-owner
endpoint.

The pre-match workflow is deliberately singular: the explicit
`--prepare-faceit-cs2` client requires the user to type `START`, then requests
the fixed handoff through the resident owner. That owner launches the verified
FACEIT Anti-Cheat Launcher, makes one best-effort check for a visible verified
Chrome window with a FACEIT-like title, and opens `https://www.faceit.com/` in
Chrome if no clear title match is found. It does not verify the actual tab URL
or handle authentication. It then requests CS2 through Steam app 730. It is
represented as one typed action rather than three arbitrary launch APIs. The
supervisor rechecks profile admission before each step, and each fixed launch
repeats local identity verification. The monitor serializes the handoff with
polling and immediately reconciles after it. Jarvis does not start
`FACEITService` directly; the official launcher manages its own dependencies.
If a step fails, later steps stop and the partial dispatch history remains
available through `--last-handoff` until the monitor exits.

The separate `--open-discord` command is the first narrow desktop action. It
validates the registered Discord action and current profile generation on the
resident owner, then starts only the fixed signed launcher under Local AppData.
It cannot accept a path or arguments. A successful CreateProcess return is
reported as dispatch, not verified application readiness. It is never
available while Competitive Mode closes the action gate.

The native setup check is read-only: it accepts only FACEIT's fixed
Anti-Cheat Launcher/service locations under Program Files, verifies their Windows trust,
checks that the registered `FACEITService` has exactly that service binary,
verifies fixed Chrome and Steam installations, and confirms Steam's local
app-730 manifest. It starts no application or service, carries no durable
launch permission, and is not an anti-cheat compatibility certification.

The V1 storage foundation is a private SQLite adapter under
`%LOCALAPPDATA%\Jarvis\jarvis.db`. The supervisor exposes synchronous, typed
preference/correction writes only after stable God Mode gates and the expected
profile generation are confirmed;
the connection is opened lazily for that call and dropped before returning.
The numbered migration is checksummed and transactional, the schema has no
transcript-specific field, each identifier is limited to 128 ASCII bytes, and
writes are idempotent by event ID with an 8,192-record cap. No storage call is
made during Competitive Mode, no worker depends on this crate, and there is
one explicit text command for the fixed Chrome/Discord preferred-app key.
It requires `SAVE` confirmation through the resident supervisor, is
informational only, and does not affect routing or launch decisions. There is
no general memory command or learning pipeline. Tests use isolated
temporary roots rather than the production path.

The storage adapter also has a revision-bound preference tombstone operation.
`--clear-preferred-app` asks for exact `CLEAR` confirmation and revokes only the
currently observed preference revision. It preserves an audit record rather
than claiming physical erasure; ordinary saves cannot replay across a forget
boundary. A later `--remember-preferred-app` asks for `SAVE` and restores from
the exact current tombstone event ID. A v1 database receives
a verified SQLite-consistent backup at a fixed sibling path before migration
to v2; an existing backup or failed backup blocks that upgrade.

`jarvis-session-context` is a RAM-only contract for a future God Mode session
worker. It holds bounded opaque execution/target identities, clears on profile
generation advance, and fails closed on ambiguous or stale references. It
does not store transcripts or authorize actions, and is not wired into the
placeholder worker yet.

`jarvis-verification` separately models dispatch, exact execution-anchored
feedback, and trusted machine observations. Dispatch or positive feedback
alone cannot produce a verified machine outcome. Its native observation
constructor is intentionally private until a reviewed observer exists; this
is a contract foundation, not a live result verifier.

V2 embeddings and inferred routines, and V3 generated code/build sandboxing,
are intentionally absent until the V1 safety and lifecycle tests pass.
