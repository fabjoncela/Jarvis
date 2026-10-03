# Implementation status

## Implemented foundation

The workspace provides a validation foundation and one explicit, confirmed
pre-match launch sequence, not a running desktop assistant:

The dormant model fallback gate also rejects protected gaming topics, including
separator variants of FACEIT, Anti-Cheat, and CS2 and requests naming Steam or
Counter-Strike. This is a conservative routing rule, not an anti-cheat
compatibility claim; no model inference or model-proposed action is running.

1. `jarvis-core` creates request IDs, profile generations, and cancellation
   tokens.
2. `apps/jarvis::Supervisor` is the sole mutable owner of configuration,
   profile policy, deterministic routing, action validation, the fixed worker
   manifest, and its owned Job Object. `--monitor-competitive` keeps the
   authoritative owner alive; `--serve-stdin` remains a separate validation-
   only process-local console.
3. `jarvis-profiles` owns God Mode/Competitive Mode transitions and the action
   and worker-start gates.
4. `jarvis-config` validates the fixed TOML bundle all-or-nothing and rejects unknown
   fields, unsafe storage paths, raw executable paths, policy weakening, alias
   collisions, and external cleanup targets. It does not certify FACEIT
   compatibility or a local FACEIT identity.
5. `jarvis-actions` accepts only typed, configured plans. There is no general
   action executor; the pre-match capability and fixed Discord/Chrome launchers
   have separate supervisor-owned paths. Standalone CS2/FACEIT Anti-Cheat
   launch plans are rejected even though those IDs are configured; the
   confirmed pre-match handoff must be the plan's sole action.
6. `jarvis-router` performs conservative normalization, config-derived exact
   aliases, and a bounded `set volume to <0..100>` pattern.
7. `jarvis-windows-ffi` owns the only reviewed raw Win32 calls. It creates an
   unnamed Job Object with `KILL_ON_JOB_CLOSE`, and can start only the fixed
   session- or model-worker enum entry under an installation directory: create
   suspended, assign to the Job Object, verify membership, then resume. It
   creates each worker's fixed named exit event before resume, observes the
   exact process handle and exit code, and never accepts a general IPC payload.
8. `jarvis-windows` retains the in-memory adapter for policy tests, owns the
   fixed two-entry internal-worker manifest, and provides an opaque worker
   manager. It sends only the fixed cooperative-exit signal, waits a bounded
   500 ms, falls back to terminating only its Job Object, verifies emptiness,
   and permits restart only after that verified empty state. It exposes no API
   to open or adopt a PID, or launch a user-supplied executable.
9. `jarvis-gaming` models competitive transitions against testable in-memory
   lifecycle boundaries. Its native source reconciles `cs2.exe`, the exact
   verified `faceitclient.exe` path, and `FACEITService` status/binary identity.
   Target process identity uses limited query access, PID, creation time, and an
   exact canonical FACEIT image path; no process memory or command line is read.
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
13. `--monitor-competitive` attempts fixed-scope process/service lifecycle
    notifications before its startup snapshot when setup identities verify.
    Notification payloads are opaque hints only; each hint triggers a new
    complete identity-verified snapshot. The independent 500 ms snapshot
    watchdog remains authoritative and is not postponed by event bursts.
    Subscription setup/runtime failure falls back to that poller; neither
    elevation nor WMI permission changes are attempted. Setup identities are
    rechecked every 30 seconds (every 5 seconds when unverified), both gates
    close on an active trigger or unhealthy observation, and God Mode restores
    only from a healthy all-clear snapshot after the configured cooldown.
    `--detection-self-check` runs one reconciliation cycle and exits. For the
    current Windows account, the monitor owns one exclusive, owner-only local
    named pipe with a fixed protocol for status, the handoff, both worker
    self-checks, Discord/Chrome, volume, feedback, and bounded preferred-app
    state/save/clear/restore commands.
    It rejects remote clients and validates the connected
    client's executable path. Handoff and worker-start CLI commands require
    this authority and are serialized on the same owner; they fail closed if it
    is absent or its policy gates are closed. `--supervisor-status` queries its
    latest reconciled status. Optional `--diagnostics` flags report polling,
    lifecycle-reconcile, handler, and client round-trip durations; diagnostics
    are off by default and do not change policy. Other Windows accounts are
    unsupported by this endpoint.
