# clankr

A minimalist terminal harness for AI agents. All code is written by AI agents.

## Motivation

Claude Code and Codex are heavy, update too often, and have a complex set of rules and a lot of legacy baggage.

[pi.dev](https://pi.dev/) has an awful UI that's unpleasant to work with. It's also written in Node.js, which is a big downside.

[fx](https://fx.dev/) has a pleasant design, but limited functionality and model support. It's also written in Zig, whose creator [calls AI enthusiasts "tasteless"](https://andrewkelley.me/post/my-thoughts-bun-rust-rewrite.html) and whose project [bans AI-assisted contributions](https://ziglang.org/code-of-conduct/#strict-no-llm-no-ai-policy).

## Planned features

- Multiple providers and models sharing one conversation.
- Streaming Markdown, multiline editing, and native terminal scrollback.
- Reusable skills from project and user collections.
- File search, reading, editing, and shell commands with explicit permissions.
- Image attachments from files and the clipboard.
- Saved, resumable conversations.
- Automatic retries when models are busy or rate-limited.

## Contributing

Pull requests are welcome, including ones made with agents: point yours at [agents.md](agents.md).

## 🤖 How it's made

clankr is written by AI agents (Claude Code, Codex) following [agents.md](agents.md); I choose what to build and review every pull request. CI drives the release binary in a pseudo-terminal through scripted sessions with a mock model (menus, streamed Markdown, model switching, the queue) and checks the escape sequences it writes, the transcript and the terminal state it leaves ([tests/cli.py](tests/cli.py)). I've written software by hand for years ([macmon](https://github.com/vladkens/macmon), [twscrape](https://github.com/vladkens/twscrape), [ecloop](https://github.com/vladkens/ecloop)); agents are how I build now.

Releases are rare, and an update doesn't break what you rely on: the CLI, the config and your data stay compatible; a breaking change waits for a major version.
