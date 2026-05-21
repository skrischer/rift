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

## Task breakdown

### Step 1: Subscription infrastructure

**Goal:** Register tmux subscriptions on connect and process `%subscription-changed` notifications.

**Changes:**
- Add subscription registration after flow control activation in the connect sequence
- Register subscriptions for: `pane_current_path`, `pane_current_command`, `window_name`
- Route `%subscription-changed` notifications to SessionView via a new channel or existing snapshot channel
- Parse subscription payload: `<name> $<session> @<window> %<pane> <value>`

**Validation:** Subscription values arrive in SessionView when CWD or command changes in a pane. Log output confirms values update within ~1 second of change.

### Step 2: Tab bar rendering

**Goal:** SessionView renders a tab bar above the pane layout showing all tmux windows.

**Changes:**
- `crates/terminal/src/session_view.rs` — add tab bar to render output, above the flex layout
- Tab bar shows `window.index: window.name` for each window from TmuxSnapshot
- Active window tab is visually highlighted
- Tab bar height subtracted from available terminal grid space

**Validation:** Create multiple tmux windows (`tmux new-window`). Tab bar shows them. Active window is highlighted. Terminal grid size adjusts correctly.

### Step 3: Window switching

**Goal:** Click a tab or press Ctrl+Shift+N to switch tmux windows.

**Changes:**
- Click handler on tab sends `select-window -t @<window_id>` via TmuxClient
- Keyboard handler intercepts Ctrl+Shift+1..9 before PTY input routing
- tmux notifications -> NeedsRefresh -> snapshot refresh -> pane layout rebuilds for new window
- Panes of previous window stop rendering but keep VTE state

**Validation:** Click tab -> window switches, panes rebuild. Ctrl+Shift+2 -> switches to window 2. Switch back -> pane content preserved.

### Step 4: CWD from subscriptions

**Goal:** Replace snapshot-polling CWD with subscription-driven updates.

**Changes:**
- SessionView updates per-pane CWD from `%subscription-changed` for `pane_current_path`
- Statusbar CWD display updates reactively
- Remove or reduce snapshot-based CWD refresh (keep snapshot as fallback for initial state)

**Validation:** `cd /tmp` in a pane -> statusbar CWD updates within ~1 second. No snapshot refresh needed for CWD changes.

### Step 5: Statusbar enrichment

**Goal:** Statusbar shows git branch, pane command, session/window name, connection status.

**Changes:**
- Git branch: from subscription `pane_current_path` -> run `git rev-parse --abbrev-ref HEAD` via tmux `send-keys` or a separate mechanism. Alternative: use a subscription format like `#(cd #{pane_current_path} && git rev-parse --abbrev-ref HEAD 2>/dev/null)`
- Pane command: from `pane_current_command` subscription
- Session/window name: from snapshot (already available)
- Connection status: track SSH connection state, show indicator

**Validation:** Statusbar shows all four pieces of information. Git branch updates when switching to a different repo directory. Command name reflects the actual running process (e.g. `bash`, `python`, `cargo`).

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
