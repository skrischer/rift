# tmux Control Mode Roadmap

Integration of tmux control mode (`-CC`) into rift. Replaces the current "raw SSH PTY attached to tmux" approach with a structured event stream, enabling multi-pane awareness, pane-scoped state, and tmux-native CWD/layout information without OSC passthrough.

## Reference projects

Study these before implementing. Priority order:

| Project | Where | What to learn |
|---|---|---|
| **tmuxy** | `github.com/flplima/tmuxy` — clone to CascadeProjects | Closest match: Rust + Tokio, Parser + StateAggregator + Monitor pattern. Adaptive throttling, flow control, layout debouncing. ~500 LOC parser. |
| **WezTerm termwiz::tmux_cc** | `docs.rs/termwiz/latest/termwiz/tmux_cc/` | Only published Rust parser. `Event` enum is the reference type. `unvis()` for octal decoding. Known bug: UTF-8 split across `%output` boundaries. |
| **iTerm2** | `github.com/gnachman/iTerm2` — `TmuxGateway.m`, `TmuxController.m` | Most mature implementation. Architecture patterns: command queue correlation, resize counter against feedback loops, window affinity/equivalence classes, async pane materialization. |
| **coremux** | `/home/developer/CascadeProjects/coremux/bin/core-tmux-worker` | Our own Bash prototype. Uses subscriptions (`refresh-client -B`) for agent status, branch, path. JSONL event emission. |
| **Ghostty** | `github.com/ghostty-org/ghostty` — `src/terminal/tmux/` | In development (target: Sept 2026). `libghostty-vt` parser. Watch for architectural insights as it matures. |

**Not useful as control mode reference:** Arbor (no tmux), Claude Squad (CLI polling only), Gas Town (CLI only).

## Protocol overview

Connection: `tmux -CC attach -t <session>` (or `new-session`). Double-C = no terminal echo. tmux sends DCS `\033P1000p` on entry.

All server messages are line-based, prefixed with `%`. Output encoding: characters < ASCII 32 and `\` are octal-escaped (OpenBSD vis format).

### Notifications

| Message | Payload | When |
|---|---|---|
| `%output` | `%<pane_id> <escaped_text>` | Pane produces terminal output |
| `%extended-output` | `%<pane_id> <ms> : <text>` | Extended output with age |
| `%window-add` | `@<window_id>` | Window created |
| `%window-close` | `@<window_id>` | Window destroyed |
| `%window-renamed` | `@<window_id> <name>` | Window title changed |
| `%window-pane-changed` | `@<window_id> %<pane_id>` | Active pane switched |
| `%session-changed` | `$<session_id> <name>` | Attached session changed |
| `%session-renamed` | `<name>` | Session renamed |
| `%sessions-changed` | (none) | Session list changed |
| `%session-window-changed` | `$<session_id> @<window_id>` | Active window in session changed |
| `%layout-change` | `@<window_id> <layout> [visible_layout] [flags]` | Pane layout changed |
| `%pane-mode-changed` | `%<pane_id>` | Pane entered/exited copy mode |
| `%pause` | `%<pane_id>` | Flow control: pane output paused |
| `%continue` | `%<pane_id>` | Flow control: pane output resumed |
| `%client-detached` | `<client>` | Another client detached |
| `%client-session-changed` | `<client> $<session_id> <name>` | Another client changed session |
| `%subscription-changed` | `<name> $<session> @<window> %<pane> <value>` | Watched format changed |
| `%exit` | `[reason]` | Server disconnect |

### Command/response

```
<command>\n
%begin <epoch> <cmd_number> <flags>
[output lines]
%end <epoch> <cmd_number> <flags>       // or %error
```

Commands are standard tmux commands. Responses are correlated by `cmd_number`.

### Flow control (tmux 3.2+)

Activate: `refresh-client -f pause-after=N` (N = seconds of buffered output before pause).
Server sends `%pause %<pane_id>` when buffer exceeds threshold.
Client resumes: `refresh-client -A '%<pane_id>:continue'`.
Hard limit: tmux disconnects after 300s without drain.

### Subscriptions (tmux 3.4+)

Register: `refresh-client -B '<name>:<scope>:<format>'`.
Server sends `%subscription-changed` when format value changes (max 1x/second).
Useful for: `pane_current_path`, `pane_current_command`, `window_name`, custom formats.

## Architecture for crates/tmux-core

```
┌──────────────────────────────────────────────────┐
│  crates/tmux-core                                │
│                                                  │
│  connection.rs  -- PTY spawn, connect, reconnect │
│  parser.rs      -- line-by-line, octal decode    │
│  state.rs       -- session/window/pane tree      │
│  monitor.rs     -- event loop, throttling, sync  │
│  types.rs       -- TmuxEvent enum, IDs, layout   │
│  lib.rs         -- public API                    │
└──────────────────────────────────────────────────┘
         │
         │  tokio::mpsc channels
         ▼
  crates/app or crates/daemon
