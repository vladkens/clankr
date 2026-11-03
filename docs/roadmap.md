# Roadmap

Build the terminal interface first, then bring the existing agent capabilities from `old` into it. Each row is one large task with one detailed plan; its implementation steps, open issues, and research stay in that plan.

| Task | Outcome | Status |
| --- | --- | --- |
| [Interface and rendering](plans/20261010-tui.md) | A usable terminal editor, menus, streamed output, Markdown, and copying, exercised with mocks. | Prepared |
| [Agents and conversations](plans/20261010-conversations.md) | Real provider responses, model settings, and conversations that survive restart and provider changes. | Current |
| [Skills and tools](plans/20261010-agent-tools.md) | An agent that uses explicit skills, works with files, and runs commands with visible results and approvals. | Future |
| [Image attachments](plans/20261010-images.md) | Images from files or the clipboard reach the model and remain part of the saved conversation. | Future |

The interface is prepared, including prompt history, request queuing, and submitted-message backgrounds. Codex now streams into the normal TUI with in-memory conversation context; `clankr --mock` retains the offline interface fixture. Next, connect Claude and shared conversation history, then add session persistence. Tools and image attachments build on conversations.

## Reference implementation

The local `old` branch contains working provider, session, model, skill, tool, and image code. Each plan names the relevant sources. Reuse the necessary behavior and regression coverage while preserving the new interface; do not copy interconnected modules wholesale. Check old model lists and provider fields against current specifications when integrating them.

## Working approach

- Follow the repository instructions in `agents.md`.
- Keep a normal `clankr` launch usable as the selected task progresses. Verify each change with focused checks and record manual or live acceptance separately.
- Add or update Rust dependencies through `cargo add`, selecting the latest available version; use `cargo add --dev` for development dependencies and `cargo rm` for removals. Let Cargo update the lockfile.
