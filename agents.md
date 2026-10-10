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

- Agree tasks with the user, keep the high-level scope in `docs/roadmap.md` and the actionable checklist in `docs/plans/yyyymmdd-<name>.md`. Track execution by ticking checkboxes, without progress or status prose. Keep the README for users; do not add progress journals or duplicate plans.
- The orchestrator writes bounded tasks and delegates implementation to other agents. Keep the user informed about who is doing what, where to inspect the work, and what remains. Handle small fixes directly when useful; leave substantial implementation to the assigned agent.
- Branch independent work from `main` as `feat/<name>`, whatever the change. Branch dependent work from the feature branch it builds on. Each agent works in its own git worktree, so parallel tasks don't touch each other's files.
- Keep `old` as a read-only implementation reference; port needed behavior instead of merging it.
- Build in usable increments and keep the normal CLI runnable. During authorized plan execution, any agent assigned to the task may commit each verified step, including supporting tests and requested documentation. Use a short lowercase English subject without prefixes or a body. Fix commits are fine on the branch; amend or rewrite commits only when authorized.
- Push the feature branch and open one PR per feature or roadmap phase with `gh pr create`. Target `main` for independent work and the parent feature branch for dependent work. Use a short lowercase title suitable for the squash commit and a brief body describing the change and validation, with measurements for performance work.
- The orchestrator reviews the diff and verification evidence, runs missing checks and sends findings back to the implementing agent until resolved. Reuse passing checks for unchanged work; do not accept an agent's conclusion without inspecting its evidence.
- The orchestrator follows CI to completion and handles Copilot review. If Copilot has not reviewed automatically, request it when available. Fix confirmed findings, answer the rest in their threads and hand the ready PR to the user with its link and any remaining limitations.
- The user performs the final review and squash-merges one complete commit into `main`. An agent may merge only on an explicit request. Never commit or push directly to `main`.
- Resolve conflicts by rebasing onto the PR's base and verifying affected work. After a parent PR merges, rebase dependent branches onto `main` and retarget their PRs. Use `--force-with-lease` only for feature branches.
- Remove completed feature branches and their worktrees after merging. Preserve `main`, `old` and unfinished work.
