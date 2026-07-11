# rift — Architecture

## Overview

The system is a native GPU-accelerated terminal application that connects via SSH to a remote host, attaches to tmux, and renders terminal output through GPUI — no WebView, no browser-based terminal emulation, no Node.js runtime.

Current state (Phase 2): single-window terminal connected via SSH using tmux control mode (`-CC`). Event-driven notification processing, flow control, active pane tracking. The daemon architecture is designed but deferred to Phase 3+.

Target architecture (Phase 3+): split into two processes connected by an SSH tunnel:

- **GPUI frontend** — a native application that handles all rendering, editing, and user interaction.
- **Daemon** — a statically linked Linux binary that runs on the remote host, manages tmux, watches the filesystem, runs language servers, serves file buffers to the editor, and parses terminal output.

## Agent-agnostic design

The system has no concept of "which coding agent is running." It sees tmux panes producing byte streams and a filesystem receiving changes. Whether Claude Code, Codex, OpenCode, Gemini CLI, or plain bash is running in a pane makes zero difference.

All IDE features derive from three universal, agent-agnostic signals:

- **PTY byte streams** — terminal output, parsed by the VTE layer into cell grids. Any process that writes to a terminal works.
- **Filesystem events** — file creation, modification, deletion. Any process that writes files triggers the file watcher, the explorer update, and LSP diagnostics.
- **Host resource state** — the host's own CPU / memory / swap / load, read from `/proc` by the daemon (Phase 43). A host-global observable of the machine the agents run on, not of any agent: `/proc` knows nothing about which process is Claude Code vs `cargo`. Attributing usage to a tmux pane (Phase 45) keys on the pane's process subtree (`pane_pid`), never on agent identity.

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
┌───────────────────────────┐       ┌────────────────────────────────┐
│  Windows host             │       │  Remote host (WSL / VPS)       │
│                           │       │                                │
│  GPUI frontend            │       │  Daemon (static musl binary)   │
│  ├─ Terminal renderer     │       │  ├─ tmux control mode client   │
│  ├─ Editor + diagnostics  │       │  ├─ VTE parser                 │
│  ├─ File explorer         │       │  ├─ File watcher (inotify)     │
│  ├─ Context menus         │  SSH  │  ├─ Git status                 │
│  └─ Session bar           │◄─────►│  ├─ Language servers (LSP)     │
│                           │       │  └─ File buffer service        │
│                           │       │                                │
│                           │       │  tmux server                   │
│                           │       │  Agents, dev servers, scripts  │
└───────────────────────────┘       └────────────────────────────────┘
```

Since phase 35 the daemon holds **one reactive context per project root** (a
worktree snapshot, git status, and language servers), reference-counted by the
sessions attached to it — the Zed `HeadlessProject` / `WorktreeStore` shape (one
server, N per-root contexts). The watched root follows the active tmux session
(see the Connection robustness contract below), so it is no longer a single
process-global root bound at daemon start (`spec-per-session-project-root.md`).

### Host resource telemetry (a daemon-global signal)

The worktree / git / diagnostics / LSP-health pushes are **per-context** state —
they belong to a project root. Host resource state (CPU / memory / swap / load) is
different: it is a property of the **machine**, not of any root, so it is a
**daemon-global** signal (Phase 43, `spec-host-telemetry.md`). One sampler per
daemon process reads `/proc` via `sysinfo` on a fixed interval (`spawn_blocking`,
connection-gated so an idle daemon polls nothing), caches the latest sample in a
process-global `watch`, and pushes a push-only `HostMetrics` on a daemon-global bus
that every connection drains **alongside** its per-context bus — replayed once
behind `welcome` like `lsp_status`. So two app instances attached to different
roots on the one shared daemon see the same host figure from a single sample, never
one `/proc` read per context.

### Why LSP runs on the remote

Language servers need access to the full project environment — `node_modules`, `target/`, `venv/`, `$GOPATH` — to resolve types and dependencies. These directories are not in git, platform-specific, and often gigabytes in size. Syncing them to the local host would require either mirroring the entire dependency tree (hundreds of MB, platform mismatches) or running a parallel package install locally. Every other remote-capable IDE (VS Code Remote, JetBrains Gateway, Zed) runs LSP on the remote for this reason.

The daemon starts language servers on demand and forwards diagnostics as lightweight JSON over a dedicated `russh` channel (russh already multiplexes channels, so no extra framing layer is needed). No project mirroring, no local copies of the dependency tree, no path translation.

**File contents and the worktree path are kept separate, on purpose.** The explorer / worktree path carries *structure* — the file tree, ignore status, mtimes, git status, diagnostics — as lightweight messages, and **never file contents**. File contents move only over a deliberate, request/response **buffer channel** that the editor opens: read a file on open, write it back on save (see "The GUI is the editor" below). This is why the placeholder `FileSync { content }` push was removed from the worktree protocol (#107) — unsolicited content on the structure path was the wrong design; editing is an explicit pull/push on its own channel.

Today the daemon's LSP reads document state from **disk** (agents save their edits, so disk is the source of truth) — correct for the read-only diagnostics data layer being built now. Once the editor lands, the document source-of-truth shifts from disk to the **rift buffer**: the editor sends `didChange` for unsaved edits so diagnostics track the live buffer, not just the last save. Work on the current data layer must not bake in a permanent "LSP only ever reads disk" assumption.

When the daemon is introduced, VTE parsing may move server-side (daemon sends pre-parsed cell diffs) or remain client-side (daemon forwards raw PTY streams). That decision is deferred.

## The GUI is the editor (the process runtime stays tmux)

> Decision recorded 2026-06-10. Supersedes the earlier "Neovim handles editing; rift is not a text editor" stance in `vision.md`.

rift's GUI is a first-class editor: it reads and writes the project's files directly, with LSP-grade navigation and edits (go-to-definition, hover, references, rename, format, code actions), and saves back to the remote. The split stays clean:

- **tmux is the process runtime.** Coding agents, dev servers, and build/test scripts all run in tmux panes — persistent, remote, multi-pane. This is unchanged and central; tmux is *not* demoted to "agent runner".
- **rift is the cockpit and the editor.** Rendering, file viewing/editing, diagnostics, git, navigation — the GUI surface.

**What this overturns** (from `vision.md`): "Not another text editor" and "Neovim runs inside the terminal panes and handles editing." rift now edits.

**What stays** (and is strengthened): "tmux is the engine, the GUI is the cockpit" — now cleaner, since the engine is the *process runtime* and the cockpit gains editing. "Vanilla agents", "remote-first", "reactive awareness", and "tmux-native" are untouched. The differentiator is explicitly *not* the editor surface (Zed and VS Code Remote do that well): it is that rift's process layer is real tmux running vanilla CLI agents. Editors eat roadmaps — the editor must never let "compete with Zed on editor features" pull the roadmap off that axis, so the future editor spec carries a hard not-in-v1 list.

**Neovim** is not integrated and is not a component: it is just another process a user may run in a pane (agent-agnostic — panes are black boxes). Terminal-driven editing keeps working with zero rift code; rift adds a GUI editor, it does not forbid the terminal one. No Neovim protocol, plugin, or special-casing — that would smuggle process-specific code into the agent-agnostic core, the one thing this project forbids.

### Named concern: concurrent writes (agent ↔ human)

Because the agent (in a pane) and the human (in rift's editor) can write the same file, the editor needs a "file changed under you" strategy. The signal already exists: the worktree snapshot carries per-entry `mtime` (#107), so a worktree-changed update for a path open in the editor is exactly the detector —

- **Buffer clean** → silent auto-reload. This is a *feature*, not a hazard: you watch the agent edit your open file live.
- **Buffer dirty** → conflict UI (the Zed model): surface the divergence and let the user reconcile.

The mechanism is sketched here; the solution is owned by the editor spec, not built now.

### File buffer channel

Editing uses a deliberate request/response buffer channel over the daemon transport: read a file's content on open, write it back on save (whole-file for v1, with an `mtime` conflict check). This is distinct from — and must not be folded into — the worktree/explorer structure path, which never carries contents (see "Why LSP runs on the remote").

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

## Connection robustness contract (phase 20)

> Ratified with `spec-connection-robustness.md` (2026-07-05). Governs every
> transport seam; supersedes the earlier quit-on-disconnect behavior. Amended by
> `spec-post-connect-picker.md` (phase 33, 2026-07-09) — the session-pick step and
> its re-Attach precondition —, by `spec-per-session-project-root.md` (phase 35,
> 2026-07-09) — the current-session watch also driving the daemon's watched root
> (marked below) —, and by `spec-session-lifecycle.md` (phase 40, 2026-07-10) — the
> mid-session sessionless state and the session-end-vs-transport-loss distinction
> (marked below).

- **Protocol versioning:** `PROTOCOL_VERSION` (crates/protocol) reflects the
  message set — every message-set change bumps it, enforced by a pinned
  fingerprint test in the protocol crate. Client and daemon enforce strict
  equality at Hello/Welcome; the daemon answers a mismatched Hello with its
  own version and closes cleanly (never streams to a mismatched client).
- **The client owns the daemon version:** on mismatch with a running daemon,
  the client stops it (pidfile), re-deploys the matching binary
  (content-fingerprinted), respawns, and reconnects. The daemon never
  self-updates.
- **No silent stream death:** a dead daemon channel (EOF, malformed frame)
  while SSH is up auto-reconnects and resyncs via the Welcome snapshot replay
  plus a terminal re-Attach (fresh-LayoutSnapshot reset). An SSH drop enters a
  visible bounded-backoff reconnect loop (`ConnectionStatus::Reconnecting`)
  instead of quitting; tmux session persistence makes the terminal lossless
  across it. _(Phase 33)_ The re-Attach targets the current-session watch; when it
  is unset — a post-connect session pick not yet made — the reconnect re-shows the
  session picker instead of blind-attaching, and re-attaches the picked session
  once it is seeded.
- **Not-connected is a UI state**, owned by the Connection screen (design
  "Connection — Startup"), never a blind exit. The screen is also the startup
  state on every launch (prefilled config, explicit Connect) — the app never
  auto-connects blindly. _(Phase 33)_ The session is chosen AFTER connect, not on
  the connect card: the flow is connect → session-pick → cockpit. The entry point
  decides — a recent's still-present remembered session attaches
  directly; a fresh "Connect →", or a recent whose session is gone, shows the
  post-connect picker.
- _(Phase 40)_ **"Session ended" is a mid-session transition, not a disconnect.**
  The active tmux session ending while the connection is alive — killed from the
  cockpit, or its attach otherwise exiting (`TerminalExit`) — is a first-class
  **connected, no active session** state, distinct from a transport loss. The app
  keeps the SSH connection, daemon client, and reverse-path bridges alive and
  re-enters the pre-cockpit picker over the live client: the session picker when
  ≥1 session remains (always shown — no auto-attach, even for exactly one), the
  zero-sessions root picker (the phase-36 create flow) when none remain. The
  re-`Attach` after the mid-session pick re-roots the reactive layer (phase 35),
  exactly like a switch. **Only a real SSH/transport loss routes to the reconnect
  loop and then the Connection screen** — a session end never does
  (`spec-session-lifecycle.md`).
- _(Phase 35)_ **The current-session watch also drives the daemon's watched root.**
  A session's project root is coupled to the tmux session via a session-scoped
  `@root` user option (stamped by the daemon at `new-session`, resolved daemon-side
  on `Attach` via `display-message -p`, falling back to `#{session_path}`). A
  session switch — and reconnect / post-connect pick, which both flow through
  `Attach` — re-roots the reactive layer (file tree, git, LSP) to the new session's
  root, not only the terminal attach. The watched root is no longer the single
  connect-time `--root` pinned for the daemon's lifetime
  (`spec-per-session-project-root.md`).

