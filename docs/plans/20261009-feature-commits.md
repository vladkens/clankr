# clankr: migrate features from old one at a time

## Goal and current state

Current stage: TUI with mocks. A normal `clankr` launch opens the editor; a submitted message returns as a streaming echo response with Markdown. The interface supports waiting, generation, counters, completion, errors, and cancellation. The mock deliberately emits small chunks with delays to exercise the interface; a real API remains a separate task. There are no separate demo commands.

Foundation: `cbeef65 initialize project` contains an empty Rust executable, Cargo, Makefile, rustfmt, `readme.md`, and `agents.md`. The source for selective migration is the local `old` branch at `f503db8`. Extract only the parts needed, not entire interconnected monolithic modules. Parts of `old:readme.md` lag behind the code; check Cmd+Backspace behavior against the latest fix in `old:src/input.rs` and its regression tests.

Completed: editor and echo (`ce8dcdc`), command/skill menus and model settings (`86b081e`), native response wrapping and Ctrl+C behavior (`5909c41`), and streaming Markdown. Copying submitted user messages has been fixed; the next recommended task is copying code blocks. Other task cards remain candidates and do not start automatically.

## How to structure commits

- Develop each feature or fix on a separate branch. Several working commits are allowed within it; each must contain a meaningful, verified step.
- Fold corrections to unfinished work into the commit they repair using amend or fixup/rebase. Do not leave separate cleanup commits for mistakes in intermediate work.
- When integrating into `main`, squash the branch into one complete feature or specific fix commit. Include sources, application integration, dependencies, necessary tests, and documentation together. The commit titles in the task cards refer to this final result; the first version needs both a proper editor and message submission in full.
- A normal `clankr` launch must work after every step. Do not migrate future fields, dependencies, old demo scenarios, or a progress journal in advance.
- Each selected step must produce a reproducible user-visible result. Refine the commit titles below to match the actual diff.

## Dependencies

- Add Rust dependencies and change their versions only through `cargo add`; use `cargo add --dev` for development dependencies. Always choose the latest available version when adding or updating a dependency; do not copy old versions from `old`.
- Do not edit dependency versions in `Cargo.toml` or change `Cargo.lock` by hand. Cargo must make these changes.

## First feature commit: working TUI with editing and echo responses

Commit: `ce8dcdc add terminal chat with multiline editing and mock replies`. Depends on: the completed scaffold.

Sources: `Keyboard`, cleanup, and the necessary dispatch from `old:src/chat.rs`; `Input`, shared `Layout`, word navigation, and regression tests from `old:src/input.rs`; prompt, append-only transcript, and resize from `old:src/view.rs`; minimal PTY lifecycle from `old:tests/cli.py`. Paths: `src/main.rs`, `src/chat.rs`, `src/input.rs`, `src/view.rs`, `tests/cli.py`, Cargo files, and `readme.md`.

- [x] A normal `clankr` launch immediately opens a TUI with an input field. Startup requires no authentication, network, API, or saved session. There is no separate subcommand for this version.
- [x] The layout matches `ag.cast` and the user's screenshots: `┃` runs through every input line, including empty ones; the draft uses normal text, the submitted message is bold, and the response uses normal text with a two-character indent. Leave one empty row between blocks and before the bottom status line. Keep the terminal's default colors, without a custom palette. Do not show role labels or hints for obvious actions; until the API is connected, the status shows only `echo`. The README contains only a brief product description for end users.
- [x] Unicode text can be entered and edited: Left/Right, Backspace/Delete, word navigation, Home/End within a visual line, and Ctrl+A/E within a logical line. Never split a grapheme; the editor and renderer share the same layout.
- [x] Shift+Enter/Alt+Enter insert a newline. Bracketed paste inserts multiline text without submitting it. Up/Down move the cursor across visual lines with a sticky column. Long input has a viewport that keeps the cursor visible; the existing 1 MiB limit applies.
- [x] Cmd+Backspace and Ghostty Ctrl+U clear the entire current logical line, even from its middle; on an empty line, they remove one newline. Option+Backspace/Ctrl+W delete a word. Preserve adjacent lines and Unicode.
- [x] Enter leaves the message in the conversation and adds a response with the same text, including newlines; the roles are visually distinct. The field clears so another message can be sent. Enter on empty input sends nothing. The response uses the submitted text snapshot, not the next draft. The terminal must not execute control sequences from the text.
- [x] Echo is implemented separately in `src/agent.rs` through a nonblocking `Backend` contract: submit a prompt snapshot, receive `Text`/`Finished` events, and cancel. The renderer displays the prompt and an arbitrary response separately. The next draft remains editable while waiting or streaming; pressing Enter again does not start a second request, and Esc cancels the current one. Errors preserve text already received, including the last chunk before `Finished`; cancellation and errors preserve the next draft. Responses are limited to 16 MiB, and events are processed in bounded batches. The backend owns its work and must stop it when dropped; the current echo creates no background tasks.
- [x] The conversation stays on the terminal's main screen with native scrollback and normal mouse selection. Previous messages survive later submissions; resizing preserves text and cursor position without duplicating completed history. Input and rendering have one owner, with no alternate screen or mouse capture.
- [x] During a request, Ctrl+C cancels it and preserves the next draft. When idle, it first clears a nonempty draft and shows a hint to press again; an empty new session closes immediately, while an existing session requires a second Ctrl+C. Ctrl+D exits when input is empty. Restore available terminal modes on normal exit, handled errors, and through the panic hook. Do not promise cleanup after SIGKILL.

