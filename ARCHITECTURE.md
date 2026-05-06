# rift — Architecture

## Overview

The system is a native GPU-accelerated terminal application that connects via SSH to a remote host, attaches to tmux, and renders terminal output through GPUI — no WebView, no browser-based terminal emulation, no Node.js runtime.

Current state (Phase 1.5): single-window terminal connected directly via SSH. The daemon architecture is designed but deferred to Phase 3+.

Target architecture (Phase 3+): split into two processes connected by an SSH tunnel:

- **GPUI frontend** — a native application that handles all rendering, user interaction, and local compute (language servers).
- **Daemon** — a statically linked Linux binary that runs on the remote host, manages tmux, parses terminal output, and watches the filesystem.

## Agent-agnostic design

The system has no concept of "which coding agent is running." It sees tmux panes producing byte streams and a filesystem receiving changes. Whether Claude Code, Codex, OpenCode, Gemini CLI, or plain bash is running in a pane makes zero difference.

All IDE features derive from two universal signals:

- **PTY byte streams** — terminal output, parsed by the VTE layer into cell grids. Any process that writes to a terminal works.
- **Filesystem events** — file creation, modification, deletion. Any process that writes files triggers the file watcher, the file sync, the explorer update, and the LSP re-index.

This is a deliberate architectural constraint. There is no agent detection, no agent-specific event parsing, no protocol integration with any agent's internals. The agents are black boxes.

## Current architecture (Phase 1.5)

```
┌─────────────────────────────┐       ┌──────────────────────────────┐
│  Local host                  │       │  Remote host (WSL / VPS)      │
│                              │       │                               │
│  GPUI application            │  SSH  │  tmux server                  │
│  ├─ Terminal widget (GPUI)   │◄─────►│  └─ Shell / agents in panes   │
│  ├─ alacritty_terminal (VTE) │       │                               │
│  ├─ Tokio runtime (SSH I/O)  │       │                               │
│  └─ flume channel bridge     │       │                               │
└─────────────────────────────┘       └──────────────────────────────┘
```

### Rendering pipeline

1. SSH PTY stream delivers raw bytes from the remote shell.
2. Bytes are fed into `alacritty_terminal::Term` via a VTE parser — this handles all ANSI escape sequence processing, cursor movement, color attributes, and scrollback.
3. On each render frame, the GPUI terminal widget reads the cell grid from `Term` and paints characters with correct foreground/background colors, font weight, and underline/strikethrough styles.
4. Keyboard input is captured by GPUI, encoded as terminal escape sequences, and written back to the PTY stream.
5. Window resize triggers grid recalculation and PTY resize notification.

### Async bridge

GPUI has its own async executor. SSH I/O uses Tokio. These are bridged via `flume` channels on a dedicated OS thread:

- **PTY reader** (Tokio) → flume channel → GPUI side reads and feeds to alacritty_terminal
- **Keyboard input** (GPUI) → flume channel → Tokio side writes to PTY
- **Resize events** (GPUI) → flume channel → Tokio side resizes PTY

The two runtimes never share state beyond the channel. The `Term` instance is behind `Arc<Mutex<>>` — locked briefly by the PTY data receiver and by the render loop.

## Target architecture (Phase 3+)

```
┌─────────────────────────────┐       ┌──────────────────────────────┐
│  Windows host                │       │  Remote host (WSL / VPS)      │
│                              │       │                               │
│  GPUI frontend               │  SSH  │  Daemon (static musl binary)  │
│  ├─ Terminal renderer        │◄─────►│  ├─ tmux control mode client  │
│  ├─ File explorer            │  WS   │  ├─ VTE parser                │
│  ├─ Context menus            │       │  ├─ File watcher              │
│  ├─ Session bar              │       │  └─ File sync                 │
│  ├─ Local project files      │       │                               │
│  └─ Language servers         │       │  tmux server                  │
│                              │       │  Neovim (in panes)            │
└─────────────────────────────┘       └──────────────────────────────┘
```

When the daemon is introduced, VTE parsing may move server-side (daemon sends pre-parsed cell diffs) or remain client-side (daemon forwards raw PTY streams). That decision is deferred.

## Connection lifecycle (current)

1. Application reads SSH config from environment variables (`RIFT_SSH_HOST`, `RIFT_SSH_USER`, `RIFT_SSH_PORT`, `RIFT_SSH_KEY`).
2. Establishes SSH connection using `russh` (key-based auth).
3. Opens a PTY channel on the remote host.
4. Runs `tmux new-session -A -s rift` to create or reattach a session.
5. Bidirectional PTY I/O begins through the flume channel bridge.
6. UI goes live.

## Technology map

| Component | Crate / Technology |
|---|---|
| GUI framework | `gpui` (GPUI 0.2.2, Apache-2.0) |
| Terminal emulation | `alacritty_terminal` 0.26 |
| VTE parsing | `vte` (via alacritty_terminal) |
| SSH connection | `russh` |
| Async runtime | `tokio` |
| Channel bridge | `flume` |
| Serialization | `serde` + `serde_json` |

## Repository structure

```
rift/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── app/                # GPUI application binary
│   ├── ssh/                # SSH connection + PTY stream
│   ├── terminal/           # GPUI terminal widget + alacritty_terminal
│   ├── daemon/             # Remote daemon binary (Phase 3+)
│   ├── tmux-core/          # tmux control mode parser + state (Phase 3+)
│   ├── explorer/           # File watcher, git status, file sync (Phase 3+)
│   ├── protocol/           # Shared message types (Phase 3+)
│   └── plugin-api/         # Plugin trait for pane awareness (Phase 3+)
├── AGENTS.md
├── VISION.md
├── ARCHITECTURE.md
└── CLAUDE.md
```

## Commands

```bash
cargo build --workspace                                             # compile all
cargo clippy --workspace -- -D warnings                             # lint (zero warnings policy)
cargo fmt --all                                                     # format
cargo test --workspace                                              # test all
cargo run -p rift-app                                               # run GPUI app in dev mode
cargo build --release -p daemon --target x86_64-unknown-linux-musl  # daemon release build (Phase 3+)
```

## Cross-compilation and deployment

The daemon is compiled for `x86_64-unknown-linux-musl` (static linking). The GPUI app targets all platforms supported by GPUI: Linux (Vulkan/X11), macOS, and Windows natively.
