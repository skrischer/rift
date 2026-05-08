# tmux Control Mode Roadmap

Integration of tmux control mode (`-CC`) into rift. Replaces the current "raw SSH PTY attached to tmux" approach with a structured event stream, enabling multi-pane awareness, pane-scoped state, and tmux-native CWD/layout information without OSC passthrough.

## Reference projects

| Project | Where | What to learn |
|---|---|---|
| **tmuxy** | `github.com/flplima/tmuxy` | Closest match: Rust + Tokio, Parser + StateAggregator + Monitor pattern. Adaptive throttling, flow control, layout debouncing. ~500 LOC parser. |
| **WezTerm termwiz::tmux_cc** | `docs.rs/termwiz/latest/termwiz/tmux_cc/` | Only published Rust parser. `Event` enum is the reference type. `unvis()` for octal decoding. Known bug: UTF-8 split across `%output` boundaries. |
| **iTerm2** | `github.com/gnachman/iTerm2` — `TmuxGateway.m`, `TmuxController.m` | Most mature implementation. Architecture patterns: command queue correlation, resize counter against feedback loops, window affinity/equivalence classes, async pane materialization. |
| **coremux** | `/home/developer/CascadeProjects/coremux/bin/core-tmux-worker` | Our own Bash prototype. Uses subscriptions (`refresh-client -B`) for agent status, branch, path. JSONL event emission. |
| **Ghostty** | `github.com/ghostty-org/ghostty` — `src/terminal/tmux/` | In development (target: Sept 2026). `libghostty-vt` parser. Watch for architectural insights as it matures. |

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

## Implementation phases

### Phase 2a+2b: Control mode integration (COMPLETED)

Completed 2026-05-08. PR: `feat/tmux-control-mode`.

**What was delivered:**
- `tmux -CC new-session -A -s rift` via SSH `channel.exec()` (no interactive shell)
- termy `TmuxClient::from_streams()` with `PtySyncReader`/`PtySyncWriter` bridge
- Event-driven notification processing via flume wakeup channel
- Flow control (`pause-after=5`) activated on connect
- Active pane tracking from `TmuxSnapshot`
- Working directory from snapshot (replaces OSC 7 for tmux-managed CWD)
- Input routing to active pane via `send_input`
- Terminal resize forwarding via `set_client_size`
- Graceful disconnect via termy's `TmuxClient::drop()` (`detach-client`)

**Key decisions:**
- Used termy's `TmuxClient` directly instead of building our own parser in `crates/tmux-core`
- Contributed `from_streams()`, `send_command()`, octal unescape fix, and `#[cfg(unix)]` removal upstream (termy PR #306, merged)
- flume pinned to 0.11 to match termy
- Single `alacritty_terminal::Term` for now — per-pane VTE deferred to Phase 2c

**Known limitations:**
- All `%output` from all panes feeds into one VTE parser — only works correctly with single pane
- CWD from snapshot refresh, not subscriptions (polling on `NeedsRefresh` events)
- No `%pause`/`%continue` handling on our side (termy handles flow control internally)

### Phase 2c: Multi-pane awareness (NEXT)

- Per-pane `alacritty_terminal::Term` instances fed by pane-specific `%output`
- Track all panes in session, render active pane
- Statusbar shows window list, active window/pane indicator
- Pane switching via UI (tab bar or keyboard shortcut)
- Layout-aware pane sizing (parse tmux layout strings)

**Validation:** create split in tmux, both panes render and update independently.

### Phase 2d: Statusbar enrichment

- CWD from tmux subscriptions (`refresh-client -B`) instead of snapshot polling
- Git branch from subscription or metadata sync
- Pane command name (what's running in the pane)
- Session/window name in titlebar or statusbar
- Connection status indicator

### Phase 3: Daemon extraction

- Move tmux management + file watcher into daemon binary on remote host
- Frontend connects to daemon via protocol (see protocol.md)
- Daemon manages tmux control mode connection locally (no SSH latency for parsing)
- VTE parsing stays client-side or moves to daemon (deferred decision)

## Known pitfalls

1. **UTF-8 split across `%output` boundaries.** Treat output as `Vec<u8>`, buffer incomplete sequences. WezTerm PR #6779 documents this.
2. **Resize feedback loops.** Client resizes pane -> tmux sends `%layout-change` -> client resizes again. Use a counter (iTerm2 pattern) or debounce.
3. **Flow control is mandatory.** Without `pause-after`, fast output (builds, `find /`) overwhelms the client. Activate immediately on connect.
4. **tmux version requirements.** Flow control needs 3.2+. Subscriptions need 3.4+. Hard requirement: 3.4+.
5. **Graceful disconnect.** Send `detach-client` before closing. tmux 3.5a crashes on abrupt connection kill (tmuxy finding). Handled by termy's `TmuxClient::drop()`.
6. **PTY requirement.** `tmux -CC` requires a real terminal, not a pipe. Over SSH this is fine (SSH PTY channel).
7. **Parallel session creation.** tmux 3.5a crashes when multiple clients create sessions simultaneously. Serialize with a mutex (tmuxy pattern).
8. **Octal escaping in command responses.** tmux control mode octal-escapes characters < ASCII 32 (e.g. `\x1f` becomes `\037`). Must `unescape_tmux_payload` before parsing field separators. Fixed in termy PR #306.

## Open decisions

- [x] **Parser approach:** Use `termy_terminal_ui` directly via `TmuxClient::from_streams()`.
- [x] **Upstream strategy:** PR #306 merged into termy. No fork needed.
- [x] **Minimum tmux version:** 3.4+ (hard requirement for subscriptions).
- [ ] **VTE parsing location in Phase 3:** Client-side (current, simpler) vs. daemon-side (less data over SSH). Deferred per ARCHITECTURE.md.
- [ ] **Per-pane VTE ownership:** Does `crates/terminal` own per-pane `Term` instances, or does a new module in `crates/app` manage the pane-to-Term mapping?

## Decision log

- 2026-05-07: Research completed. tmuxy identified as primary reference (Rust + Tokio, same stack). WezTerm for parser types, iTerm2 for architecture patterns.
- 2026-05-07: termy_terminal_ui already contains a full tmux control mode implementation. Decided to use it directly.
- 2026-05-07: Opened termy issue #305 and PR #306 for `from_streams()` constructor.
- 2026-05-08: termy PR #306 merged upstream (4 commits: `from_streams`, `send_command`, `detach-client` fix, `#[cfg(unix)]` removal + octal unescape).
- 2026-05-08: Phase 2a+2b completed. tmux control mode working over SSH with event-driven notification processing.
- 2026-05-08: Decided minimum tmux version 3.4+ (hard requirement).
