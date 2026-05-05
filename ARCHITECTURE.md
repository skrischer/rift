# rift — Architecture

## Overview

The system is split into two processes connected by an SSH tunnel:

- **Tauri frontend** — a native Windows application (.exe) that handles all rendering, user interaction, and local compute (language servers).
- **Daemon** — a statically linked Linux binary that runs on the remote host (WSL, VPS, or any SSH-reachable machine), manages tmux, parses terminal output, and watches the filesystem.

This split is intentional. The remote host stays lightweight (tmux + Neovim + daemon, under 500 MB RAM). Heavy compute — language servers, GPU rendering, native UI — runs on the local machine where resources are abundant.

## Agent-agnostic design

The daemon has no concept of "which coding agent is running." It sees tmux panes producing byte streams and a filesystem receiving changes. Whether Claude Code, Codex, OpenCode, Gemini CLI, or plain bash is running in a pane makes zero difference to the daemon or the frontend.

All IDE features derive from two universal signals:

- **PTY byte streams** — terminal output, parsed by the VTE layer into cell grids. Any process that writes to a terminal works.
- **Filesystem events** — file creation, modification, deletion. Any process that writes files triggers the file watcher, the file sync, the explorer update, and the LSP re-index.

This is a deliberate architectural constraint. There is no agent detection, no Claude Code-specific event parsing, no protocol integration with any agent's internals. The agents are black boxes. They run unmodified in tmux panes. The IDE reacts to their side effects (file changes, terminal output, git state), not to their internal operations.

Consequence: if a new CLI coding agent ships tomorrow, it works in this IDE with zero code changes — as long as it runs in a terminal and edits files.

```
┌─────────────────────────────┐       ┌──────────────────────────────┐
│  Windows host                │       │  Remote host (WSL / VPS)      │
│                              │       │                               │
│  Tauri frontend (.exe)       │  SSH  │  Daemon (static musl binary)  │
│  ├─ Terminal renderer        │◄─────►│  ├─ tmux control mode client  │
│  ├─ File explorer            │  WS   │  ├─ VTE parser                │
│  ├─ Context menus            │       │  ├─ File watcher              │
│  ├─ Session bar              │       │  └─ File sync                 │
│  ├─ Local project files      │       │                               │
│  └─ Language servers         │       │  tmux server                  │
│                              │       │  Neovim (in panes)            │
└─────────────────────────────┘       └──────────────────────────────┘
```

## Connection lifecycle

1. User launches the Tauri app and selects a host from the connection list.
2. App establishes an SSH connection using `russh` (key-based or password auth, respects `~/.ssh/config`).
3. App checks if the daemon binary exists on the remote host and is the correct version. If not, it uploads the binary via SFTP.
4. App starts the daemon: `~/.local/bin/daemon --port 9500`.
5. SSH port forward: remote `:9500` → local `localhost:9500`.
6. WebSocket handshake over the forwarded port.
7. Daemon sends initial state: tmux sessions, pane layout, file tree snapshot.
8. UI goes live.

## Windows host — Tauri frontend

The frontend is a Tauri application compiled as a native Windows `.exe`. It handles:

### Terminal renderer

Each tmux pane is rendered as a cell grid (character + attributes + color per cell). The daemon sends pre-parsed cell data over the WebSocket. The frontend renders it via Canvas or WebGL — no VTE parsing on the client side.

Keyboard input is captured by the frontend and sent to the daemon, which writes it to the appropriate pane's PTY. Mouse events (click, scroll, drag) follow the same path. SGR mouse reporting is supported for Neovim integration.

### File explorer

Tree view panel showing the remote project structure. Data comes from the daemon's file watcher. Git status (modified, untracked, staged) is shown per file via color indicators. The tree updates reactively when the daemon reports filesystem changes — including changes made by coding agents.

Double-clicking a file sends a command to the daemon to open it in Neovim in the active pane.

### Context menus

Native right-click menus rendered by the OS, not the terminal. Available actions depend on context:

- **On a function call in terminal output:** Go to Definition, Find References, Rename Symbol.
- **On a file in the explorer:** Open, Open in Split, Copy Path, Reveal in Terminal.
- **On a pane border:** Split Horizontal, Split Vertical, Close Pane, Resize.

Menu actions translate to either LSP requests (handled locally) or tmux/Neovim commands (sent to daemon).

### Session bar

Tab bar showing tmux windows and sessions. Clicking switches the active view. Each pane shows a status indicator derived from the pane awareness system — active, idle, waiting for input, or error — along with the foreground process name. When plugins are loaded, the status is enriched with process-specific detail.

### Local project files

A synchronized mirror of the remote project directory, stored on the local Windows filesystem. Kept in sync by the daemon's file sync component (see below). Used exclusively by language servers for fast local file access.

Initial sync happens via `git clone` or rsync on first connect. Subsequent connections only sync deltas — typically under a second.

### Language servers

`tsserver`, `rust-analyzer`, and other language servers run as local Windows processes, reading from the local project mirror. This offloads RAM from the remote host.

LSP results (diagnostics, completions, hover info, go-to-definition targets) are used directly by the Tauri frontend for:
- Diagnostic indicators in the file explorer (error/warning icons per file).
- Context menu actions (Go to Definition sends coordinates to Neovim via daemon).
- Hover tooltips and signature help rendered as native popups.

## Remote host — Daemon

The daemon is a single statically linked binary (`x86_64-unknown-linux-musl`). No runtime dependencies — it runs on any Linux distribution. It manages four subsystems:

### tmux control mode client