14. `jarvis-storage` provides numbered, checksummed, transactional SQLite
    migrations for explicit typed preference/correction records under
    `%LOCALAPPDATA%\Jarvis\jarvis.db`. Records are parameterized, idempotent
    by event ID, bounded to 128-byte identifiers and 8,192 rows, and contain
    no transcript-specific column. The supervisor opens storage lazily for a
    synchronous write only after stable God Mode gates and an expected profile
    generation are confirmed, then drops the connection. The explicit
    `--remember-preferred-app chrome|discord` command asks for `SAVE`, then
    sends a bounded fixed-key request to the resident supervisor;
    `--show-preferred-app` reads it. This value is informational only and never
    changes a route or launch. There is no database I/O during startup or
    Competitive Mode, and the session worker has no storage dependency.
    Tests use isolated temporary roots.
15. `jarvis-verification` introduces a typed separation between dispatch,
    exact execution-anchored user feedback, and trusted machine outcomes.
    Receipt time, profile generation, deadlines, evidence order, and terminal
    outcomes are enforced; feedback never promotes a machine outcome. No
    public caller can construct trusted machine observations yet. It is not
    wired to OS observation or live action execution.
16. `jarvis-session-context` provides bounded RAM-only target and exact
    execution-anchor history, with TTLs, ambiguity rejection, and monotonic
    profile-generation invalidation. It stores no transcripts. Discord
    dispatches now register an anchor in the resident supervisor's context;
    the placeholder session worker is still not connected.
17. Storage has revision-bound preference forget/restore operations.
    A tombstone keeps audit history and blocks stale save retries; explicit
    restoration requires its exact event ID. `--clear-preferred-app` asks for
    `CLEAR` and revokes only the observed revision; a later
    `--remember-preferred-app` asks for `SAVE` and restores from the current
    tombstone. Neither command launches an app or changes routing.
    An existing v1 database must receive a verified SQLite-consistent backup
    before the v2 migration; failure or a pre-existing backup blocks upgrade.
18. `--execute <text>` is an opt-in text entry point for configured Discord and
    Chrome aliases or the bounded master-volume pattern. The client maps each
    to its fixed typed pipe command; all other plans and unknown text are
    rejected before IPC. The resident owner still validates its typed action,
    profile, and generation. `--route` remains validation-only.
19. `--discord-status` makes one read-only fixed-scope process snapshot in
    God Mode. It verifies PID/creation-time identity, canonical versioned
    Discord image location, and Windows trust, and reports the process count.
    A failed or mismatched candidate makes the state unknown. Presence is
    never attributed to a launch receipt or promoted to UI/login readiness.
20. The fixed Discord launch receipt now includes a `FEEDBACK_REF`. The
    `--feedback-discord <reference> worked|failed` CLI attaches one RAM-only
    report to the exact latest dispatch through `jarvis-verification`. Wrong,
    superseded, conflicting, late, or stale-generation reports are rejected;
    identical retries are idempotent. Competitive entry cancels pending
    verification. User feedback cannot produce a machine-verified outcome or
    persist experience for learning.
21. `--set-volume <0..100>` and deterministic `--execute "set volume to N"`
    requests now map to one bounded `SetMasterVolume` IPC command. The resident
    owner validates the typed action and current profile generation, then the
    narrow Core Audio adapter changes only the default console render
    endpoint and reads back its scalar. No arbitrary endpoint or COM object is
    request-selectable. Tests cover malformed protocol, out-of-range values,
    and Competitive/stale-generation denial. The write call has not yet been
    live-smoke-tested.
22. `--volume-status` makes one on-demand read-only Core Audio query of that
    fixed endpoint in God Mode. It is denied by the profile gate during
    Competitive Mode. Its scalar is reported as an approximate percentage,
    not evidence of audible output.
23. `--feedback-last-discord worked|failed` uses the bounded session context
    to resolve a single recent Discord execution anchor. Two launches within
    its 15-second ambiguity window are rejected, as are expired or
    generation-invalidated anchors. The reference-bearing feedback command
    remains available for exact selection. Neither path verifies success.
24. `--open-chrome` and configured `--execute "open chrome"` route to a separate
    fixed supervisor-owned Chrome launcher. It resolves only the Program Files
    Chrome image, verifies Windows trust, and passes no URL or caller
    arguments. The profile and generation gates are checked before launch;
    tests cover Competitive denial and stale request rejection. The new
    Chrome command was smoke-tested through the resident monitor on this PC;
    it does not replace the confirmed FACEIT pre-match handoff.
