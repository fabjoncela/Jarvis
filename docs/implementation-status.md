# Implementation status

## Implemented foundation

The workspace provides a validation foundation and one explicit, confirmed
pre-match launch sequence, not a running desktop assistant:

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
13. `--monitor-competitive` performs startup reconciliation, then polls fixed
    process/service snapshots every 500 ms. It rechecks signed setup identities
    every 30 seconds (every 5 seconds when unverified), closes both gates on an
    active trigger or unhealthy observation, and restores God Mode only from a
    healthy snapshot with no active triggers. `--detection-self-check` runs one
    reconciliation cycle and exits. For the current Windows account, the
    monitor owns one exclusive, owner-only local named pipe with a fixed
    protocol (`STATUS`, `PREPARE_FACEIT_CS2`, and `WORKER_SELF_CHECK`). It
    rejects remote clients and validates the connected client's executable
    path. The handoff and worker-start CLI commands require this authority and
    are serialized on the same owner; they fail closed if it is absent or its
    policy gates are closed. `--supervisor-status` queries its latest
    reconciled status. Optional `--diagnostics` flags report monotonic polling,
    reconciliation, handler, and client round-trip durations; diagnostics are
    off by default and do not change policy. Other Windows accounts are
    unsupported by this endpoint.

## Explicitly not implemented

- any worker capability beyond
  the fixed session worker's cooperative exit event
- an installed Windows service, automatic startup, or native OS process/service event subscriptions
- process monitoring faster than the 500 ms poll interval
- general desktop application, audio, browser, file, service, or system action
  execution; only the fixed confirmation-gated pre-match sequence can launch
  the FACEIT AC launcher, Chrome, and Steam app 730
- SQLite, general-purpose IPC, tray UI, microphone, STT, TTS, Qwen, or learning

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

The measured polling cadence gives no evidence that native event subscriptions
are currently necessary. The controlled fixture result below verifies policy
transitions but does not establish live OS detection latency. Keep startup
reconciliation and fail-closed behavior authoritative, and do not add a
general executable- or PID-based API.

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
