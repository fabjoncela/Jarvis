# V1 model-worker transport contract

Status: design and acceptance criteria only. No model IPC or inference is
implemented by this document.

The resident supervisor must never perform blocking model I/O. It owns one
bounded pending request and uses the in-process one-slot request/reply
mailboxes. A separate I/O task may wait on a future worker connection, but
the 500 ms competitive snapshot watchdog stays on the policy thread. A full
request queue fails closed; it does not create an unbounded backlog.

The model worker is a fixed executable started suspended, assigned to the
supervisor's kill-on-close Job Object, verified as a member, and only then
resumed. The worker cannot choose an executable, model file, capability list,
or target application from request text. The existing `--model-worker-self-check`
tests this lifecycle without IPC or inference.

## Transport requirements

- Use a dedicated, per-worker connection. Do not widen the existing
  same-`jarvis.exe` control endpoint to admit `jarvis-model-worker.exe`.
- The owner must establish the endpoint and verify the connected process is
  the exact process handle it launched (PID plus creation time), still in its
  Job Object, and the fixed model-worker image. A same-user process with a
  copied executable name is not sufficient. The worker must reject an
  unexpected server identity; the exact mechanism needs a tested Windows
  implementation before carrying an utterance.
- Keep the endpoint local, owner-only, and remote-client rejecting. If using a
  named pipe, create only one first instance, use bounded message framing,
  reject oversize/partial/trailing data, and authenticate the peer before
  sending text. A pipe name or command-line token alone is not authentication.
- If instead using inherited pipe handles, inherit only explicitly selected
  handles through `STARTUPINFOEX` / `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
  Ordinary `bInheritHandles=TRUE` leaks every inheritable handle and is not
  acceptable for this worker.
- The blocking connect/read/write loop belongs to a dedicated I/O task, not
  the supervisor or monitor reconciliation loop. The task checks the shared
  cancellation/deadline token before and during model work. Competitive entry
  closes action/worker gates, cancels pending requests, and shuts down the
  owned worker Job before any queued response can be admitted.
- Decode requests on the worker side and responses on the supervisor side with
  `jarvis-model-protocol`. No raw shell, broad desktop tool, arbitrary URL,
  file path, game action, or policy change can be emitted. A valid response is
  still only a candidate until current profile, generation, and action policy
  are rechecked immediately before each effect.
- No user utterance, model output, pipe token, or credential is written to
  normal logs. Diagnostics may record byte counts, timings, outcome codes,
  and request IDs only.

## Acceptance before enabling inference

1. Positive contained-worker round trip with `no_action` only; no OS action.
2. Wrong process/image, wrong profile generation, reused request ID, altered
   capability list, oversize frame, partial frame, and extra frame all fail
   closed without exposing the utterance.
3. Worker crash, stalled connect, stalled read, stalled model operation, and
   full mailbox do not delay the 500 ms competitive watchdog. Each has a
   bounded failure outcome and no retry storm.
4. Enter Competitive Mode with request queued, after dequeue, during IPC, and
   just before response admission. Every case cancels the request, empties the
   owned Job, and rejects the late reply. Repeat after God Mode restoration
   to prove stale generations cannot replay.
5. Validate the exact installed model/runtime's child-process behavior.
   `keep_alive` or an API timeout is not evidence that GPU memory and every
   owned inference process were released.

Microsoft documents pipe first-instance, overlapped I/O, remote-client
rejection, and the distinction between `PIPE_NOWAIT` and asynchronous I/O in
[CreateNamedPipeW](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew)
and [Synchronous and Overlapped Pipe I/O](https://learn.microsoft.com/en-us/windows/win32/ipc/synchronous-and-overlapped-input-and-output).
It documents process-ID queries for pipe peers in
[GetNamedPipeClientProcessId](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeclientprocessid)
and [GetNamedPipeServerProcessId](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeserverprocessid).
Its [process-creation guidance](https://learn.microsoft.com/en-us/windows/win32/procthread/creating-processes)
recommends explicit handle lists when inheritance is needed.
