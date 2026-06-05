# Spec: Phase 2d — Tab bar + statusbar enrichment

> Status: IN PROGRESS
> Created: 2026-05-21
> Completed: —

Tab bar for tmux window switching and enriched statusbar with live metadata from tmux subscriptions. Completes the tmux integration before Phase 3 (daemon).

## Outcome

- [x] Tab bar renders one tab per tmux window, showing `index: name`, with the active window highlighted
- [x] Clicking a tab switches to that window (pane layout rebuilds from snapshot)
- [ ] Keyboard shortcut Ctrl+Shift+1..9 switches windows 1..9
- [ ] CWD updates via tmux subscriptions (`refresh-client -B`) instead of snapshot polling
- [ ] Git branch displayed in statusbar (from subscription or metadata sync)
- [ ] Pane command name displayed (what's running in the active pane)
- [ ] Session/window name in titlebar or statusbar
- [ ] Connection status indicator (connected/reconnecting/disconnected)

## Scope

### In scope

- Tab bar UI component in SessionView
- Window switching via tmux `select-window` command
- Subscription-based CWD tracking (replaces snapshot polling for CWD)
- Statusbar enrichment with git branch, pane command, session name
- Connection status indicator

### Out of scope

- Pane zoom handling (`resize-pane -Z`) — deferred
- Drag-reorder tabs — tmux owns window order
- Session switching — single session for now
- Tab close/create from GUI — use tmux commands in terminal
- Adaptive throttling (tmuxy pattern) — add only if performance requires it
- Mirroring the user's tmux status-line config (`status-left/right/style`) — the native statusbar is the source of truth and deliberately does **not** render the `.tmux.conf` status line. Honoring that config is its own opt-in mode: [spec-tmux-statusline-mirroring.md](spec-tmux-statusline-mirroring.md). The fields here are rift-native, fed from tmux *data* (subscriptions), not from tmux's status format strings.

## Constraints

- Subscriptions require tmux 3.4+ (already a hard requirement from Phase 2a)
- termy's `TmuxClient` must support subscription registration — verify API or extend
- Tab bar must not interfere with terminal grid sizing (subtract tab bar height from available space)
- Inactive window panes stay in memory (VTE state current) but are not rendered

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Use termy's TmuxClient directly | Upstream maintained, PR #306 merged, avoids fork | 2026-05-07 |
| Minimum tmux 3.4+ | Hard requirement for subscriptions | 2026-05-08 |
| Per-pane VTE in `crates/terminal` | PaneView each has own `Arc<Mutex<Term>>`, managed by SessionView | 2026-05-20 |
| Snapshot-driven pane lifecycle | Create/remove PaneViews from TmuxSnapshot diffs | 2026-05-20 |

## Tracking

The step decomposition lives as GitHub issues under the milestone — not in this file. This spec owns the design; the issues own progress.

- Milestone: [Phase 2d — Tab bar + statusbar enrichment](https://github.com/skrischer/rift/milestone/1)
- Open steps (issues): subscription infrastructure (#15), keyboard window switching (#16), CWD from subscriptions (#17), git branch (#18), pane command name (#19), session/window name (#20), connection status (#21)
- Done before issue tracking existed: tab bar rendering, click-to-switch (commits 33fea26, 8ca4b0b)

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Tab bar visible with correct window names
- [ ] Window switching works via click and keyboard shortcut
- [ ] CWD updates within ~1 second of `cd` command (subscription-driven)
- [ ] Git branch displays correctly in statusbar
- [ ] Pane command name displays correctly
- [ ] Connection status indicator shows connected state
- [ ] Multi-window scenario: create 3 windows, switch between them, all pane content preserved
- [ ] Resize window: tab bar + terminal grid adjust correctly

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| termy's TmuxClient may not expose subscription API | Check termy source. If not exposed, contribute upstream or use `send_command()` to register subscriptions directly. |
| Git branch via tmux subscription format may be slow | Limit subscription update rate (tmux already caps at 1/sec). Alternatively, get git branch from daemon in Phase 3 and use a simple fallback here. |
| Tab bar height calculation affects terminal grid sizing | Use fixed tab bar height (e.g. 28px). Subtract from available space before grid calculation. Test with resize. |
| Subscription values may arrive before pane exists | Reuse early output buffer pattern from Phase 2c — buffer subscription values for unknown panes. |

## Decision log

Decisions made during implementation:

- 2026-05-21: Spec created from Phase 2d section of control-mode-roadmap.md. tmux protocol reference extracted to tmux-reference.md.
- 2026-05-21: Steps 2 (tab bar rendering) and 3 (click-to-switch) already implemented in prior commits (33fea26, 8ca4b0b). Status updated to IN PROGRESS.
