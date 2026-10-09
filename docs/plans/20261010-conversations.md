# Agents and conversations

[Roadmap](../roadmap.md) · Future task · Depends on the prepared interface

## Outcome

Replace echo with real Codex and Claude conversations in the existing TUI. Requests stream, cancel, and fail predictably; conversation history and model settings survive restart. The same transport supports a stateless `ask` command.

## Reference implementation

- `old:src/providers.rs`: read-only credentials, request formats, indexed SSE events, provider continuation, and error handling.
- `old:src/lib.rs`: model/message types and session load, save, and lock behavior.
- `old:src/models.rs`: model catalog, selection, and effort capabilities.
- `old:src/main.rs`: one-shot `ask` and session selection; `old:src/chat.rs`: request lifecycle and deferred settings persistence.

Current integration points: `Backend` and captured `Prompt` in `src/agent.rs`, request handling in `src/chat.rs`, settings in `src/model.rs`, and the existing menus. The backend supplies events; terminal input and drawing retain one owner. Final module placement should follow the current code rather than reproduce the old file layout.

## Work within this task

### Provider transport and conversation lifecycle

- [ ] Connect Codex streaming to the normal TUI, then Claude with shared conversation history. Reuse the existing read-only authentication source; missing credentials produce a clear error. Add no login or token-refresh flow in this task.
- [ ] Send completed conversation turns in the next request. Match SSE items and blocks correctly, decode UTF-8 strictly, handle EOF and provider errors, bound buffers, and redact credentials. Preserve received text on failure, including the last queued chunk; partial failures and cancellations must not become successful turns.
- [ ] Keep the next draft editable. Capture text, model, options, and context at submission; allow only one active request. Esc cancels the active request, and exit waits for owned work to stop. Display actual provider usage rather than mock counters.
- [ ] Preserve completed history without reprinting it while events arrive. Moving the cursor or changing the next request's settings must not redraw or alter the running response.

### Persistence and model settings

- [ ] Save successful text conversations atomically and restore them on startup. Preserve private permissions, an exclusive lock, and the 16 MiB session limit. Retain the agreed `--session` and `CLANKR_HOME` selection behavior.
- [ ] Read supported text history from `old`; reject corrupt files, unsupported formats, and unsupported tool/image blocks without overwriting them. Do not silently migrate old data. Checkpoint and attachment support arrive with their respective tasks.
- [ ] Connect the existing model and effort menus to real provider capabilities. Preserve compatible effort, keep selections independent, save the next-turn choice after success/error/cancel, and define manual model IDs without pretending a local catalog proves API availability.
- [ ] Verify model IDs, effort fields, and fast-mode support against current provider specifications. `old` has effort selection but no implemented fast/service-tier mapping; determine supported behavior before wiring the existing `/fast` toggle to requests. Custom models must not inherit another model's capabilities.

### One-shot requests and unavailable models

- [ ] Add `clankr ask 'prompt'` using the same streaming transport and meaningful exit statuses. It remains stateless: no history writes or login/refresh flow. Retain output already printed if the request fails. Define support for later skills/tools when those features are selected, without duplicating transport code.
- [ ] Add visible, cancellable waiting for capacity/rate limits using `Retry-After` or backoff with jitter. This is new work: `old` returns an error for unavailable capacity rather than implementing retries.
- [ ] Retry only the failed call with its captured request. Do not duplicate history, received text, future tool side effects, or submit the next draft. Authentication and invalid-request errors must not loop; define behavior after partial output before implementation.

## Acceptance

In the normal TUI, complete two turns, cancel another while editing the next draft, restart, and switch providers while retaining the supported conversation and settings. Exercise success and failure for both providers. `ask` streams through the same transport without session writes. Unavailable-model waiting is visible and cancellable.

Use a loopback HTTP/SSE server and temporary sessions for deterministic checks: UTF-8 splits, multiple items, partial output, EOF, 401/429/5xx, cancellation, saved-state failures, second writers, corrupt/oversized files, and credential redaction. A fixture must reproduce the historical SSE snapshot conflict in `old` before calling it fixed. Use focused App/transport tests; add a targeted PTY check for changed terminal behavior. Record live provider checks separately from fixtures.
