# Spec: Phase 2a+2b -- tmux Control Mode Integration
> Status: COMPLETED
> Created: 2026-05-07
> Completed: 2026-05-08

Integrated tmux control mode (`-CC`) over SSH, replacing raw PTY-attached-to-tmux with a structured event stream. Uses termy's `TmuxClient` directly via contributed `from_streams()` API.

## Outcome (delivered)
- [x] `tmux -CC new-session -A -s rift` via SSH `channel.exec()` (no interactive shell)
- [x] termy `TmuxClient::from_streams()` with `PtySyncReader`/`PtySyncWriter` bridge
- [x] Event-driven notification processing via flume wakeup channel
- [x] Flow control (`pause-after=5`) activated on connect
- [x] Active pane tracking from `TmuxSnapshot`
- [x] Working directory from snapshot (replaces OSC 7 for tmux-managed CWD)
- [x] Input routing to active pane via `send_input`
- [x] Terminal resize forwarding via `set_client_size`
- [x] Graceful disconnect via termy's `TmuxClient::drop()` (`detach-client`)
- [x] Upstream contributions merged: termy PR #306 (4 commits: `from_streams`, `send_command`, `detach-client` fix, `#[cfg(unix)]` removal + octal unescape)

## Key decisions
| Decision | Rationale | Date |
|---|---|---|
| Use termy's `TmuxClient` directly instead of building own parser in `crates/tmux-core` | termy already has a full control mode implementation; avoids duplicating ~500 LOC parser + state aggregator | 2026-05-07 |
| Contribute `from_streams()` upstream (PR #306) instead of forking | Cleaner dependency, no fork maintenance burden | 2026-05-07 |
| Pin flume to 0.11 | Must match termy's flume version | 2026-05-08 |
| Single `alacritty_terminal::Term` for Phase 2a+2b | Per-pane VTE deferred to Phase 2c; keeps scope manageable | 2026-05-08 |
| Minimum tmux version 3.4+ (hard requirement) | Subscriptions need 3.4+; flow control needs 3.2+ | 2026-05-08 |
| CWD from snapshot refresh, not subscriptions | Simpler; subscriptions deferred to Phase 2d | 2026-05-08 |

## Known limitations
- All `%output` from all panes feeds into one VTE parser -- only works correctly with single pane (fixed in Phase 2c)
- CWD from snapshot refresh, not subscriptions (polling on `NeedsRefresh` events)
- No `%pause`/`%continue` handling on our side (termy handles flow control internally)
- UTF-8 split across `%output` boundaries requires treating output as `Vec<u8>` and buffering incomplete sequences

## Decision log
- 2026-05-07: Research completed. tmuxy = primary reference (Rust+Tokio), WezTerm for parser types, iTerm2 for architecture patterns.
- 2026-05-07: Discovered termy_terminal_ui already contains full tmux control mode client. Decided to use directly.
- 2026-05-07: Opened termy issue #305 and PR #306 for `from_streams()` constructor.
- 2026-05-08: termy PR #306 merged upstream (4 commits).
- 2026-05-08: Phase 2a+2b completed. Control mode working over SSH with event-driven notification processing.
- 2026-05-08: Decided minimum tmux 3.4+ as hard requirement.
