# tmux control mode reference

Reference document for tmux control mode (`-CC`) protocol, notifications, flow control, and known pitfalls. Extracted from the Phase 2 research and implementation work.

## Connection

`tmux -CC attach -t <session>` (or `new-session`). Double-C = no terminal echo. tmux sends DCS `\033P1000p` on entry.

All server messages are line-based, prefixed with `%`. Output encoding: characters < ASCII 32 and `\` are octal-escaped (OpenBSD vis format).

## Notifications

| Message | Payload | When |
|---|---|---|
| `%output` | `%<pane_id> <escaped_text>` | Pane produces terminal output |
| `%extended-output` | `%<pane_id> <ms> : <text>` | Extended output with age |
| `%window-add` | `@<window_id>` | Window created |
| `%window-close` | `@<window_id>` | Window destroyed |
| `%window-renamed` | `@<window_id> <name>` | Window title changed |
| `%window-pane-changed` | `@<window_id> %<pane_id>` | Active pane switched |
| `%session-changed` | `$<session_id> <name>` | Attached session changed |
| `%session-renamed` | `$<session_id> <name>` | Session renamed (the man page omits the id field; tmux 3.4 sends it). Broadcast to every control client on the server regardless of its attached session — match the id before adopting the name |
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

## Command/response

```
<command>\n
%begin <epoch> <cmd_number> <flags>
[output lines]
%end <epoch> <cmd_number> <flags>       // or %error
```

Commands are standard tmux commands. Responses are correlated by `cmd_number`.

## Flow control (tmux 3.2+)

Activate: `refresh-client -f pause-after=N` (N = seconds of buffered output before pause).
Server sends `%pause %<pane_id>` when buffer exceeds threshold.
Client resumes: `refresh-client -A '%<pane_id>:continue'`.
Hard limit: tmux disconnects after 300s without drain.

## Subscriptions (tmux 3.4+)

Register: `refresh-client -B '<name>:<scope>:<format>'`.
Server sends `%subscription-changed` when format value changes (max 1x/second).
Useful for: `pane_current_path`, `pane_current_command`, `window_name`, custom formats.

## Known pitfalls

1. **UTF-8 split across `%output` boundaries.** Treat output as `Vec<u8>`, buffer incomplete sequences. WezTerm PR #6779 documents this.
2. **Resize feedback loops.** Client resizes pane -> tmux sends `%layout-change` -> client resizes again. Use a counter (iTerm2 pattern) or debounce.
3. **Flow control is mandatory.** Without `pause-after`, fast output (builds, `find /`) overwhelms the client. Activate immediately on connect.
4. **tmux version requirements.** Flow control needs 3.2+. Subscriptions need 3.4+. Hard requirement: 3.4+.
5. **Graceful disconnect.** Send `detach-client` before closing. tmux 3.5a crashes on abrupt connection kill (tmuxy finding). Handled by termy's `TmuxClient::drop()`.
6. **PTY requirement — `-CC` only.** `tmux -CC` requires a real terminal: it calls `tcgetattr` on the client tty, which fails on a pipe (`Inappropriate ioctl for device`). Over SSH this is fine (SSH PTY channel). Single `-C` works over plain pipes — verified empirically (spike #201): a `tmux -C` child with piped stdio speaks the full control-mode protocol, which is what lets the daemon own tmux as a child process without allocating a PTY.
7. **Parallel session creation.** tmux 3.5a crashes when multiple clients create sessions simultaneously. Serialize with a mutex (tmuxy pattern).
8. **Octal escaping in command responses.** tmux control mode octal-escapes characters < ASCII 32 (e.g. `\x1f` becomes `\037`). Must `unescape_tmux_payload` before parsing field separators. Fixed in termy PR #306.
9. **`#{...}` arguments must be quoted on command lines.** The control-mode command parser treats an unquoted `#` as a comment start and `{` as a brace block — `display-message -p RIFT:#{pane_id}` is a `parse error`, `display-message -p 'RIFT:#{pane_id}'` works (spike #201 finding; affects all command emission).

## Reference implementations

| Project | Where | Key takeaway |
|---|---|---|
| **termy** | termy_terminal_ui crate | Our primary dependency. `TmuxClient::from_streams()` (PR #306, merged). Full control mode client with parser, sessions, flow control. |
| **tmuxy** | `github.com/flplima/tmuxy` | Rust + Tokio. Parser + StateAggregator + Monitor pattern. Adaptive throttling. ~500 LOC parser. |
| **WezTerm termwiz::tmux_cc** | `docs.rs/termwiz/latest/termwiz/tmux_cc/` | Only published Rust parser. `Event` enum is the reference type. `unvis()` for octal decoding. |
| **iTerm2** | `TmuxGateway.m`, `TmuxController.m` | Most mature implementation. Command queue correlation, resize counter, window affinity, async pane materialization. |
| **Ghostty** | `src/terminal/tmux/` | In development (target: Sept 2026). Watch for architectural insights. |
