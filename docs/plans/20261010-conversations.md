# Agents and conversations

[Roadmap](../roadmap.md) · Current task · Uses the prepared interface

## Outcome

Replace echo with real Codex and Claude conversations in the existing TUI. Requests stream, cancel, and fail predictably; conversation history and model settings survive restart. The same transport supports a stateless `ask` command.

## Reference implementation

- `old:src/providers.rs`: read-only credentials, request formats, indexed SSE events, provider continuation, and error handling.
- `old:src/lib.rs`: model/message types and session load, save, and lock behavior.
- `old:src/models.rs`: model catalog, selection, and effort capabilities.
- `old:src/main.rs`: one-shot `ask` and session selection; `old:src/chat.rs`: request lifecycle and deferred settings persistence.

Current integration points: `Backend` and captured `Prompt` in `src/agent/mod.rs`, request handling in `src/ui/chat.rs`, settings in `src/agent/model.rs`, and the existing menus. The backend supplies events; terminal input and drawing retain one owner.

## Work within this task

### Provider transport and conversation lifecycle

- [x] Connect Codex streaming to the normal TUI. Read `CODEX_ACCESS_TOKEN`/`CODEX_ACCOUNT_ID` or the existing `$CODEX_HOME/auth.json` (default `~/.codex/auth.json`) without login, token refresh, or credential writes. Keep `--mock` for offline interface checks.
- [x] Replay completed Codex turns, including final encrypted reasoning and message phase. Reconcile indexed SSE snapshots and deltas, decode UTF-8 strictly, handle EOF and provider errors, bound buffers, and redact credentials. Drain received text before reporting an error; failed or cancelled turns do not enter conversation history.
- [x] Keep the next draft and queued request settings independent of the running request. Esc cancels network work, exit stops the owned worker, and usage comes from the provider. Counts remain absent until reported.
- [x] Preserve completed terminal history without reprinting it while events arrive. Provider events use the existing renderer and editor lifecycle.
- [x] Connect Claude through the same backend boundary and share conversation history across providers. Read `CLAUDE_ACCESS_TOKEN` or `$PI_CODING_AGENT_DIR/auth.json` (default `~/.pi/agent/auth.json`) without login, token refresh, or credential writes. Replay native reasoning only to its provider; retain completed text when switching providers.

### Persistence and model settings

- [ ] Save successful text conversations atomically and restore them on startup. Preserve private permissions, an exclusive lock, and the 16 MiB session limit. Retain the agreed `--session` and `CLANKR_HOME` selection behavior.
- [ ] Read supported text history from `old`; reject corrupt files, unsupported formats, and unsupported tool/image blocks without overwriting them. Do not silently migrate old data. Checkpoint and attachment support arrive with their respective tasks.
- [x] Connect the existing model and effort menus to verified Codex and Claude capabilities. Preserve compatible effort and keep model, effort, and fast-mode selections independent.
- [ ] Persist the next-turn model settings after success, error, and cancellation.
- [ ] Add catalog discovery and manual model IDs without pretending a local catalog proves API availability. Custom models must not inherit another model's effort or fast-mode capabilities.

Model capabilities live in `src/agent/model.rs`: Codex uses `service_tier: "priority"` for fast mode; Claude Opus 5.5 uses `speed: "fast"` with its beta header. Fast mode starts disabled. Claude Sonnet 5.5 and Haiku 5.5 expose effort controls without fast mode. The shared worker and in-memory history live in `src/agent/chat.rs`; provider authentication, request encoding, and stream parsing live in `src/agent/codex.rs`, `src/agent/claude.rs`, and `src/agent/stream.rs`. Session persistence is the next step.

### One-shot requests and unavailable models

- [ ] Add `clankr ask 'prompt'` using the same streaming transport and meaningful exit statuses. It remains stateless: no history writes or login/refresh flow. Retain output already printed if the request fails. Define support for later skills/tools when those features are selected, without duplicating transport code.
- [ ] Add visible, cancellable waiting for capacity/rate limits using `Retry-After` or backoff with jitter. This is new work: `old` returns an error for unavailable capacity rather than implementing retries.
- [ ] Retry only the failed call with its captured request. Do not duplicate history, received text, future tool side effects, or submit the next draft. Authentication and invalid-request errors must not loop; define behavior after partial output before implementation.

## Acceptance

In the normal TUI, complete two turns, cancel another while editing the next draft, restart, and switch providers while retaining the supported conversation and settings. Exercise success and failure for both providers. `ask` streams through the same transport without session writes. Unavailable-model waiting is visible and cancellable.

Use a loopback HTTP/SSE server and temporary sessions for deterministic checks: UTF-8 splits, multiple items, partial output, EOF, 401/429/5xx, cancellation, saved-state failures, second writers, corrupt/oversized files, and credential redaction. A fixture must reproduce the historical SSE snapshot conflict in `old` before calling it fixed. Use focused App/transport tests; add a targeted PTY check for changed terminal behavior. Record live provider checks separately from fixtures.