25. `jarvis-model-protocol` now parses an untrusted local-model response into
    an inert, request-anchored candidate. Its 4,096-byte, strict versioned JSON
    schema accepts only no-action, fixed Discord/Chrome launch IDs, or bounded
    master volume. Unknown targets, additional fields, broad actions, partial
    JSON, trailing output, and mismatched request/profile-generation echoes
    fail closed. It also encodes one ephemeral utterance of at most 1,024 bytes
    with a fixed capability list and no stored context. Its admission function
    rechecks the live Rust profile and action registry. A supervisor-owned
    30-second pending request also rejects canceled or expired replies, even
    without a profile change. It does not start a model, dispatch an action,
    or bypass the resident supervisor's live generation/profile and
    registry checks.
    The supervisor now prepares an ephemeral model request only for an unknown
    deterministic route while both God Mode gates are open. Known commands and
    invalid patterns cannot fall through to it. Only one pending request is
    retained; response admission is one-shot and validation-only. Competitive
    entry cancels it before owned-worker shutdown. No request is sent to a
    model yet. Unknown text naming FACEIT, CS2, Anti-Cheat, Steam, or
    Counter-Strike, including separator variants, is denied at this fallback
    gate and needs a reviewed explicit workflow.
    A separate one-slot, 4,096-byte reply mailbox lets a future blocking I/O
    task submit without waiting; the supervisor polls it without waiting and
    applies the same one-shot admission. Tests reject a queued reply after
    Competitive entry. A separate one-slot outbound request mailbox lets the
    supervisor enqueue without waiting; queued and dequeued requests retain
    the same cancellation/deadline token, and full queues cancel pending work.
    This is an in-process handoff contract, not worker IPC or a running I/O
    task. The worker-side decoder checks a 2,048-byte envelope,
    exact capability list, and request shape; the model-worker library returns
    only an echoed `no_action` response in tests. This is a transport contract,
    not live worker IPC. The protocol also has a bounded incremental
    length-prefixed frame decoder that rejects empty, oversize, partial, and
    extra data without retaining an utterance in diagnostics; no OS pipe is
    connected to it.
    A dormant supervisor-side I/O pump now composes the bounded outbound
    queue, request/response frames, cancellation checks, and reply queue.
    In-memory tests use the inert no-action handler and cover queued and
    in-flight Competitive cancellation plus extra response data. The pump is
    not spawned or called by the resident monitor; it has no authenticated
    Windows transport or model runtime.
26. A second fixed `jarvis-model-worker.exe` identity is in the worker
    manifest and FFI allowlist. The binary currently only waits for its
    manager-created cooperative shutdown event; it loads no model and has no
    request channel. The worker manager can contain it in the owned Job
    Object, but no user-facing model-loading or model-execution path is enabled.
    A native integration test started and cooperatively stopped the fixed
    shell twice, verified owned Job Object emptiness, and the supervisor tests
    deny both worker identities before path resolution in Competitive Mode.
    A separate test confirms the shell immediately exits with an error when
    launched without its manager-created shutdown signal.
    Those two native lifecycle integration tests are now opt-in/ignored in
    default workspace runs and require a fresh healthy idle competitive
    snapshot before a manual run. During development on 3 October, a default
    package test briefly started the inert shell while FACEITService was
    active and no resident monitor was running; it exited cleanly, no worker
    remained, and the tests were changed to prevent a default repeat.
    The resident-only `--model-worker-self-check` now runs two fixed shell
    cycles through the authoritative owner. Its first live run returned two
    distinct process IDs, left no model-worker process, and preserved healthy
    idle God Mode. This diagnostic still loads no Qwen model or inference
    runtime.
27. `--combined-worker-self-check` is a fixed resident-only diagnostic for
    starting both inert shells in the same owned Job, checking that it has
    exactly two members, and shutting both down cleanly. It reconciles the
    competitive snapshot before a second cycle and does not restart if the
    worker gate closed. Protocol and workspace tests pass, but the combined
    live check has not run: on 3 October the resident monitor reported
    `ActivityObservationUnavailable` and stable Competitive Mode while
    FACEITService was running. No worker or FACEIT component was changed.

## Fixed Chrome launch smoke result (3 October 2026)

The matching isolated build at `target\v1-chrome\debug\jarvis.exe` passed a
self-check and was started as the resident monitor only after the previous
monitor reported healthy stable God Mode with all fixed CS2/FACEIT triggers
inactive. The new monitor reported the same state. One `--open-chrome` request
returned `CHROME_LAUNCH_REQUESTED`; 19 Chrome processes were present afterward,
but no before count or window check was taken, so the process count cannot be
attributed to that request. No URL, credentials, game, FACEIT component, or
service was launched by this Chrome command.

## Model-worker lifecycle shell deployment (3 October 2026)

