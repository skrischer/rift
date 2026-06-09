# rift — Architecture

## Overview

The system is a native GPU-accelerated terminal application that connects via SSH to a remote host, attaches to tmux, and renders terminal output through GPUI — no WebView, no browser-based terminal emulation, no Node.js runtime.

Current state (Phase 2): single-window terminal connected via SSH using tmux control mode (`-CC`). Event-driven notification processing, flow control, active pane tracking. The daemon architecture is designed but deferred to Phase 3+.

Target architecture (Phase 3+): split into two processes connected by an SSH tunnel:

- **GPUI frontend** — a native application that handles all rendering and user interaction.
- **Daemon** — a statically linked Linux binary that runs on the remote host, manages tmux, watches the filesystem, runs language servers, and parses terminal output.

## Agent-agnostic design

The system has no concept of "which coding agent is running." It sees tmux panes producing byte streams and a filesystem receiving changes. Whether Claude Code, Codex, OpenCode, Gemini CLI, or plain bash is running in a pane makes zero difference.

All IDE features derive from two universal signals:

- **PTY byte streams** — terminal output, parsed by the VTE layer into cell grids. Any process that writes to a terminal works.
- **Filesystem events** — file creation, modification, deletion. Any process that writes files triggers the file watcher, the explorer update, and LSP diagnostics.

This is a deliberate architectural constraint. There is no agent detection, no agent-specific event parsing, no protocol integration with any agent's internals. The agents are black boxes.

## Current architecture (Phase 2)

```
┌──────────────────────────────┐       ┌──────────────────────────────┐
│  Local host                  │       │  Remote host (WSL / VPS)     │
│                              │       │                              │
│  GPUI application            │  SSH  │  tmux server                 │
│  ├─ Terminal widget (GPUI)   │◄─────►│  └─ Shell / agents in panes  │
│  ├─ termy TmuxClient         │       │                              │
│  ├─ alacritty_terminal (VTE) │       │                              │
│  ├─ Tokio runtime (SSH I/O)  │       │                              │
│  └─ flume channel bridge     │       │                              │
└──────────────────────────────┘       └──────────────────────────────┘
```

### Rendering pipeline

1. SSH PTY channel runs `tmux -CC new-session -A -s rift` (control mode, no terminal echo).
2. termy's `TmuxClient` reads the control mode protocol stream, parses `%output` notifications, and decodes octal-escaped bytes.
3. `TmuxNotification::Output { pane_id, bytes }` delivers raw terminal output per pane via a flume wakeup channel.
4. An `OscInterceptor` (from `termy_terminal_ui`) extracts custom OSC sequences (working directory, shell integration) before passing filtered bytes to the VTE parser.
5. Filtered bytes are fed into `alacritty_terminal::Term` — this handles ANSI escape sequence processing, cursor movement, color attributes, and scrollback.
6. On each render frame, the terminal widget reads the cell grid from `Term`, converts cells to `termy_terminal_ui::CellRenderInfo`, and hands them to `TerminalGrid` for GPU-accelerated rendering with box-drawing geometry, shaped-line caching, and paint-damage optimization.
7. Keyboard input is captured by GPUI, encoded as terminal escape sequences, and sent to the active tmux pane via `TmuxClient::send_input()`.
8. Mouse events are routed to the PTY (when terminal mouse mode is active) or handled locally (text selection, Ctrl+click link opening).
9. Window resize triggers grid recalculation and `TmuxClient::set_client_size()`.

### Async bridge

GPUI has its own async executor. SSH I/O uses Tokio. termy's `TmuxClient` uses blocking I/O with `PtySyncReader`/`PtySyncWriter`. These are bridged via `flume` channels and dedicated OS threads:

- **tmux output** — poll thread receives wakeup, calls `TmuxClient::poll_notifications()`, sends `%output` bytes via flume to GPUI
- **Keyboard input** (GPUI) → flume channel → input thread calls `TmuxClient::send_input()`
- **Resize events** (GPUI) → flume channel → resize thread calls `TmuxClient::set_client_size()`
- **Snapshots** — poll thread refreshes on `NeedsRefresh` notification, sends `TmuxSnapshot` via flume to GPUI for CWD and active pane tracking

The two runtimes never share state beyond the channels. The `Term` instance is behind `Arc<Mutex<>>` — locked briefly by the PTY data receiver and by the render loop.

## tmux control-mode interaction model

The decision to drive tmux through control mode (`-CC`) rather than as a normal rendered terminal shapes every terminal interaction feature. It was previously only implicit ("tmux-native" in `vision.md`, "the only documented programmatic interface" in `prior-art.md`); this section records it as a deliberate architecture decision with its alternative and exit.

**The decision and why.** `-CC` delivers *structure as a protocol*: per-pane `%output` byte streams, `%layout-change` geometry, window/pane lifecycle notifications, and flow control. On top of that rift gets native tmux session persistence, multi-client, and remote-over-SSH semantics for free. This structure is the foundation for everything rift is: each pane drives its own `alacritty_terminal::Term`, the split tree is built from tmux coordinates, and per-pane awareness becomes possible. Without a structured stream none of that exists.

**The rejected alternative.** Running real tmux in a single PTY and rendering its TUI natively would inherit copy-mode, configured key bindings, and the status line for free — but rift would see a *single character grid* with no pane structure. It would have to recover pane boundaries by screen-scraping rendered box-drawing characters and parsing the status line: fragile, theme-dependent, and the exact anti-pattern rift forbids for agents, turned on tmux itself. It deletes rift's reason to exist. The tension is fundamental *per tmux attach mode* — one attach gives you the control stream **or** the rendered TUI, never both — which is why rift takes the control stream and re-provides the interactive features as GUI affordances (see `spec-terminal-interaction-fixes.md`).

