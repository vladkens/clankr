# clankr

A minimalist terminal harness for AI agents. Follow [docs/roadmap.md](docs/roadmap.md) for the current task and its detailed plan.

## Code

- Prefer minimal, readable code and a small, coherent design. Use existing library capabilities before writing custom machinery; add dependencies or abstractions only when they simplify the complete solution. Keep local fixes local instead of redesigning adjacent subsystems.
- Keep the module tree shallow: place independent subsystems side by side under `src/`, with their related implementation files directly inside each module directory.
- Keep `agent` independent of terminal input and rendering. Backends emit events through `Backend`; `ui` consumes them, and `markdown` formats response text.
- Add, remove and update Rust dependencies through `cargo add`, `cargo rm` and `cargo upgrade`. Let Cargo update `Cargo.lock`; do not edit dependency declarations or the lockfile by hand.

## Terminal

- Keep the CLI on the terminal's main screen with native scrollback. Do not take over mouse selection, enter alternate screen or redraw completed history.
- Give terminal input and rendering one owner, and use the same grapheme/cell layout for editor navigation and display.
- Capture the active request's model, options, text, skills and images at submission. Editing the next draft must not change the running request.

## Checks

- For Rust changes, use `make check` and `make test`. `make prepare` rewrites files; it is not a check. Reuse passing results while the checked code is unchanged.
- Run `make test-cli CASE=<case>` only for affected terminal behavior during development. CI runs the full CLI suite in parallel; manual theme switching, mouse selection and clipboard checks remain separate.
- Measure binary size, memory and latency on the same input before accepting a performance change. Keep existing rendering and interaction behavior covered; do not weaken assertions to make a new implementation pass.

## Workflow

- Branch from `main` as `feat/<name>`, whatever the change. Each agent works in its own git worktree, so parallel tasks don't touch each other's files.
- Keep `old` as a read-only implementation reference; port needed behavior instead of merging it.
- Make each authorized commit one coherent, verified change, including its supporting tests and requested documentation. Use a short lowercase English subject without prefixes or a body.
- Fold corrections into the unfinished commit they repair only when amending or rewriting that commit is authorized. Integrate each task into `main` as one complete commit, then remove its branch; preserve `main` and `old`.
- Build in usable increments: keep the normal CLI runnable as each task progresses. Keep the README for users and planning in `docs/roadmap.md` and `docs/plans/`; do not add progress journals or duplicate plans.
