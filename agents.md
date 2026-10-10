# clankr

- Prefer minimal, readable code and a small, coherent design. Use existing library capabilities before writing custom machinery; add dependencies or abstractions only when they simplify the complete solution. Keep local fixes local instead of redesigning adjacent subsystems.
- Keep the module tree shallow: place independent subsystems side by side under `src/`, with their related implementation files directly inside each module directory.
- Develop each feature or fix on a dedicated branch. Every working commit must be a coherent, verified part of that change.
- Fold corrections to unfinished work into the commit they repair using amend or fixup/rebase; do not leave separate cleanup commits for mistakes in that same work.
- Squash the feature branch into one complete feature or fix commit when integrating into `main`. Include the supporting tests and documentation in that change.
- Keep the CLI on the terminal's main screen with native scrollback. Do not take over mouse selection, enter alternate screen or redraw completed history.
- Give terminal input and rendering one owner, and use the same grapheme/cell layout for editor navigation and display.
- Capture the active request's model, options, text, skills and images at submission. Editing the next draft must not change the running request.