The new fixed worker shell passed a native contained start, cooperative stop,
empty-Job verification, and restart test. The workspace test, clippy, and
build checks passed. The resident monitor was replaced with the matching
`target\debug\jarvis.exe` (PID 12080 at that deployment) only after its old owner
reported healthy stable God Mode with CS2, FACEIT Anti-Cheat Launcher, and
`FACEITService` inactive. The new owner reported the same state. No model was
loaded, no game or FACEIT component was touched, and there is no user-facing
model execution path yet.

The later matching `target\v1-model-check\debug\jarvis.exe` was deployed as
the resident monitor (PID 30440 at deployment) after the same healthy idle
precheck. It passed `--model-worker-self-check` with two distinct owned PIDs,
and reported healthy stable God Mode with all fixed triggers inactive before
and after. No game, FACEIT component, browser, Ollama server, or audio setting
was changed for this diagnostic.

## Read-only volume smoke result (3 October 2026)

After a successful workspace build and self-check, the old monitor was
replaced while its latest identity-verified CS2/FACEIT snapshot was healthy
and idle. The new resident process runs from `target\debug\jarvis.exe` and
reported healthy stable God Mode with all three fixed triggers inactive.
`--volume-status` returned approximately 98% for the default console output
endpoint. No audio setting, game, browser, FACEIT component, or service was
changed. The `--set-volume` write path remains untested on the live endpoint.

## Explicitly not implemented

- any worker capability beyond the fixed session/model shells' cooperative
  exit events
- an installed Windows service or automatic startup
- native lifecycle notifications available on every Windows token; the current
  local self-check received WMI access denied, so this workstation must use the
  authoritative 500 ms identity-snapshot fallback unless access is later
  deliberately changed. This is not a reason to elevate or weaken permissions
  automatically.
- general desktop application, browser, file, service, or system action
  execution; only the fixed confirmation-gated pre-match sequence and the
  fixed Discord/Chrome commands can currently launch user applications, while the
  one bounded volume action affects the default output endpoint
- general-purpose memory/feedback commands or a learning pipeline, general-purpose IPC, tray
  UI, microphone, STT, TTS, Qwen inference, or learning workers

## Measurement result (2 October 2026)

An opt-in diagnostics path was added and sampled on the local Windows session.
The already-running, exact FACEIT Anti-Cheat Launcher was observed; CS2 and the
FACEIT service were inactive. Jarvis did not start or stop any game, FACEIT
component, or service. No dedicated CPU-load benchmark was captured.

- Startup reconciliation: 257 ms.
- Periodic reconciliation start intervals: 500.0–515.7 ms for the observed
  sample, against the configured 500 ms interval. Most cycles took about
  3.7–6.0 ms; occasional 206–215 ms cycles were observed.
- 30 status-client round-trips: 4.392–5.982 ms; median 5.093 ms; p95 5.828 ms.
  Server-side status handlers took 3.662–5.021 ms.
- The observed state remained consistent: FACEIT launcher active, CS2 and
  FACEIT service inactive, detection healthy, Competitive Mode active, and
  worker-start denied. No false-positive or false-negative observation was
  seen in this static-state sample. No trigger start/stop transition was
  induced, so live transition accuracy and event-to-detection latency remain
  unmeasured; synthetic policy tests do not replace that measurement.

The measured poller remains the correctness watchdog. A lifecycle-subscription
probe in an isolated build received WMI access denied (`0x80041003`), so
event-driven latency is not available in this current run; the safe operating
path is still the 500 ms identity-verified snapshot fallback. No elevation,
WMI ACL, service, FACEIT, Chrome, or CS2 change was made. The controlled fixture
result below verifies policy transitions but does not establish live OS
detection latency. Keep startup reconciliation and fail-closed behavior
authoritative, and do not add a general executable- or PID-based API.

The resident monitor was updated while detection was healthy and CS2, the
verified FACEIT launcher, and `FACEITService` were inactive. It now runs from
`target\next-discord-verify2\debug\jarvis.exe` as an explicit hidden process, not
an installed service or automatic-start application. Status and preferred-app
read/empty-clear/canceled-save commands were exercised through its fixed pipe.
The read created an empty `%LOCALAPPDATA%\Jarvis\jarvis.db`; no preference was
saved or cleared. Five concurrent status clients succeeded after a bounded
busy-pipe retry was added. No game, browser, FACEIT component, or service was
started or stopped for these checks.

## Controlled transition fixture result

