# Future architecture upgrades: compatibility plan

Status: design candidate only. This document adapts the user-supplied
`Jarvis_Architecture_Upgrades.md` proposal for a release after the current
implementation is complete. It grants no runtime permission, changes no
profile, and does not claim that any proposed feature is implemented.

The maintained `C:\Users\Admin\codex-memory\JARVIS.md` blueprint and current
repository behavior remain the design contract. Its V1/V2/V3 milestones are
not renumbered here; choose a release name only when the current version meets
its acceptance criteria.

Many proposal topics already exist in that roadmap. Continue implementing
those existing V1 requirements under the original blueprint; this plan does
not postpone them or create replacement implementations. Only genuinely new
or expanded behavior waits for the later release.

## Release gate and invariants

Do not begin the broad upgrade just because the current foundation builds.
First demonstrate the existing V1 acceptance criteria: an authoritative
supervisor, reliable Competitive shutdown/restoration, useful typed desktop
actions, offline voice/STT/Piper and on-demand Qwen, confirmation and trusted
result verification, explicit memory, and versioned data-based skills with
inspect/disable/rollback controls. Current fixed FACEIT/Discord paths and
contract-only crates are not substitutes for that complete assistant.

The future release must keep:

- Rust as the policy and orchestration owner; C/C++ and external processes are
  narrow, cancellable adapters, never model-controlled native-code paths.
- Typed, registered capabilities. A route, skill, model plan, MCP result, or
  delegated agent cannot execute arbitrary shell text or enlarge its grant.
- The current God Mode semantics. A central permission engine may enforce
  existing rules but cannot silently introduce blanket preapprovals or fewer
  confirmations. The proposal's item 24 remains on hold.
- The strict default Competitive profile: no microphone, wake detector,
  command hotkeys, voice, LLM, learning, embeddings, general actions, MCP,
  planner, sandbox, or external-agent worker. Only the minimal lifecycle
  supervisor and status/exit controls remain. The proposal's item 25 advice
  to keep lightweight wake/direct commands during play is **not compatible**
  with this profile and is excluded. A less strict opt-in profile would need
  a separate explicit decision and review.
- The fixed, confirmation-gated FACEIT AC → Chrome site → Steam CS2 handoff.
  It is not a generic agent routine, not proof of anti-cheat approval, and
  never performs login or gameplay interaction. No automatic recovery step
  may start/retry FACEIT or CS2 outside that authority.
- Offline-first operation, bounded private state in `%LOCALAPPDATA%\Jarvis`,
  no default recording/transcript retention, and no silent network fallback.

## Fit to today's modules

These are responsibility mappings, not a request to scaffold all suggested
crates now. Split modules only when a tested vertical slice needs isolation.

