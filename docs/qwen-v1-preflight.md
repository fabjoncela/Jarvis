# V1 local Qwen preflight

Status: inventory and implementation gate, not an active model integration.
Observed on this Windows workstation on 3 October 2026; recheck before use.

The workspace now includes `jarvis-model-protocol`, a bounded request encoder
and parser for versioned, untrusted model responses. Replies must echo the
exact request ID and profile generation. It returns inert candidates for only
Discord/Chrome launch, master volume, or no action. Schema and negative policy
tests pass; there is still no inference runtime or model-driven execution.
The fixed model-worker binary is now present as a lifecycle-only shell; it
waits for the owned manager's shutdown event and cannot perform inference.
Its start/stop/restart containment test passes on this PC, but model loading,
request/response IPC, timeout, crash, and child-process cases remain open.
The resident supervisor's explicit `--model-worker-self-check` also passed
two cooperative cycles on 3 October 2026 while the fixed competitive triggers
were inactive. It is a lifecycle diagnostic, not inference.
The protocol crate now has a one-slot nonblocking reply mailbox; an I/O task
can submit at most 4,096 bytes and the supervisor can poll without waiting.
The mailbox and post-Competitive rejection are fixture-tested, but no real
worker IPC is connected.

The future response must be one complete JSON value, for example:

```json
{"schema_version":1,"request_id":42,"profile_generation":0,"action":{"type":"launch_application","application_id":"discord"}}
```

The request ID and generation are examples, not reusable values. The worker
must echo the actual values from the bounded request; a mismatch is denied.
Pending requests expire after 30 seconds and can be canceled; neither a model
reply nor worker JSON can extend the deadline.
The Rust supervisor now creates one such pending request only for unknown
deterministic commands in healthy God Mode. It rejects known commands and
invalid patterns as model fallback, consumes responses once, and cancels the
pending request at Competitive entry. There is still no worker IPC or model
load path.
The capability list advertises only `launch_application:discord`,
`launch_application:chrome`, and `set_master_volume`. `no_action` is also
valid. This schema does not grant execution authority.

## Existing local assets

- Ollama 0.32.5 is installed, but it was not running before this inventory.
  Invoking its CLI started the app/server; those inventory-started processes
  were stopped afterward. Jarvis must not silently depend on a persistent
  Ollama daemon, especially across Competitive Mode entry.
- `ollama list` showed one model: `qwen3.6:27b`, approximately 17 GB. Its model
  layer is a 17,420,420,832-byte GGUF v3 blob under the existing Ollama model
  store. Do not copy or redownload it merely to test a runtime.
- `nvidia-smi` reported an RTX 5070 with 12,227 MiB total VRAM; one snapshot
  showed 10,622 MiB free. The 27B blob cannot be assumed to reside fully in
  VRAM, and a snapshot is not a performance measurement.
- No smaller Ollama model or standalone `llama-cli` / `llama-server` executable
  was found in the inspected installation. Ollama bundles native llama/ggml
  libraries, but that is not yet a stable Jarvis worker contract.

## First implementation slice

1. Choose and pin a supported local inference runtime and exact model
   manifest: revision, quantization, license, digest, context cap, and memory
   budget. Reuse the GGUF blob only after the runtime can load it reliably;
   do not bind Jarvis to an internal, undocumented Ollama DLL ABI.
2. Start one fixed Jarvis-owned model worker on demand in the supervisor's
   containment job. It receives a bounded typed planning request and returns
   only a schema-valid proposal. The worker never receives raw executable
   paths, shell authority, or a profile/policy mutation capability.
   Request/response transport must not block the supervisor's independent
   500 ms competitive snapshot watchdog while the model loads or generates.
   A bounded I/O thread or equivalent async boundary may carry bytes, but
   only the serialized supervisor may admit a returned proposal.
3. Recheck profile generation and all typed action permissions in the Rust
   supervisor after the model response and again before each side effect.
   Reject stale, malformed, partial, oversized, or uncertain plans. A
   permission denial cannot be retried by asking a stronger model.
4. Shut down the model worker and its children at Competitive entry, deny
   restart there, and keep the model unloaded after God Mode restoration until
   a fresh unknown request needs it.
5. Benchmark the single-model path first. Then evaluate an optional small
   fast-tier Qwen for bounded classification/tool selection versus the main
   installed model for harder planning. Choose the split only if held-out
   action/argument accuracy and end-to-end latency improve within RAM/VRAM
   limits. Do not keep both tiers resident by default.

## Acceptance evidence before user-facing AI execution

- Fixed worker start/stop, crash, timeout, and child-process containment tests.
- Competitive-entry race tests while loading, generating, and returning a
  proposal; stale responses must never execute.
- Held-out prompts for known-command bypass, valid typed plans, wrong targets,
  out-of-scope actions, malformed JSON, prompt injection, and permission
  denial. Compare single-model and dual-tier candidates on the same cases.
- Cold/warm latency, CPU/RAM/VRAM, and power/performance observations on this
  PC, without launching or interacting with a game for artificial scores.
- No cloud fallback, automatic model download, persistent server, or default
  transcript retention.

Ollama's documented API defaults to keeping a model loaded for five minutes
after a request; an API `keep_alive` value can override that. This alone does
not prove Competitive Mode cleanup, because the serving process and any
children still need ownership and verified termination. See the
[official Ollama API documentation](https://github.com/ollama/ollama/blob/main/docs/api.md).

The current Jarvis checkout has no inference worker yet. The existing
deterministic commands and fixed launch paths remain the only runtime
execution paths.