Validation: `make check`, `make test`, `make build`. In this commit, migrate focused editor regression tests, submitted-snapshot/echo checks, and rendering/resize checks; selecting zero tests does not count as validation. A minimal `tests/cli.py` runs the normal compiled `clankr` in a PTY, isolates HOME/cwd/CLANKR_HOME and credential environment variables, waits for observable states, and checks terminal restoration. Runner interface: `uv run --no-project --with pyte==0.8.2 python tests/cli.py --bin target/release/clankr --case tui`. These are test runner arguments, not new application flags.

Acceptance: open `./target/release/clankr`, type a multiline Unicode message, edit it with arrow keys and word/line deletion, submit it, and see an exact echo; send a second message and check that the first remains intact. Repeat with deletion of empty first/middle/last lines, pasting, long text, and resizing 119×53 → 40×12 → 80×24 → 119×53, including a narrow window. The PTY checks legacy/CSI-u bytes and terminal cleanup; physical keyboard shortcuts, mouse selection, and native Ghostty reflow require separate checks and must not be claimed as verified by the PTY alone.

Implementation result: `make check`, `make test` (21 tests), `make build`, and the `tui` PTY scenario passed. The final waiting-line alignment fix was verified by rerunning five renderer tests, `make check`, the build, and the PTY. The PTY ran through `uv run --offline --no-project --with pyte==0.8.2 python tests/cli.py --bin target/release/clankr --case tui`, using the shared cache. It checks the normal echo CLI, layout, and default terminal colors. Rust tests of the production App with a controlled backend cover waiting, streaming, errors, and cancellation; a terminal model checks long streams and resizing. Dependencies were added through `cargo add`; errors use `thiserror`, and `anyhow` was removed through `cargo rm`. Native keys, Ghostty reflow, and mouse selection remain manual checks. On unknown terminal versions, resizing may leave an old draft on screen; completed history is neither cleared nor reprinted.

## Completed: menus and streaming Markdown

- [x] `/` opens commands, and `$` opens a separate skill list. Filtering, cyclic navigation, Right/Tab completion, and Esc dismissal are available. `/clear`, `/quit`, `/copy`, `/fast`, `/model`, and `/effort` work in the normal TUI; `/help` is not needed. Skill and model lists are still mocks.
- [x] Model selection preserves compatible effort or uses the model's default otherwise. Effort is selected separately through the menu or Ctrl+, / Ctrl+.; fast mode changes independently. The active request retains its settings while the next draft is edited.
- [x] Markdown renders headings, nested formatting, lists, block quotes, aligned tables, and code blocks. Every line of a nested block repeats its container prefixes: a list item adds indentation matching its marker width, and a block quote adds a faint `> `; an empty line inside a quote remains `>`. Links are underlined and clickable, without repeating the URL next to the label; inline code is not underlined. `/copy` uses the original response.
- [x] After waiting, echo emits up to 16 graphemes every 50 ms. An unfinished block appears in an editable preview; a completed block enters history with native wrapping. The draft remains available. Terminal colors are preserved; the startup line chooses one robot splash per launch.
- [x] One manual example, `mocks/markdown.md`, and `mocks/robot.svg` replace the numbered examples. Syntax highlighting is described in a separate task card below; formulas remain text, and images are represented by alt text/links.

Current Markdown validation: `make check test build` (50 Rust tests), PTY `tui`, `menus`, `markdown`, and the complete manual fixture through the PTY passed. The Ghostty 1.3.1 engine was checked separately: character-by-character streaming, width changes, and selection preserve the response's original newlines without duplication. This does not verify physical keys or the GUI clipboard in all terminals.

## Completed: clean copying of user messages

Commit: `preserve native wrapping in submitted user messages`. Paths: `src/view.rs`, `tests/cli.py`, focused renderer regression tests. The previous `Live::prompt` wrapped lines manually and printed `┃ ` before each one; selection included both prefixes and extra newlines.

