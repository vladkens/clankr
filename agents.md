# clankr

A minimalist terminal harness for AI agents. [docs/roadmap.md](docs/roadmap.md) lists the tasks, each with one plan in `docs/plans/` holding its steps, open issues and research; the readme is for users.

- Keep `agent` independent of terminal input and rendering: backends emit events through `Backend`, `ui` consumes them and `markdown` formats response text. Keep the module tree shallow, one directory per subsystem under `src/`.
- Stay on the terminal's main screen with native scrollback: no alternate screen, no mouse capture, no redrawing completed history. Terminal input and rendering have one owner, and the editor and the display share one grapheme/cell layout.
- A request captures its model, options, text, skills and images at submission; editing the next draft never changes it.
- The local `old` branch is a read-only reference: port the behavior a task needs, never merge it.
- Keep a normal `clankr` launch usable at every step.
- Change dependencies only with `cargo add` / `cargo remove` / `cargo upgrade`, never by hand.
- `make check` and `make test` pass before every commit; for terminal behavior run the affected `make test-cli CASE=<case>` (CI runs them all). Measure binary size, memory and latency on the same input before accepting a performance change.
- Branch as `feat/<name>`; commits are one short lowercase subject without a prefix or body, and each task reaches `main` as one commit.
