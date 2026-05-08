# rift — Phase 1: MVP Implementation

## Goal

Implement the minimum viable loop: launch the Tauri app, connect to a remote host via SSH, attach to a tmux session, render terminal output in the GUI, and forward keyboard input back. When done, I can type in the rift window and interact with a remote tmux session as if using a terminal emulator.

## Context

Read these files before writing any code:
- `AGENTS.md` — coding principles, repo layout, architectural rules
- `ARCHITECTURE.md` — full system architecture (P1 only implements a subset)
- `VISION.md` — project vision and core principles
- `.claude/docs/patterns.md` — error handling, state management, async discipline

Key constraint: the daemon is NOT part of P1. The Tauri app connects directly to the remote host via SSH, starts tmux, and forwards raw PTY bytes to xterm.js. No daemon binary, no WebSocket protocol, no cell-grid serialization. That comes later.

## Current state (Phase 0 complete)

The workspace is scaffolded and compiling:

```
rift/
├── Cargo.toml                # workspace (7 members)
├── crates/
│   ├── daemon/               # empty shell — not used in P1
│   ├── tmux-core/            # error types only — not used in P1
│   ├── terminal/             # error types only — not used in P1
│   ├── explorer/             # error types only — not used in P1
│   ├── protocol/             # message types (ClientMessage, DaemonMessage)
│   └── plugin-api/           # PanePlugin trait — not used in P1
├── app/
│   ├── src-tauri/            # Tauri v2 shell (compiles, shows empty window)
│   ├── index.html
│   └── package.json
├── .github/workflows/ci.yml  # fmt + clippy + test
├── justfile                  # just ci = full check
├── deny.toml                 # license policy
└── rust-toolchain.toml       # stable + clippy + rustfmt + musl
```

`cargo build --workspace --exclude rift-app` and `just ci` pass cleanly.

## Scope — what to build

### 1. New crate: `crates/ssh/` (rift-ssh)

Add to workspace. Use the `russh` crate. Implement:

- Connect to a host using key-based auth (read `~/.ssh/id_ed25519` or `~/.ssh/id_rsa`)
- Support reading `~/.ssh/config` for host aliases via `ssh2-config` or manual parsing
- Open a PTY channel (`channel_open_session` → `request_pty` → `request_shell`)
- Execute `tmux new-session -A -s rift` on the remote shell to attach or create a session
- Forward PTY output bytes to the caller
- Forward caller input to the PTY channel

Expose a clean async API:

```rust
pub struct SshConnection { /* ... */ }

impl SshConnection {
    pub async fn connect(host: &str, user: &str, key_path: &Path) -> Result<Self>;
    pub async fn open_pty(&mut self, cols: u16, rows: u16) -> Result<PtyStream>;
}

pub struct PtyStream { /* ... */ }

impl PtyStream {
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    pub async fn write(&mut self, data: &[u8]) -> Result<()>;
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()>;
}
```

The exact API will evolve — this is a starting point, not a spec to implement blindly.

### 2. Tauri backend (app/src-tauri)

Bridge rift-ssh and the frontend:

- On app start, read a hardcoded connection config (host, user, key path). No connection UI yet.
- Establish SSH connection via rift-ssh
- Open PTY, run `tmux new-session -A -s rift`
- Spawn two async tasks:
  - **Reader**: reads PTY output, sends to frontend via Tauri event (`pty-output`, payload: base64-encoded bytes)
  - **Writer**: listens for frontend events (`pty-input`), writes to PTY
- Handle terminal resize events from frontend (`pty-resize`)

### 3. Frontend terminal (TypeScript)

Use `@xterm/xterm` + `@xterm/addon-webgl` + `@xterm/addon-fit`:

- Initialize xterm.js terminal filling the entire window
- Listen for Tauri events (`pty-output`), decode base64, write to xterm.js
- Capture keyboard input from xterm.js `onData`, send to Tauri backend (`pty-input`)
- Handle window resize: use addon-fit to calculate cols/rows, send `pty-resize` event
- WebGL renderer for performance

Reference: `github.com/marc2332/tauri-terminal` — working Tauri + xterm.js example. Ours is SSH-based instead of local PTY.

## Scope — what NOT to build

- No connection UI (hardcoded config is fine)
- No file explorer, file sync, LSP
- No context menus, plugin system, pane awareness
- No session bar / tab bar
- No daemon binary (direct SSH from Tauri app)
- No tmux control mode parsing (just raw PTY)
- No multi-pane support (single fullscreen terminal)

## Tech decisions

| Dependency | Purpose |
|---|---|
| `russh` | SSH client |
| `russh-keys` | SSH key loading |
| `tokio` (workspace) | Async runtime |
| `tauri` v2 | GUI framework |
| `@xterm/xterm` | Terminal emulation in frontend |
| `@xterm/addon-webgl` | GPU-accelerated rendering |
| `@xterm/addon-fit` | Auto-resize to container |

## Definition of done

1. `cd app && cargo tauri dev` — app window opens
2. App connects to a preconfigured remote host via SSH
3. tmux session starts (or reattaches)
4. Remote shell prompt rendered in xterm.js
5. Interactive shell works (type commands, see output)
6. Window resize propagates to remote terminal
7. `just ci` passes
8. CI workflow green

## How to start

1. Install frontend deps (`npm install` in `app/`) — xterm.js, vite, typescript
2. Get xterm.js rendering a static terminal in the window (no SSH yet)
3. Create `crates/ssh/`, get russh connecting and executing a command (no frontend yet)
4. Wire together: SSH PTY ↔ Tauri events ↔ xterm.js
5. Add tmux attachment
6. Add resize handling
