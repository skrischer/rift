# rift

An agent-centric IDE for terminal-based coding agents.

rift is a native GUI shell that treats terminal-based coding agents (Claude Code, Codex, OpenCode, Gemini CLI) as the primary interface and provides reactive IDE features around them. Agents run completely unmodified — rift reacts to their side effects (file changes, terminal output, git state), never to their internals.

**tmux is the engine, the GUI is the cockpit.**

## The problem

Terminal-based coding agents have changed how software gets written. But you're flying blind: the agent edits files you can't see, introduces errors you won't notice, and commits changes you have to `git log` to understand. Existing IDEs bolt AI on as a sidebar feature. Pure terminal setups lack visual feedback. Nothing combines native GUI performance, remote-first SSH, tmux multiplexing, and unmodified CLI agents.

## Architecture

Split into two processes connected via SSH:

```
┌───���─────────────────────────┐       ┌──────────────────────────────┐
│  Windows host               │       │  Remote host (WSL / VPS)     │
│                             │       │                              │
│  Tauri frontend (.exe)      │  SSH  │  Daemon (static musl binary) │
│  ├─ Terminal renderer       │◄─────►│  ├─ tmux control mode client │
│  ├─ File explorer           │       │  ├─ VTE parser               │
│  ├─ Language servers        │       │  ├─ File watcher             │
│  └─ Context menus           │       │  └─ File sync                │
└─────────────────────────────┘       └──────────────────────────────┘
```

- **Tauri frontend** — native Windows app handling rendering, UI, and local language servers
- **Daemon** — statically linked Linux binary managing tmux, parsing terminal output, watching the filesystem

The daemon is agent-agnostic. It sees PTY byte streams and filesystem events. Any CLI tool that runs in a terminal and edits files works with zero code changes.

## Tech stack

- **Language:** Rust (2021 edition), TypeScript (Tauri webview)
- **Async runtime:** Tokio
- **GUI framework:** Tauri v2
- **Terminal emulation:** alacritty_terminal
- **SSH:** russh
- **Build targets:** `x86_64-unknown-linux-musl` (daemon), `x86_64-pc-windows-msvc` (app)

## Repository layout

```
rift/
├── crates/
│   ├── daemon/       # Remote daemon binary
│   ├── ssh/          # SSH connection and PTY management
│   ├── tmux-core/    # tmux control mode parser + state
│   ├── terminal/     # VTE parser, cell grid
│   ├── explorer/     # File watcher, git status, file sync
│   ├── protocol/     # Shared message types
│   └── plugin-api/   # Plugin trait for pane awareness
├── plugins/          # Optional pane awareness plugins
├── app/              # Tauri frontend
└── .github/          # CI workflows
```

## Development

```bash
just build          # compile all crates
just lint           # clippy with zero warnings
just test           # run all tests
just ci             # fmt-check + lint + test
just run-daemon     # run daemon locally
```

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
7. **Native performance** — Rust + Tauri, no Electron

## Status

Early development. Phase 0 (scaffolding) complete. Phase 1 (SSH + terminal rendering MVP) in progress.

## License

MIT