| Proposal items | Compatible landing place | Adaptation before implementation |
| --- | --- | --- |
| 1–3: Direct/Controlled/Agent, skills, planning | `jarvis-router`, `jarvis-actions`, future God Mode session worker | Mode selection chooses a workflow, never grants authority. All modes submit bounded typed plans to the same supervisor. Start with one deterministic routine and one bounded plan fixture. |
| 4, 17: tool routing/loading | Typed action registry, then a versioned tool catalogue | Expose small relevant schemas to Qwen; cache schema selection only with profile generation and catalogue version. Never cache authorization. |
| 5: fast/main Qwen tiers | Future on-demand model worker and scheduler | Benchmark models, quantization, accuracy, latency, RAM and 12 GB VRAM on this PC. One model/prompt strategy may suffice; do not assume two resident models or a 27B model fitting in VRAM. Terminate all model workers in Competitive Mode. |
| 6: ambient transcript | Future voice/session worker | Separate opt-in feature, visible capture state, memory-only bounded retention and clear control. Off by default and always off in Competitive Mode. Do not persist ambient speech or infer approval from wake-word use. |
| 7, 14–16: memory/retrieval/relationships | `jarvis-session-context`, `jarvis-storage`, later retrieval adapter | Extend the existing single SQLite store with numbered, backed-up migrations and provenance. Exact structured retrieval first; embeddings/FTS and relationships only when measured useful. Corrections/forgetting must invalidate derived indexes. |
| 8: live context | Trusted, time-stamped observer adapters | Keep current state separate from remembered facts. Read on demand with freshness limits; no constantly rewritten JSON files or game-memory inspection. |
| 9–11: permission engine, approvals, MCP | Existing profile controller and action registry; later policy and connector adapters | Add a shared enforcement interface without redefining God Mode. Approvals, if introduced, are exact-resource, version-bound, expiring/revocable and never override Competitive Mode. MCP defaults to no invocation grant; remote output is untrusted data. |
| 12–13: verification/recovery | `jarvis-verification`, supervisor, narrow native observers | Bind trusted evidence to exact execution/request/generation and deadlines. Dispatch, user feedback, process presence and goal success remain distinct. Retry only idempotent or state-checked steps within the original scope; no blind CS2/FACEIT retries. |
| 18: result compression | Future bounded task trace | Redact secrets and unnecessary command text before storage or model context. Preserve uncertainty and evidence references; do not keep complete raw tool output by default. |
| 19: PersonalJarvis | No core dependency | Deferred and optional. No install, provider choice, source reuse or orchestration integration until separately requested. |
| 20: subagents | Future typed task/result boundary | Define scope, budget, cancellation and evidence first; the supervisor retains permission and verification ownership. No delegated agent in Competitive Mode. External coding CLIs remain under item 19's hold. |
| 21–23: experience, learning, skills | Existing storage/verification contracts, later versioned skill registry | Learn only from independently verified outcomes. Prefer declarative typed recipes; drafts cannot self-promote. Require review for new effects or broader scope, with immutable versions and rollback. |
| 24: God Mode reinterpretation | No implementation | Explicit hold. Record any conflict rather than changing current profile authority or interaction semantics. |
| 25: Competitive profile | `jarvis-profiles`, `jarvis-gaming`, worker manager | Keep current strict shutdown. Measure frame times and 1% lows, but never trade away voice/worker shutdown or security features for a performance target. |
| 26–27: runtime diagram and repo tree | Existing Cargo workspace | Treat the diagram/tree as logical responsibilities. Reuse existing crates and add new ones only for a working, testable vertical slice. |
| 28: proposed phase order | Existing V1/V2/V3 roadmap | Do not restart with voice as phase 1. Finish the current action/policy/verification and Competitive foundations; introduce later features behind the same boundaries. |

## Sequencing after the current version is complete

1. **Baseline audit:** run the current V1 acceptance suite, record actual
   missing capabilities, process/VRAM use and latency, and freeze a regression
   set for policy, stale generations, partial outcomes and Competitive races.
2. **Shared contracts:** version typed capability/tool manifests, exact target
   identities, profile-generation-bound requests, confirmation scopes,
   cancellation, execution anchors, observation freshness and unknown results.
   Extend existing modules rather than creating a second policy owner.
3. **One end-to-end vertical slice:** Direct → supervised typed action →
   independent observation → user-visible outcome. Then add one Controlled
   routine with bounded stop/recovery. Unknown/inconclusive is not success.
4. **On-demand intelligence:** measure local Qwen options; add limited tool
   selection and a bounded Agent planner only after deterministic execution
   and verification are sound. Keep model workers disposable and off in
   Competitive Mode.
5. **Knowledge and extensions:** add provenance-aware experience retrieval,
   optional embeddings, scoped connectors/MCP, then candidate-skill learning
   and controlled proactive behavior, each with its own rollback and denial
   tests. Ambient transcript remains a separate privacy opt-in.
6. **Optional last:** consider PersonalJarvis or external coding agents only
   after a new explicit authorization. Item 24 remains unscheduled.

Each slice must pass malformed-input, stale-generation, cancellation,
permission-denial, restart, Competitive-entry and false-success tests. Measure
latency/resource impact on this workstation. Do not use real destructive or
game/anti-cheat actions merely to accumulate success counts.

## Open decisions for the later design review

- What exact capabilities, if any, warrant persistent scoped approval under
  the unchanged God Mode rules? A design that changes those rules waits for an
  explicit decision on item 24.
- Is optional ambient transcription worth its privacy and performance cost?
  It needs separate consent, an indicator, clear/reset and retention settings.
- Which local model/backend combination meets measured latency and quality
  targets without harming gaming or other local AI workloads?
- Which actions have a trustworthy native success observation, and which must
  remain `Unknown` even when a process or API call appears successful?
- Which external connector/agent is needed for a concrete user task? Reference
  repositories are research leads, not audited dependencies. Before importing
  code, verify a specific revision, license, provenance and security behavior.

No proposed module, approval, integration or profile change is authorized by
this plan alone.