Connects to tmux via control mode (`tmux -CC attach` or `new-session`). Parses the structured event stream (`%output`, `%session-changed`, `%window-add`, `%layout-change`, etc.) and maintains an internal state tree:

```
State
├─ Session "work"
│  ├─ Window 0 "code"
│  │  ├─ Pane 0 (active, 120x40, running: claude-code)
│  │  └─ Pane 1 (80x40, running: nvim)
│  └─ Window 1 "tests"
│     └─ Pane 0 (200x40, running: bash)
└─ Session "infra"
   └─ ...
```

State changes are pushed to the frontend over WebSocket as they happen.

### VTE parser

Each tmux pane produces a raw byte stream with ANSI escape sequences. The VTE parser (based on `alacritty_terminal`) translates each stream into a cell grid — the same internal representation Alacritty uses for rendering.

Cell grids are sent to the frontend as serialized diffs (only changed cells per frame), keeping bandwidth low even over remote connections.

### File watcher

Monitors the project directory using `notify` (inotify on Linux). Reports file creation, modification, deletion, and rename events. Integrates with `git2` to annotate events with git status.

Events serve two consumers:
- The frontend's file explorer (reactive tree updates).
- The file sync component (triggering delta pushes).

### File sync

Pushes filesystem changes to the Windows host's local project mirror over the WebSocket connection. The sync is unidirectional: remote → local. The local copy is read-only (used only by language servers).

Sync strategy:
- On connect: full delta sync (rsync-style checksum comparison).
- During session: incremental push triggered by file watcher events.
- Filters: respects `.gitignore`, excludes `node_modules`, `target/`, and other build artifacts.

## Pane awareness and plugins

The daemon maintains a status model per pane derived from two sources: tmux metadata (foreground process name, activity flags) and terminal output (byte flow rate, pattern matches). This gives the frontend enough information to show at a glance whether a pane is active, idle, waiting for input, or in an error state.

The core daemon is agent-agnostic — it tracks generic signals like output throughput and foreground process changes. For richer, process-specific awareness, the daemon supports a plugin interface.

A plugin registers which foreground processes it handles (e.g. `claude`, `codex`, `node`) and receives the pane's output stream. It returns structured status updates — what the process is doing, whether it's waiting for user input, whether it encountered an error. The daemon doesn't interpret these updates, it passes them to the frontend for display.

This means:
- With no plugins loaded, the daemon still provides basic status (active/idle based on output flow, foreground process name from tmux).
- With a `claude-code` plugin, the frontend knows when Claude Code is editing files, waiting for approval, or finished.
- With a `devserver` plugin, it knows when a dev server is listening, has crashed, or is rebuilding.
- Plugins are independent of each other and of the core. New plugins can be added without modifying the daemon.

The plugin API is defined in `crates/plugin-api/`. The daemon core depends only on this trait crate, never on specific plugin implementations. Plugins can be compiled into the daemon as optional cargo features or, in a later phase, loaded dynamically as WASM modules for community extensibility.

## Communication protocol

All communication runs over a single WebSocket connection, tunneled through SSH port forwarding.

Messages are JSON-encoded with a `type` discriminator:

```
// Frontend → Daemon
{ "type": "input",        "pane_id": 3, "data": "ls\n" }
{ "type": "resize_pane",  "pane_id": 3, "cols": 120, "rows": 40 }
{ "type": "tmux_command",  "cmd": "split-window -h" }

// Daemon → Frontend
{ "type": "pane_output",  "pane_id": 3, "cells": [...] }
{ "type": "state_update", "sessions": [...] }
{ "type": "file_event",   "kind": "modify", "path": "src/main.rs", "git_status": "modified" }
{ "type": "file_sync",    "path": "src/main.rs", "content": "..." }
```

The protocol may migrate to MessagePack if JSON serialization becomes a bottleneck.

## Technology map

| Component | Crate / Technology |
|---|---|
| SSH connection | `russh` |
| Terminal emulation | `alacritty_terminal` |
| VTE parsing (low-level) | `vte` |
| tmux CLI bindings | `tmux-interface` |
| File watching | `notify` |
| Git integration | `git2` |
| Directory traversal | `walkdir` |
| Gitignore filtering | `ignore` |
| LSP types | `lsp-types` |
| Async runtime | `tokio` |
| Serialization | `serde` + `serde_json` |
| GUI framework | Tauri v2 |
| Frontend rendering | TypeScript + Canvas/WebGL |

## Repository structure

```
rift/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── daemon/             # Remote daemon binary
│   ├── tmux-core/          # tmux control mode parser + state
│   ├── terminal/           # VTE parser, cell grid (alacritty_terminal wrapper)
│   ├── explorer/           # File watcher, git status, file sync
│   ├── protocol/           # Shared message types (used by both daemon and frontend)
│   └── plugin-api/         # Plugin trait for pane awareness (no implementations)
├── plugins/                # Optional pane awareness plugins (compiled as cargo features)
├── app/                    # Tauri frontend
│   ├── src-tauri/          # Rust backend for Tauri
│   └── src/                # TypeScript frontend
├── AGENTS.md
├── VISION.md
├── ARCHITECTURE.md
└── CLAUDE.md
```

## Cross-compilation and deployment

The daemon is always compiled for `x86_64-unknown-linux-musl` (static linking, no glibc dependency). The Tauri app is compiled for `x86_64-pc-windows-msvc`.

The daemon binary is embedded in the Tauri app bundle. On first connect to a new host, the app uploads it to `~/.local/bin/`. Version checks happen on every connect — if the embedded version is newer, the binary is re-uploaded automatically.