Verified in the Ghostty 1.3.1 engine: ordinary borders, OSC 133 prompt markers, DECSCA, and left margins do not exclude `┃` from selection. Bold and inverse text without added characters copies continuously, including after resizing. The [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/#controlling-displayed-image-layout) allows a separate image layer; using it for a border would require independent support and reflow checks and is not considered a ready solution.

- [x] Submitted text uses native terminal wrapping, preserving only the user's original newlines and indentation; `┃ ` was removed from both history and the editor. User text is not parsed as Markdown.
- [x] Accepted design: roles differ through attributes, not color. Submitted messages are bold (SGR 1), starting in the first column without role labels or borders; responses use normal text with Markdown. Service lines, including turn summaries (time, tokens, cancellation, errors) and notifications such as `Copied to clipboard.`, are faint (SGR 2) with a 2-column indent; they are not copied. Faint `─` lines frame the editor above and below, with the status directly beneath the lower line; that line also forms the menu's upper boundary. Colors are reserved for Markdown highlighting: headings will receive a color accent, so bold text in the default color remains the user-message marker. The terminal derives attributes from its current text color, so history follows theme changes, including in tmux. In small windows, lines disappear before the status and input.
- [x] A focused test selects a long message with paragraphs, indentation, Unicode, and blank lines before/after resizing 80 → 40 → 120, then compares it with the source. Completed history is not redrawn, and mouse input is not captured.
- [x] Validation: `make check test build` (53 tests), PTY `tui`, `menus`, `models`, `markdown`. Mouse selection in a real terminal remains a manual check.

## How to choose subsequent tasks

After fixing user-message copying, handle easy copying of code blocks as a separate task, then syntax highlighting. Next, choose the needed TUI capability or API integration. Formulas are deferred until a suitable renderer is available. Do not invent an intermediate feature just to create another commit.

The candidates are listed below. Before implementing a selected task card, refine its scope against the working application. A dependency means required behavior, not a mandatory commit number. For current changes, run `make check`, focused tests, `make build`, and the feature scenario; the full `make test` is needed when shared contracts change.

Task scenarios are covered jointly by Rust tests and the PTY. After API integration, offline tests inject a fake transport directly into the production App and use temporary Session instances and a loopback HTTP/SSE server. This is internal test organization, without demo commands or hidden substitution of the normal application. The normal CLI's PTY still checks input and available actions without a live request; Rust integration tests cover dynamic responses, wire format, save/reload, and failures. If a new UI feature needs a PTY with controlled events, define how to launch a separate test harness in its task card before implementation, without adding a mode to the shipped CLI. The PTY scenario names below are proposed checks to refine when selecting a task card; they are not presented as existing commands.

## Candidates: TUI improvements

### Copy code blocks without decorative borders

Commit: `make rendered code blocks easy to copy`. Paths: `src/markdown.rs`, `src/view.rs`, `tests/cli.py`. This is separate from copying user messages and from syntax highlighting. Currently, `Render::open/close` adds actual `─ robotlang ─────` and `────────` lines to the response content; these are not a separate visual border and enter the clipboard when selected.

- [x] The `─ rust ─────`/`────────` borders were replaced with faint (SGR 2) fence lines: the opening fence is three backticks plus the first token of the info string (`` ```rust ``, or `` ``` `` without a language), and the closing fence is `` ``` ``. Each fence occupies a separate line, so selecting the body copies only code, while copying the whole response retains valid Markdown. Indented blocks and `~~~` blocks use the same `` ``` `` fences; within lists and quotes, fences receive the normal container prefixes. Tables, quotes, and the `────────` separator are unchanged; `/copy` still copies the original response.
- [x] The block body, highlighting, preview `seed` state, and streaming invariants are unchanged: indentation, blank lines, and Unicode copy as-is, without prefixes or width-induced newlines.
- [x] Limitation: `mdtext 0.1.2` does not expose the original fence character and length, and the opening line is printed before the body, so fences always use three backticks. A nested block such as ```` ````markdown ```` from `mocks/markdown.md` renders correctly, but in copied text the inner `` ``` `` closes the outer block prematurely.
- [x] Validation: `make check test build` (58 tests), PTY `tui`, `menus`, `models`, `markdown`. Tests cover fences with/without a language, `~~~`, indented blocks, blocks inside lists and quotes, nested ```` ````markdown ````, faint fence styling, and code highlighting; streaming (including indented blocks, `~~~`, and nested fences) matches full rendering; selecting code in the view before/after resizing matches the source between fences. PTY `markdown` checks the lines between fences and their faint styling. Manual selection in a real terminal was not performed.

### Markdown code highlighting

Commit: `highlight streamed markdown code blocks`. Paths: `src/markdown.rs`, `src/view.rs`, Cargo files, `tests/cli.py`, `mocks/markdown.md`. Requires completed streaming Markdown; no API is needed.

- [x] Highlighting uses `syntect 5.3` (`default-features = false`, `parsing` + `regex-fancy`, without onig/C) and bat syntaxes from `two-face 0.5.2` (`syntect-fancy`): syntect's set lacks TypeScript, and the JavaScript grammar corrupts state on TS types. The syntax set loads lazily at the first block with a language. Rust, Python, TypeScript, Bash, JSON, SQL, and diff are highlighted; unknown/unspecified languages and indented blocks remain plain text.
- [x] The palette uses only ANSI-16 through classic SGR 31–36, with no backgrounds, 256/RGB, black/white, or bright colors, so previously printed history also follows theme changes. Headings are bold blue (default-color bold remains reserved for user messages), inline code is cyan without underlining, and links are unchanged. Faint (SGR 2): code block borders, table borders, separators, and quote prefixes. In code: keywords/storage are magenta, strings green, numbers and constants yellow, functions blue, types cyan, comments faint italic, and diff `+`/`-` green/red; everything else uses the default color. A small syntect theme selects scopes, storing the SGR code in the red channel.
- [x] Text, indentation, native wrapping, and `/copy` are unchanged; styling uses only SGR. Parser state lives in the committed renderer for each block and resets at every fence; mdtext emits code only as complete lines, appending `\n` to the last line on completion. Preview still redraws the open block from the start, but uses saved state for previously displayed lines, keeping styles consistent without quadratic highlighting. Each `Text::write` ends with a reset, preventing color from leaking into the next paragraph, editor, or summary.
- [x] Validation: `make check test build` (56 tests), PTY `tui`, `menus`, `models`, `markdown`, and the complete `mocks/markdown.md` through the PTY passed. Tests cover heading/inline-code/decoration palettes, Rust/Python/TypeScript/diff colors, plain text for unknown languages, multiline comments/strings, streaming (splits within lines and tokens) matching full rendering, committed + preview = completed prefix, and the absence of `38;`/`48;` or color leaks into the UI. The release binary grew from 1.4 → 4.2 MiB (3.5 MiB after strip); bat syntax data is ≈0.9 MiB. The first TypeScript block compiles regular expressions in ≈0.3 s. Mouse selection in a real terminal remains a manual check.

### Readable Markdown formulas

- [ ] Integrate text-based LaTeX rendering: single-line formulas within paragraphs, and fractions, roots, matrices, and `aligned` in separate blocks. Preserve normal scrolling, mouse selection, and original Markdown for `/copy`; leave unsupported formulas as source text without losing any parts.
- [ ] `mdtext 0.1.2` already recognizes formulas through `Options::MATH`, but this mode is currently disabled in clankr. The library emits LaTeX events; rendering belongs to the client, so a terminal-rendering PR to `mdtext` is not needed. During streaming, collect the entire block formula before final rendering.
- [ ] Choose and verify a renderer. A trial of [term-maths 1.0.0](https://docs.rs/term-maths/1.0.0/term_maths/) on 2026-10-09 showed that simple powers and fractions work, but `aligned` from `mocks/markdown.md` is truncated to `\begin{aligned} A`, and `\sqrt[3]{x}` becomes a square root with brackets. Upstream fixes or another renderer are needed; do not silently integrate it. `tui-math 0.1.1` is also not directly suitable: its required `ratatui 0.29` needs `unicode-width =0.2.0`, conflicting with the existing `unicode-width ^0.2.2`.
- [ ] Check `mocks/markdown.md`, stream splits within formulas, cancellation, ordinary `$5`/`$10`, unknown commands, long formulas, narrow windows/resizing. Formula styling must not leak into the next paragraph or input field.

### Submitted prompt history with draft restoration

Commit: `add prompt history with exact draft restoration`. Requires: the first TUI version.

Sources: `old:src/input.rs` (`History`) and the corresponding `App::edit` in `old:src/chat.rs`. Paths: `src/input.rs`, `src/chat.rs`, PTY runner.

- [ ] At document boundaries, Up/Down select submitted prompts; moving forward past the last entry restores the current draft's exact text and cursor offset. Editing a recalled prompt must not corrupt the saved new draft.
- [ ] Validation: `cargo test --release --locked --bin clankr input::tests`, PTY `history`: submit two multiline prompts, start a third, navigate backward/forward, and compare the restored text and cursor.

### Resume conversations after restart

Commit: `save and restore conversations atomically`. Requires: the first TUI version.

Sources: `Session::load/save/lock` and text session fixtures from `old:src/lib.rs`, and the CLI session path from `old:src/main.rs`. Paths: `src/lib.rs`, `src/main.rs`, `src/chat.rs`.

- [ ] Save successful text conversations and restore them on startup; `--session` and `CLANKR_HOME` select the file. Atomic saves, private permissions, an exclusive lock, and a 16 MiB limit are part of this feature.
- [ ] Do not treat the user's old files as disposable. Read supported text history from `old`; explicitly reject unknown versions, corrupt files, and unsupported tool/image blocks without overwriting them. Do not silently migrate old files or include executors that are not yet needed just for deserialization.
- [ ] Validation: `cargo test --release --locked --lib tests::session` and `cargo test --release --locked --bin clankr chat::tests`: save a fixture response, create a new App from the file, test a second writer, save failures, and corrupt/oversized/unsupported history while checking the original bytes. PTY `restart` uses an isolated HOME to open a prepared session with the normal CLI, checks the transcript after two launches, and exits without a model request. Isolate the lock test from unrelated parallel forks, as in the fixed version in `old`.

## Candidates: API integration

### Real streaming Codex responses in the existing TUI

Commit: `connect terminal chat to codex with streaming and cancellation`. Requires: the first TUI version.

The backend contract, streaming display, errors/cancellation, and an independent next draft are already implemented in the first version. Connect a real transport to that interface here and verify provider-specific behavior; do not rewrite the completed TUI.

Sources: read-only credentials, Codex requests, and indexed SSE from `old:src/providers.rs`; minimal shared Model/Message types from `old:src/lib.rs`; request lifecycle/capture/cancel from `old:src/chat.rs`; incremental drawing from `old:src/view.rs`. Paths: `src/providers.rs`, `src/lib.rs`, `src/main.rs`, `src/chat.rs`, `src/view.rs`, Cargo files.

- [ ] In the same normal `clankr`, submission calls Codex instead of echo and displays a streaming response. The next request includes the current process's completed turns. Authentication uses the existing read-only source; missing credentials produce a clear error without silently falling back to echo. The `ask` command and saving to disk are not required for this feature.
- [ ] The next draft remains editable during a response. Esc cancels the active request while preserving the next draft; pressing Enter again does not start a parallel request. Capture the active request's model/options/text at submission. Graceful exit waits for the worker to stop.
- [ ] On error, preserve received text, including the last queued batch; partial errors/cancellations do not become successful turns. Show the model, elapsed time, and only usage counters received from the provider. Stream updates do not reprint completed history; cursor-only events do not reprint text.
- [ ] Migrate correct SSE item/block matching, strict UTF-8, EOF/error handling, bounded buffers, and redaction. Do not migrate tools/images/Claude in advance. Check exact wire fields and defaults against the primary specification during implementation.
- [ ] Validation: `cargo test --release --locked --lib providers::tests`, `cargo test --release --locked --bin clankr chat::tests`, `cargo test --release --locked --bin clankr view::tests`. Using a loopback server and the production App, check two turns, the second request's history, UTF-8 splits, multiple items, 401/429/5xx, long streams, the next draft, cancel/exit, and an error after the last text chunk. Check for credential leaks and preserve the TUI. Record any real request with available credentials separately from offline fixtures.

### Claude with shared conversation history

Commit: `add claude with shared conversation history`. Requires: Codex in the TUI and conversation persistence.

Sources: Claude credentials, text wire format, and indexed SSE from `old:src/providers.rs`; Provider/Model parsing from `old:src/lib.rs`. Paths: `src/providers.rs`, `src/lib.rs`, `src/main.rs`, `src/chat.rs`.

- [ ] `--model claude` and explicit `provider/id` select another provider; previously saved text conversations actually appear in its request body. Do not migrate tool/image/effort fields that are not yet needed. Authentication remains read-only.
- [ ] Validation: `cargo test --release --locked --lib providers::tests` and `cargo test --release --locked --bin clankr chat::tests`: saved Codex turn → Claude fixture → continuation after reload; Claude usage, indexed chunks, error/EOF, and redaction. PTY `provider-switch` checks model selection through the CLI and rendering of saved history without a live request. Do not infer live model availability from a fixture.

### Connect real models to the existing menu

Commit: `connect the model picker to real providers`. Requires: providers and conversation persistence. The `/model` menu itself already works with mocks.

Sources: local catalog/manual selection from `old:src/models.rs`, model picker and deferred save from `old:src/chat.rs`. Paths: `src/models.rs`, `src/lib.rs`, `src/chat.rs`, `src/view.rs`.

- [ ] Connect real models to the existing `/model`, preserving independent effort/fast selection and the active request's captured settings. Save selection after success, error, and cancellation; define manual ID support and selection history during implementation. Do not present a local list as an API catalog of available models.
- [ ] Validation: `cargo test --release --locked models::tests`, PTY `models`: selection during a response, draft preservation, cancellation and reload with the new selection, manual IDs, and invalid IDs.

### Working effort settings

Commit: `add supported model effort controls`. Requires: model selection within a conversation.

Sources: `Options`, `Selection`, and exact-model capabilities from `old:src/models.rs`, option serialization from `old:src/providers.rs`. Paths: `src/models.rs`, `src/providers.rs`, `src/chat.rs`, `src/lib.rs`.

- [ ] Connect the existing `/effort` menu and Ctrl+, / Ctrl+. shortcuts to values actually supported by the provider and request serialization; custom models do not inherit other models' capabilities. The choice for the next turn is saved without changing the active turn. Check current models and fields against the primary specification during implementation; do not treat the list in `old` as permanently correct.
- [ ] Validation: `cargo test --release --locked models::tests`, `cargo test --release --locked --lib providers::tests`, PTY `effort`: exact wire format, unsupported/custom IDs, changes during a response, cancel/error, and restart.

### One-shot Codex requests through `ask`

Commit: `add stateless codex requests with streamed output`. Requires: a working Codex transport.

Sources: text `ask` from `old:src/main.rs`, Codex credentials/request/SSE from `old:src/providers.rs`, and the necessary Model/Message types from `old:src/lib.rs`. Paths: `src/main.rs`, `src/lib.rs`, `src/providers.rs`, Cargo files.

- [ ] `clankr ask 'prompt'` sends one text request, prints the stream, and returns a meaningful exit status. It uses the existing read-only authentication source; it neither reads nor writes history, executes tools, nor creates a login/refresh flow.
- [ ] Reuse the working transport with correct text SSE item/block matching, strict UTF-8, EOF/error handling, redaction, and bounded output. Preserve text already printed after an error. If other providers, skills, or tools already exist when selecting this task card, define their support in `ask` as part of the same feature without duplicating the transport.
- [ ] Validation: `cargo test --release --locked --lib providers::tests`, `cargo test --release --locked --bin clankr tests::ask`: loopback HTTP/SSE fixture, every UTF-8 split, multiple items, 401/429/5xx, Ctrl+C/cancellation, no session writes, and no credential leaks. Recheck exact wire fields and defaults during implementation; no new endpoint flag or credential store is needed.

## Candidates: skills, tools, and images

### Explicit skill inclusion by name

Commit: `include explicit skill instructions in model requests`. Requires: providers and conversation persistence.

Sources: `Catalog::discover/prepare`, token parser, and snapshots from `old:src/skills.rs`, and skill context for both providers. Paths: `src/skills.rs`, `src/lib.rs`, `src/providers.rs`, `src/chat.rs`, `src/main.rs`.

- [ ] `$name`, `$user/name`, and `$project/name` find skills only in the two agreed roots, load complete instructions into chat (and `ask`, if that command has been added), save a snapshot with the turn, and reproduce it after source changes/deletion and provider switching.
- [ ] Support symlink roots, project priority, ambiguity, YAML descriptions, per-file/total 1 MiB limits, and literal/code/escaped dollar signs. Errors in a selected skill preserve the draft. At this stage, resource paths are passed as context; resource reading arrives with `read_file`.
- [ ] Validation: `cargo test --release --locked --lib skills::tests`, `cargo test --release --locked --lib providers::tests`, PTY `skills`: artificial roots, both providers' wire formats, source rename/deletion, save/reload; unchanged stateless `ask` behavior if that command has been added.

### Connect real skills to the `$` menu

Commit: `connect the skill picker to discovered skills`. Requires: skill inclusion by name. The `$` menu, filtering, completion, and selected-token deletion already work with mocks.

Sources: mentions/pins from `old:src/skills.rs`, skill picker from `old:src/chat.rs`, and the shared popup. Paths: `src/skills.rs`, `src/chat.rs`, `src/view.rs`.

- [ ] Replace the mock catalog with real names, descriptions, and sources; connect the selected identity to prepared instructions. Preserve current-token completion without prompt submission and selection removal on deletion. A changed symlink or missing selected file must not silently switch to another skill.
- [ ] Validation: `cargo test --release --locked --bin clankr chat::tests`, PTY `skill-picker`: collisions between the two roots, pins after Unicode editing/history navigation, selection during streaming, and actual instructions in the next request.

### The model reads a file through `read_file`

Commit: `let the model read files with scoped permissions`. Requires: providers, skills, and conversation persistence; the skill picker is optional.

Sources: only the read/schema/Target parts of `old:src/tools.rs`, the minimal loop from `old:src/agent.rs`, typed calls/results and provider continuation from `old:src/lib.rs`/`providers.rs`, approvals and checkpoint acknowledgement from `old:src/chat.rs`. Paths: `src/tools.rs`, `src/agent.rs`, `src/lib.rs`, `src/providers.rs`, `src/chat.rs`, `src/main.rs`.

- [ ] Complete scenario: question → model tool call → temporary file read → tool result → model response, for both providers. Advertise only `read_file`; unknown/malformed calls return errors without execution. Restrict reading to regular files, 1 MiB of input, and bounded/truncated results.
- [ ] Read immediately within the workspace and resources of explicitly selected skills; outside them, request one-time permission for an exact preview. The UI remains available; Esc denies, and a second Esc cancels the turn. If `ask` has been added, it immediately rejects reads requiring approval. The preview displays control characters as unambiguous escapes; all lines remain accessible through scrolling/resizing.
- [ ] Integrate call/result pairing, persistence of completed facts, recovery of unfinished turns without automatic replay, isolated continuation when switching providers, bounded events, and limits of 20 rounds/100 calls from the start. Preserve facts already read if a later error occurs; checkpoint failure must not permanently leave the conversation with missing results.
- [ ] Validation: `cargo test --release --locked --lib agent::tests`, `cargo test --release --locked --lib providers::tests`, `cargo test --release --locked --lib tools::tests`, PTY `read-file`: skill references, approval/denial, symlinks/external paths, restart/provider switching, save failures, and the next request. The protocol, loop, and first read form one feature, not three commits that cannot work independently.

### Find files by pattern

Commit: `let the model find files by glob pattern`. Requires: the tool call loop and access rules from read_file.

Sources: the glob operation and bounded `Search::walk` from `old:src/tools.rs`. Paths: `src/tools.rs` and the corresponding agent fixture.

- [ ] Add `glob_files` to the schemas actually available. Respect roots/approval, skip `.git`/`target` and directory symlinks, and bound depth, visited entries, and output; explicitly report truncation/errors.
- [ ] Validation: `cargo test --release --locked --lib tools::tests`, PTY `glob-files`: the model finds the required file among fixtures and uses its path with the existing read operation.

### Search file contents

Commit: `let the model search file contents`. Requires: the tool call loop and bounded traversal from glob_files.

Sources: literal grep and shared traversal from `old:src/tools.rs`. Paths: `src/tools.rs` and the corresponding agent fixture.

- [ ] Add `grep_files` with literal search, file paths, and line numbers in results. Preserve traversal, total-read, and output limits; binary/unreadable paths must not cause endless traversal or silently count as complete success.
- [ ] Validation: `cargo test --release --locked --lib tools::tests`, PTY `grep-files`: find Unicode text, read the matched range, and respond with the correct source.

### Create and overwrite files with approval

Commit: `add approved atomic file writes`. Requires: the tool loop, approvals, and checkpoint persistence; glob/grep are optional.

Sources: write/Prepared/Target/atomic replacement from `old:src/tools.rs`, intent/result checkpoints from `old:src/agent.rs` and `chat.rs`. Paths: `src/tools.rs`, `src/agent.rs`, `src/chat.rs`, `src/lib.rs`.

- [ ] `write_file` creates or overwrites a file only after a single Ctrl+Y approval of the full exact preview. Denial changes nothing. The argument snapshot is immutable; changes to the path, symlink, contents, or permissions after preview require new approval. Limit content to 1 MiB and preserve permissions during atomic replacement.
- [ ] Save intent and receive acknowledgement before the side effect; save the result afterward. On save failure, stop new operations, preserve known results in memory, and honestly report unknown outcomes after a crash; never retry automatically. Validation and rename remain separate syscalls, without promising OS compare-and-swap.
- [ ] Validation: `cargo test --release --locked --lib tools::tests`, `cargo test --release --locked --lib agent::tests`, PTY `write-file`: create/overwrite/deny, bidi and literal escapes in filenames, changed targets, failures before/after the side effect, restart, and continuation without repeating the write.

### Exact replacement of a file fragment

Commit: `add approved exact file edits`. Requires: approved atomic writes.

Sources: `replacement`, edit prepare/execute, and ambiguous-match regression tests from `old:src/tools.rs`. Paths: `src/tools.rs` and the corresponding agent fixture.

- [ ] `edit_file` replaces exactly one exact match, with a preview of the old/new text and separate approval. Zero, multiple, or overlapping matches return an error, leaving the file intact. Reuse the verified checkpoint and atomic-write paths.
- [ ] Validation: `cargo test --release --locked --lib tools::tests`, PTY `edit-file`: read → unique edit → reread, plus denial, ambiguity, and file changes between preview and execution.

### Shell commands with output and cancellation

Commit: `add approved cancellable shell commands`. Requires: the tool loop, approvals, and checkpoint persistence; file writes/edits are optional.

Sources: shell/ProcessGroup/Capture and corrected lifecycle tests from `old:src/tools.rs`, worker events/approval from `old:src/agent.rs`. Paths: `src/tools.rs`, `src/agent.rs`, `src/chat.rs`, Cargo files.

- [ ] Every `shell` call shows the exact command/cwd and requires approval. Separate stdout/stderr, exit status, and elapsed time are available in the UI; retain up to 32 KiB per stream while draining the rest. Close stdin; impose no arbitrary runtime timeout, and show any explicitly supplied `timeout_ms` in the preview.
- [ ] Esc/exit/timeout and normal completion terminate the remaining owned process group and await cleanup/reaping of the direct child. Do not replace this with aborting a Tokio task. Preserve group identity protection until reap and macOS read-only EPERM → ESRCH cleanup; do not claim an OS sandbox or guarantees after SIGKILL/intentional detachment.
- [ ] Validation: `cargo test --release --locked --lib tools::tests`, `cargo test --release --locked --lib agent::tests`, PTY `shell`: stdout+stderr, large output, nonzero exits, denial, cancellation during fork, descendants with closed pipes, CLI exit, and another command after cleanup. All commands operate only on temporary fixtures.

### Inspect operations through `/tools`

Commit: `add a tool activity inspector`. Requires: working tools; read_file is enough to start.

Sources: records, `tools_menu`, and `Menu::details/reflow` from `old:src/agent.rs`, `chat.rs`, and `menu.rs`. Paths: `src/agent.rs`, `src/chat.rs`, `src/menu.rs`, `src/view.rs`.

- [ ] `/tools` shows current/recent calls, their states, arguments, and bounded results. All detail rows remain accessible after narrowing the window; closing restores the exact draft. The inspector shows only tools already implemented.
- [ ] Validation: `cargo test --release --locked --bin clankr chat::tests`, `cargo test --release --locked --bin clankr view::tests`, PTY `tools`: sequential read/edit/shell, success/deny/cancel, long Unicode arguments, and return to the draft.

### Image file attachments in conversations

Commit: `add persistent image file attachments`. Requires: providers and conversation persistence; tools are optional.

Sources: validated Snapshot/file/path from `old:src/images.rs`, attachment UI and history from `chat.rs`, native image wire format from `providers.rs`. Paths: `src/images.rs`, `src/lib.rs`, `src/providers.rs`, `src/chat.rs`, `src/view.rs`, Cargo files.

- [ ] `/attach PATH` accepts PNG/JPEG/WebP, including paths with spaces; `/attachments` explicitly removes the selected attachment. Image-only submission works. The byte snapshot actually reaches both providers' requests and the session; the next draft, cancellation, source deletion, and restart do not change an already submitted image.
- [ ] Validate before large allocations: 4 MiB input/image, 8 MiB of new bytes/turn, 8192 per side, 16 Mi pixels, 64 MiB decoded output, and 16 MiB session/request. Do not read FIFOs as endless streams or infer MIME types from extensions; describe decoder allocation budgets as best-effort. Validate snapshots on reload; read-only validation/rendering must not copy base64 unnecessarily.
- [ ] Validation: `cargo test --release --locked --lib images::tests`, `cargo test --release --locked --lib providers::tests`, PTY `images`: attach/remove/image-only, exact wire bytes for both providers, source deletion, restart, and the next draft. A fixture does not establish that the model recognized the image.

### Paste images from the macOS clipboard

Commit: `add asynchronous macos image paste`. Requires: file attachments.

Sources: raw NSPasteboard bridge and watchdog from `old:src/images.rs`, clipboard worker from `old:src/chat.rs`. Paths: `src/images.rs`, `src/chat.rs`, and Cargo files only if necessary.

- [ ] Ctrl+V reads the image clipboard once in a separate worker; input remains available, and submission waits for the read to finish. Do not read the clipboard during drawing. PNG/JPEG/WebP use the same validation; TIFF is normalized to PNG after bounded checks. The worker has bounded stdout and a 3 s watchdog with kill/reap; text/file URLs do not automatically become images.
- [ ] Validation: `cargo test --release --locked --lib images::tests`, `cargo test --release --locked --bin clankr chat::tests`, PTY `clipboard` with an explicit test double: success, empty buffer, invalid/large/TIFF input, timeout, the next draft, and exit. Physical paste from the macOS clipboard remains a manual check for this feature.

## New feature: wait when a model is unavailable

Automatic waiting for capacity/rate limits is absent from `old` but remains requested work. It requires a working API; migrating all other capabilities is not a prerequisite. Separate feature commit: `retry unavailable model requests without losing the conversation`.

Visible, cancellable waiting, `Retry-After` or backoff with jitter, retrying only the failed call with the captured request, without duplicating tools/side effects/history/text or submitting the next draft. Authentication/invalid-request errors must not loop. Define behavior after a partial stream and verifiable retry scenarios before implementation.

## Manual and external checks

Check native Ghostty resize/reflow, physical keys and mouse selection/copy, the actual macOS clipboard, and live Codex/Claude as the corresponding capabilities become available, and record them separately from offline checks. Do not claim the historical SSE snapshot conflict in `old` is fixed without a reproducing fixture or capture.

As related features arrive, check them together: skill → resource read → approved edit → shell cancellation → image → restart → provider switch. Fix discovered defects in separate meaningful fix commits; validation alone does not require a new commit.

## Plan status

The working TUI, menus, native response wrapping, and streaming Markdown are implemented. Continue developing the interface with mocks. Clean copying of user messages is implemented along with the new editor design. Next, fix code-block copying separately, then add syntax highlighting. APIs, real skills, session persistence, and tools remain separate candidates.