`apps/jarvis/src/main.rs::monitor_cycles_fail_closed_and_restore_only_after_all_fixture_triggers_clear`
now feeds synthetic healthy snapshots through the same monitor reconciliation
cycle used by the resident monitor. It covers a clear baseline, FACEIT launcher
activation, overlapping FACEIT/CS2 activity, CS2 remaining after FACEIT exits,
service-only activity, and a final fully clear snapshot. The test verifies both
policy gates remain closed for every active trigger and reopen only after all
triggers clear. It uses no real process/service changes and does not measure
live OS event-to-detection latency.

## Live preflight result (2 October 2026)

The native setup self-check verified the fixed FACEIT launcher and registered
service identities, Chrome and Steam installations, and the Steam CS2 app 730
manifest. It started no application or service. At the same time, the observed
session had the FACEIT launcher running, `FACEITService` stopped, and CS2 absent.
The one-cycle detector and resident supervisor both reported healthy detection,
entered stable Competitive Mode because the verified FACEIT launcher was
active, and kept the worker-start gate closed. The resident monitor was stopped
after its status check. No FACEIT, Chrome, Steam, or CS2 process was changed.

## Live transition smoke result (2 October 2026, partial)

With the resident monitor active, force-closing the exact verified FACEIT
launcher process produced a healthy transition to stable God Mode with both
gates open. The fixed `--prepare-faceit-cs2` handoff then completed with
`FACEIT=true CHROME=true STEAM=true`, healthy detection, and Competitive Mode.
The Chrome window title showed the FACEIT site; no page controls or credentials
were entered.

After force-closing all verified Google Chrome processes and the exact FACEIT
launcher process, `FACEITService` remained stopped and untouched. Steam's CS2
process remained active. Jarvis correctly stayed in stable Competitive Mode
with both gates closed from the CS2 trigger alone. Chrome is not a profile
trigger; the FACEIT launcher and service are independently observed triggers.

The live CS2 exit transition was not observed directly. At a later follow-up,
`--supervisor-status` reported a healthy stable God Mode snapshot with CS2,
FACEIT launcher, and FACEIT service all inactive. This confirms the final
all-clear state, but not the exact transition timing or in-game state at exit.
Jarvis continues to observe `FACEITService` read-only and does not stop it.

## Last handoff dispatch history

The resident monitor now exposes `--last-handoff` as read-only, process-local
dispatch history. It records rejected, stopped, or fully dispatched attempts
with per-step flags; the outcome is always labelled unverified. It does not
launch, inspect tabs, verify a login, or certify anti-cheat/game readiness.
The record is lost on monitor restart.

## Fixed Discord launch smoke result (2 October 2026)

`--open-discord` was sent once through the resident monitor while its latest
snapshot was healthy God Mode and CS2/FACEIT triggers were inactive. The owner
reported a launch request accepted. A separate read-only process query then
found Discord processes under this account's local Discord installation.
This is evidence that Discord started, but the current command still does not
verify a ready window or login. The unit tests deny the action in Competitive
Mode and reject an old request generation after profile restoration. No
FACEIT, Chrome, Steam, or CS2 process was launched or closed for this test.

The new text entry point was smoke-tested with `--execute "Jarvis, open
Discord"`; it returned the fixed Discord launch receipt and the resident
monitor stayed in healthy God Mode. Separate `open cs2`, `prepare faceit cs2`,
and unknown-command attempts exited without launch. The older `--route`
command remains preview-only.

## Fixed Discord process observation (3 October 2026)

The on-demand `--discord-status` command observed six signed processes under
the fixed versioned Discord installation. An initial implementation checked
the same image signature once per process and took about 1.1 seconds per
request; it was replaced with one trust check per distinct image, capped at
two image versions. Three subsequent client round trips measured 229.5–249.8
ms while the supervisor stayed in healthy God Mode. These are local samples,
not a hard latency guarantee; the synchronous query can still briefly delay
the resident monitor's next poll. No Discord, game, browser, or FACEIT process
was started or closed for this status test.

## One-time Chrome FACEIT title check

The fixed Chrome step now makes one best-effort pass over visible top-level
Chrome windows. It counts only windows owned by the locally verified Chrome
binary and whose title starts with a recognized FACEIT marker. A match skips
the Chrome URL launch; no match or an unavailable/ambiguous check preserves the
existing fixed URL launch. Handoff output distinguishes `CHROME_URL_REQUESTED`
from `FACEIT_TITLE_MATCH` so a title match is not reported as a verified URL.

This is a convenience heuristic, not a URL or tab verifier: it can miss a
FACEIT background tab or a title without the marker, and it does not inspect
page contents. It runs only during the explicit handoff, with no polling,
extension, or remote-debugging endpoint. Unit tests cover title matching,
unknown-state fallback, accurate reporting, and continuing to the Steam step.