**The durable contract (consequences for features).**

- `send-keys -t <pane> -H <hex>` injects bytes straight into the pane PTY, bypassing tmux's key tables — so configured keybindings need an explicit mirror (see `spec-tmux-keytable-mirroring.md`).
- copy-mode/choose-mode are not rendered to control clients — so scrollback is fetched via `capture-pane`, not forwarded.
- `-CC` exposes a single client size (`refresh-client -C`) — so font zoom is a whole-client resize, not per-pane.
- pane geometry is tmux-owned — so resize/zoom go through `resize-pane` / `resize-pane -Z`.
- all input and command emission flows through one narrow seam (today `TmuxClient` via flume channels) so the Phase 3 transport swap (`TmuxClient` → daemon protocol) is a single-seam change.

**Exit criteria.** The single-seam interface keeps the choice reversible. If `-CC` parsing/state ever becomes a maintenance burden (the trigger already named in `prior-art.md`), evaluate the WezTerm-mux RPC protocol as a structured substrate that drops tmux while keeping a protocol — *before* any raw-PTY-from-scratch multiplexer, which would make rift "another tmux replacement" (the thing `vision.md` defines rift against). Do not pre-spend on this.

## Target architecture (Phase 3+)

```
┌─────────────────────────────┐       ┌──────────────────────────────┐
│  Windows host                │       │  Remote host (WSL / VPS)      │
│                              │       │                               │
│  GPUI frontend               │  SSH  │  Daemon (static musl binary)  │
│  ├─ Terminal renderer        │◄─────►│  ├─ tmux control mode client  │
│  ├─ File explorer            │ russh │  ├─ VTE parser                │
│  ├─ Context menus            │       │  ├─ File watcher (inotify)    │
│  └─ Session bar              │       │  ├─ Git status                │
│                              │       │  └─ Language servers (LSP)    │
│                              │       │                               │
│                              │       │  tmux server                  │
│                              │       │  Neovim (in panes)            │
└─────────────────────────────┘       └──────────────────────────────┘
```

### Why LSP runs on the remote

Language servers need access to the full project environment — `node_modules`, `target/`, `venv/`, `$GOPATH` — to resolve types and dependencies. These directories are not in git, platform-specific, and often gigabytes in size. Syncing them to the local host would require either mirroring the entire dependency tree (hundreds of MB, platform mismatches) or running a parallel package install locally. Every other remote-capable IDE (VS Code Remote, JetBrains Gateway, Zed) runs LSP on the remote for this reason.

The daemon starts language servers on demand and forwards diagnostics as lightweight JSON over a dedicated `russh` channel (russh already multiplexes channels, so no extra framing layer is needed). No file sync, no local project copies, no path translation.

When the daemon is introduced, VTE parsing may move server-side (daemon sends pre-parsed cell diffs) or remain client-side (daemon forwards raw PTY streams). That decision is deferred.

## Connection lifecycle (current)

1. Application reads SSH config from environment variables (`RIFT_SSH_HOST`, `RIFT_SSH_USER`, `RIFT_SSH_PORT`, `RIFT_SSH_KEY`).
2. Establishes SSH connection using `russh` (key-based auth).
3. Opens a PTY channel via `channel.exec()` (not interactive shell).
4. Runs `tmux -CC new-session -A -s rift` — control mode, creates or reattaches session.
5. termy's `TmuxClient::from_streams()` wraps the PTY reader/writer via `PtySyncReader`/`PtySyncWriter`.
6. Flow control activated: `refresh-client -f pause-after=5`.
7. Initial `TmuxSnapshot` fetched for active pane ID and working directory.
8. Three worker threads start: input routing, resize forwarding, notification polling.
9. UI goes live — poll thread processes `%output`, `NeedsRefresh`, `Exit` notifications.

## Technology map

| Component | Crate / Technology |
|---|---|
| GUI framework | `gpui` (from Zed git, Apache-2.0) |
| Terminal rendering | `termy_terminal_ui` (MIT) — grid painting, link detection, OSC interception, shell integration, tmux control mode client |
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
│   ├── terminal/           # Terminal widget wrapping alacritty_terminal + termy_terminal_ui
│   ├── daemon/             # Remote daemon binary
│   ├── tmux-core/          # tmux control mode parser + state (currently using termy's TmuxClient directly)
│   ├── explorer/           # File watcher, git status — library used by daemon
│   ├── protocol/           # Shared message types. Serializable with serde
│   └── plugin-api/         # Plugin trait for pane awareness (Phase 3+)
├── AGENTS.md
├── CLAUDE.md               # Symlink → AGENTS.md
└── docs/                   # Architecture, specs, roadmap, reference docs
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

The daemon is compiled for `x86_64-unknown-linux-musl` (static linking). The target is declared in `rust-toolchain.toml`, so `rustup` installs it automatically; the daemon is a pure-Rust binary that links self-contained via Rust's bundled linker, so no `musl-gcc`/`musl-tools` is required locally. Build it with `just release-daemon`. The CI `daemon-musl` job builds the same artifact on each PR to keep the cross-compile reproducible. The GPUI app currently targets Windows (`x86_64-pc-windows-gnu`, cross-compiled from WSL via MinGW) and Linux (Vulkan/X11); macOS is supported by GPUI but deferred for rift. The primary dev loop builds the app in WSL and runs the resulting `.exe` on the Windows host.