## Technology map

| Component | Crate / Technology |
|---|---|
| GUI framework | `gpui` (from Zed git, Apache-2.0) |
| Terminal rendering | `termy_terminal_ui` (MIT) — grid painting, link detection, OSC interception, shell integration, tmux control mode client |
| Terminal emulation | `alacritty_terminal` 0.26 |
| VTE parsing | `vte` (via alacritty_terminal) |
| SSH connection | `russh` |
| LSP client | `async-lsp` (MIT OR Apache-2.0) over `lsp-types` 0.95 (MIT) — daemon-side, `gpui`-free, musl-clean |
| Host metrics | `sysinfo` (MIT) — daemon-side `/proc` sampler (CPU / memory / swap / load), `["system"]` feature only, musl-clean |
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
│   ├── logging/            # Shared logging sink (size rotation, filter chain, panic hook) — library used by app + daemon, gpui-free + musl-clean
│   ├── lsp/                # Daemon-side LSP client (async-lsp) — library used by daemon, gpui-free + musl-clean
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
cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl  # daemon release build (Phase 3+)
```

## Cross-compilation and deployment

The daemon is compiled for `x86_64-unknown-linux-musl` (static linking). The target is declared in `rust-toolchain.toml`, so `rustup` installs it automatically; the daemon is a pure-Rust binary that links self-contained via Rust's bundled linker, so no `musl-gcc`/`musl-tools` is required locally. Build it with `just release-daemon`. The CI `daemon-musl` job builds the same artifact on each PR to keep the cross-compile reproducible. The GPUI app currently targets Windows (`x86_64-pc-windows-gnu`, cross-compiled from WSL via MinGW) and Linux (Vulkan/X11); macOS is supported by GPUI but deferred for rift. The primary dev loop builds the app in WSL and runs the resulting `.exe` on the Windows host.
