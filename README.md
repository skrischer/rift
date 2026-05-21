# rift

An agent-centric IDE for terminal-based coding agents.

rift is a native GUI shell that treats terminal-based coding agents (Claude Code, Codex, OpenCode, Gemini CLI) as the primary interface and provides reactive IDE features around them. Agents run completely unmodified — rift reacts to their side effects (file changes, terminal output, git state), never to their internals.

**tmux is the engine, the GUI is the cockpit.**

## The problem

Terminal-based coding agents have changed how software gets written. But you're flying blind: the agent edits files you can't see, introduces errors you won't notice, and commits changes you have to `git log` to understand. Existing IDEs bolt AI on as a sidebar feature. Pure terminal setups lack visual feedback. Nothing combines native GUI performance, remote-first SSH, tmux multiplexing, and unmodified CLI agents.

## Architecture

Currently a single GPUI application that connects via SSH and attaches to tmux in control mode (`-CC`). Target architecture (Phase 3+): split into GPUI frontend + remote daemon.

```
+---------------------------------+       +--------------------------------+
|  Local host                     |       |  Remote host (WSL / VPS)       |
|                                 |       |                                |
|  GPUI application               |  SSH  |  tmux server                   |
|  +- Terminal widget (GPUI)      |<----->|  +- Shell / agents in panes    |
|  +- alacritty_terminal (VTE)    |       |                                |
|  +- termy tmux control client   |       |                                |
|  +- flume channel bridge        |       |                                |
+---------------------------------+       +--------------------------------+
```

The app connects via SSH, launches `tmux -CC` (control mode), and processes the structured event stream. Terminal output arrives as `%output` notifications per pane, parsed by `alacritty_terminal` and rendered through GPUI. The system is agent-agnostic — any CLI tool that runs in a terminal works with zero code changes.

## Tech stack

- **Language:** Rust (2021 edition)
- **Async runtime:** Tokio
- **GUI framework:** GPUI (from Zed, GPU-accelerated native rendering)
- **Terminal emulation:** alacritty_terminal
- **tmux integration:** termy_terminal_ui (control mode client)
- **SSH:** russh
- **Build target:** Linux/X11 (GPUI native)

## Repository layout

```
rift/
+-- crates/
|   +-- app/          # GPUI application binary
|   +-- ssh/          # SSH connection and PTY management
|   +-- terminal/     # Terminal widget (alacritty_terminal + termy_terminal_ui)
|   +-- daemon/       # Remote daemon binary (Phase 3+)
|   +-- tmux-core/    # tmux control mode state (Phase 3+, currently using termy)
|   +-- explorer/     # File watcher, git status, file sync (Phase 3+)
|   +-- protocol/     # Shared message types (Phase 3+)
|   +-- plugin-api/   # Plugin trait for pane awareness (Phase 3+)
+-- .claude/docs/     # Architecture docs and roadmaps
```

## Development

```bash
cargo build --workspace                      # compile all
cargo clippy --workspace -- -D warnings      # lint (zero warnings policy)
cargo fmt --all                              # format
cargo test --workspace                       # test all
cargo run -p rift-app                        # run GPUI app in dev mode
```

SSH connection is configured via environment variables: `RIFT_SSH_HOST`, `RIFT_SSH_USER`, `RIFT_SSH_PORT`, `RIFT_SSH_KEY`.

## Branching

- `main` — production-ready, merges from `develop` via PR
- `develop` — integration branch, receives feature PRs
- Feature branches — `feat/<scope>`, `fix/<scope>`, `chore/<scope>`

## Core principles

1. **Agent-first** — the terminal agent is the primary actor
2. **Vanilla agents** — CLI agents run completely unmodified and are interchangeable
3. **Open source and free** — always, no exceptions
4. **Reactive** — IDE features update automatically from terminal and filesystem signals
5. **tmux-native** — don't reinvent multiplexing
6. **Remote-first** — SSH is the default, local is a special case
7. **Native performance** — Rust + GPUI, no Electron, no web runtime overhead

## Status

Phase 2 (tmux control mode integration) complete. SSH connection to remote tmux via control mode (`-CC`), event-driven notification processing, flow control, active pane tracking, terminal rendering through GPUI. Phase 2c (multi-pane awareness) in progress.

## License

GPL-3.0-or-later
