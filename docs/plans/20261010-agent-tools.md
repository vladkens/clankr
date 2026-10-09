# Skills and tools

[Roadmap](../roadmap.md) · Future task · Depends on agents, conversations, and persistence

## Outcome

Turn the conversational client into an agent that uses explicitly selected skills, reads and changes workspace files, and runs shell commands. The user sees each operation, can approve or cancel it, and can resume a conversation without replaying completed side effects.

## Reference implementation

- `old:src/skills.rs`: discovery, scoped names, mentions, immutable instruction snapshots, and resource roots.
- `old:src/agent.rs`: bounded tool loop, records/events, intent/result checkpoints, and recovery.
- `old:src/tools.rs`: `read_file`, `glob_files`, `grep_files`, `write_file`, `edit_file`, `shell`, approvals, target identity checks, and process-group cleanup.
- `old:src/lib.rs` and `old:src/providers.rs`: typed calls/results, persistence, and provider continuation.
- `old:src/chat.rs` and `old:src/menu.rs`: approval interaction and operation inspection.

Use the existing `$` picker, request snapshots, and draft handling. Skills and tool capabilities share this task, but discovery and instruction inclusion can be integrated before execution. Individual tools are internal steps of the same agent capability.

## Work within this task

### Skill context

- [ ] Discover skills from the agreed user/project roots, including symlink roots. Support `$name`, `$user/name`, and `$project/name`, project priority, ambiguity reporting, YAML descriptions, and literal/code/escaped dollar signs. Keep per-file and total instruction limits of 1 MiB.
- [ ] Replace the mock catalog in the existing picker with real identities, descriptions, and sources. Preserve filtering, current-token completion, and selected-token deletion. Missing files or changed symlinks must not silently select a different skill; preparation errors preserve the draft.
- [ ] Capture full instructions with the submitted turn, send them to both providers, persist them, and reproduce the context after source edits/deletion and provider switching. Make selected resource paths available to `read_file` when it is integrated. Preserve stateless `ask` behavior if skill inclusion is supported there.

### Execution loop, permissions, and recovery

- [ ] Implement a complete model call → tool execution → result → model continuation cycle for both providers, starting with `read_file`. Advertise only implemented tools; malformed or unknown calls return errors without execution.
- [ ] Preserve call/result pairing, completed facts, bounded events, and limits of 20 rounds/100 calls. Recover unfinished turns without automatically replaying operations; keep provider-specific continuation isolated when switching providers.
- [ ] Read within the workspace and resources of explicitly selected skills directly. External reads require one-time approval for an exact preview. Esc denies the pending operation, and another Esc cancels the turn; the next draft remains available. Stateless `ask` rejects operations that require interactive approval.
- [ ] Show complete immutable previews for writes, edits, and shell commands; require Ctrl+Y approval. Escape control characters unambiguously and keep every preview row accessible when scrolling/resizing. Denial performs no side effect.
- [ ] Save intent and receive acknowledgement before a side effect, then save its result. On persistence failure, stop new operations and preserve known results in memory. Report uncertain outcomes after a crash honestly; never retry them automatically. Checkpoint failures must not permanently leave completed calls without their known results.

### Files and search

- [ ] Restrict `read_file` to regular files, 1 MiB input, and bounded/truncated results. Recheck symlinks and external paths according to the access rules; do not block on FIFOs.
- [ ] Add `glob_files` and literal `grep_files` through bounded traversal. Respect roots/approval, skip `.git`, `target`, and directory symlinks, and limit depth, visited entries, total reads, and output. Include file paths and line numbers; report truncation and unreadable/binary inputs without claiming complete results.
- [ ] `write_file` creates or overwrites at most 1 MiB through atomic replacement while preserving permissions. Changes to path, symlink, contents, or permissions after preview require new approval. Validation and rename remain separate system calls; do not claim atomic compare-and-swap.
- [ ] `edit_file` replaces exactly one exact match after its own preview. Zero, multiple, or overlapping matches return an error without changing the file. Reuse verified checkpoints, target checks, and atomic writes.

### Shell lifecycle and operation inspection

- [ ] Run `shell` only after approval of the exact command and working directory. Close stdin; display stdout/stderr, status, and elapsed time. Retain up to 32 KiB per stream while draining excess output. Impose no arbitrary runtime timeout; show any explicitly supplied `timeout_ms` in the preview.
- [ ] Cancel/exit/timeout and normal completion terminate remaining owned process-group members and reap the direct child. Keep group-identity protection until reap and the macOS read-only EPERM → ESRCH cleanup behavior. Do not substitute task abortion for process cleanup or claim an OS sandbox or cleanup after SIGKILL/intentional detachment.
- [ ] Connect `/tools` to current/recent calls, states, arguments, and bounded results. Keep long Unicode details readable after narrowing the window; closing restores the exact draft. Display only implemented operations.

## Acceptance

Exercise skill → resource read → search → approved write/edit → shell → final response for both providers, including denial and cancellation. Restart and switch providers without losing completed facts, substituting changed skill instructions, or repeating side effects. Confirm that errors leave the next draft editable and the agent can run another operation after cleanup.

Use temporary files, artificial skill roots, fake provider responses, and controlled subprocesses. Cover path changes after approval, ambiguous edits, checkpoint failures before/after side effects, large output, descendants with closed pipes, cancellation during fork, and shutdown. Focus terminal checks on previews and operation inspection; verify process and persistence behavior directly rather than through slow UI scripts.