```

### Parser (parser.rs)

Line-by-line stateful processor. Two modes:
- **Notification mode:** immediate dispatch on `%` prefix lines
- **Response mode:** accumulate lines between `%begin` and `%end`/`%error`

Octal decoding via `unvis()` function (port from WezTerm or tmuxy). Output as `Vec<u8>` not `String` to handle UTF-8 splits.

### State (state.rs)

```
TmuxState
├── sessions: HashMap<SessionId, SessionState>
│   └── windows: Vec<WindowId>
├── windows: HashMap<WindowId, WindowState>
│   ├── name: String
│   ├── layout: String
│   └── panes: Vec<PaneId>
└── panes: HashMap<PaneId, PaneState>
    ├── current_path: Option<String>
    ├── current_command: Option<String>
    ├── vte: alacritty_terminal::Term  (per-pane VTE parser)
    └── paused: bool
```

Each pane gets its own `alacritty_terminal::Term`. `%output` feeds into the pane's VTE parser. The frontend reads cell grids from the per-pane Term — same rendering pipeline as today, just per-pane.

### Monitor (monitor.rs)

`tokio::select!` event loop. Patterns from tmuxy:
- **Adaptive throttling:** track event frequency per 100ms window. >20 events -> batch at 32ms intervals
- **Layout debouncing:** coalesce `%layout-change` within 16ms window
- **Flow control:** on `%pause` immediately send `refresh-client -A`
- **Metadata sync:** `list-panes` 500ms after output settles (gets CWD, command, dimensions)
- **Idle heartbeat:** 15s without events -> full consistency check via `list-panes`

### Connection (connection.rs)

tmux needs a real PTY for `-CC` (not a pipe). Use `pty_process` crate or our existing SSH PTY.

Key concern: our connection is via SSH. Two options:
1. **SSH channel runs `tmux -CC`** directly (replaces current `tmux new-session -A -s rift`)
2. **Daemon on remote host** runs `tmux -CC` locally, exposes state via protocol

Option 1 is simpler and works for Phase 2 (no daemon). Option 2 is Phase 3.

Graceful close: `detach-client` command, not SIGKILL (tmuxy found tmux 3.5a crashes on kill).

## Implementation phases

### Phase 2a: Parser + types (crates/tmux-core)

- `TmuxEvent` enum covering all notification types
- Line-by-line parser with octal decode
- Command/response correlation (command queue with oneshot callbacks)
- Unit tests for every message type, including malformed input
- No connection logic yet — parser takes `&[u8]` input

**Validation:** parse recorded tmux -CC sessions, compare output.

### Phase 2b: Connection + state

- Switch SSH command from `tmux new-session -A -s rift` to `tmux -CC new-session -A -s rift`
- Wire parser to SSH PTY stream (replaces current raw byte forwarding)
- Build TmuxState from initial `%window-add` + `%layout-change` burst
- Per-pane `alacritty_terminal::Term` instances fed by `%output`
- Flow control (`pause-after`, `%pause`/`%continue` handling)
- Subscriptions for `pane_current_path` (replaces OSC 7 dependency)

**Validation:** single pane renders identically to current Phase 1.5 output.

### Phase 2c: Multi-pane awareness

- Track all panes in session, render active pane
- Statusbar shows window list, active window/pane indicator
- Pane switching via UI (tab bar or keyboard shortcut)
- Layout-aware pane sizing (parse tmux layout strings)
- Monitor with throttling, debouncing, metadata sync

**Validation:** create split in tmux, both panes render and update independently.

### Phase 2d: Statusbar enrichment

- CWD from tmux subscriptions (no more OSC passthrough needed)
- Git branch from subscription or metadata sync
- Pane command name (what's running in the pane)
- Session/window name in titlebar or statusbar
- Connection status indicator

### Phase 3: Daemon extraction

- Move tmux-core + file watcher into daemon binary on remote host
- Frontend connects to daemon via protocol (see protocol.md)
- Daemon manages tmux control mode connection locally (no SSH latency for parsing)
- VTE parsing stays client-side or moves to daemon (deferred decision, see ARCHITECTURE.md line 76)

## Known pitfalls

1. **UTF-8 split across `%output` boundaries.** Treat output as `Vec<u8>`, buffer incomplete sequences. WezTerm PR #6779 documents this.
2. **Resize feedback loops.** Client resizes pane -> tmux sends `%layout-change` -> client resizes again. Use a counter (iTerm2 pattern) or debounce.
3. **Flow control is mandatory.** Without `pause-after`, fast output (builds, `find /`) overwhelms the client. Activate immediately on connect.
4. **tmux version requirements.** Flow control needs 3.2+. Subscriptions need 3.4+. Test minimum version and degrade gracefully.
5. **Graceful disconnect.** Send `detach-client` before closing. tmux 3.5a crashes on abrupt connection kill (tmuxy finding).
6. **PTY requirement.** `tmux -CC` requires a real terminal, not a pipe. Over SSH this is fine (SSH PTY channel), but local testing needs `pty_process` or similar.
7. **Parallel session creation.** tmux 3.5a crashes when multiple clients create sessions simultaneously. Serialize with a mutex (tmuxy pattern).

## Open decisions

- [x] **Parser approach:** Use `termy_terminal_ui` (already a dependency). See decision 2026-05-07 below.
- [ ] **VTE parsing location in Phase 3:** Client-side (current, simpler) vs. daemon-side (less data over SSH). Deferred per ARCHITECTURE.md.
- [ ] **Minimum tmux version:** 3.2 (flow control) or 3.4 (subscriptions). Subscriptions replace polling and are very useful. Recommendation: require 3.4+.
- [x] ~~**tmuxy as dependency vs. inspiration**~~ Superseded by termy decision.

## Decision log

- 2026-05-07: Research completed. tmuxy identified as primary reference (Rust + Tokio, same stack). WezTerm for parser types, iTerm2 for architecture patterns.
- 2026-05-07: **termy_terminal_ui already contains a full tmux control mode implementation.** `TmuxClient`, `ControlStateMachine`, `TmuxSnapshot`, `TmuxNotification` — all public or usable through the client API. Same pattern as Phase 1.5: termy built it, MIT-licensed, we already depend on it.
- 2026-05-07: **Decided: parallel track for SSH integration.**
  - **Problem:** `TmuxClient::new()` spawns tmux locally via `std::process::Child`. We need it over SSH.
  - **Ideal fix:** `TmuxClient::from_streams(stdin, stdout)` constructor that accepts pre-existing I/O streams instead of spawning a child process. Small change, benefits any embedded/remote use case.
  - **Strategy:**
    1. Open issue/PR on termy proposing `from_streams()` constructor.
    2. In parallel, build a thin adapter in `crates/tmux-core` that reimplements the channel wiring (~200 LOC) using termy's public `TmuxNotification`/`TmuxSnapshot` types and our SSH PTY stream. The `ControlStateMachine` logic is `pub(crate)` so we reimplement the line-by-line parser (well-documented, well-tested pattern).
    3. **If termy merges the PR:** replace our adapter with `TmuxClient::from_streams()`, delete reimplemented parser.
    4. **If termy declines:** our working adapter becomes the permanent solution. Offer our implementation as evidence in the PR.
  - **No blocker either way.** Both paths produce a working control mode client. The termy path is less code to maintain; the parallel path is zero external dependency on timeline.
  - **Upstream issue:** https://github.com/termy-org/termy/issues/305 — opened 2026-05-07. Note: termy is a local desktop terminal emulator with no SSH/remote scope. The feature request is valid but not a problem they have themselves. Low priority expected from their side. Our parallel adapter approach is the primary path.
