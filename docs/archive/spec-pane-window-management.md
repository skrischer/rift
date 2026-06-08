# Spec: Pane and window lifecycle management

> Status: COMPLETED
> Created: 2026-06-07
> Completed: 2026-06-08

Make the tmux pane and window lifecycle directly manipulable from the GPU UI — create, close, focus, rename, split, all with the mouse — and stop `exit` from tearing down the whole application.

## Design framing

rift already renders tmux's windows as tabs and panes as a layout tree, but the only lifecycle operation wired to the UI is *switching* (`select-window` on tab click, `select-pane` on focus) plus *resizing* (#41). Everything else — create a window, close a window, close a pane, split a pane, rename a window — still requires typing into the terminal. That is the same "failing at being a GUI" gap the interaction-fixes spec named: a native frontend should expose these as direct-manipulation affordances.

It also exposes a latent bug: closing a pane by typing `exit` quits the entire app, because the only app-shutdown path is a per-pane `cx.quit()`. Fixing that is a prerequisite for any close affordance, so it leads this spec.

All mutations follow the established discipline (drag-to-resize, #41): the UI emits a tmux command through the single command seam, and the next snapshot / `%layout-change` redraws the result. The snapshot stays the source of truth; the UI never optimistically mutates the pane/window set locally.

## Outcome

What is true when this work is done:

- [x] Closing a pane (e.g. typing `exit`) closes only that pane/window; rift quits only when the tmux session itself ends (last pane gone / control mode exits)
- [x] The window tab bar creates a window via a `+` control (`new-window`) and closes a window via a per-tab `x` control and middle-click (`kill-window`)
- [x] A per-window pane sidebar lists the active window's panes; clicking a row focuses it (`select-pane`), a per-row `x` closes it (`kill-pane`), and header controls split the active pane side-by-side (`|`) or stacked (`-`); focus follows the newly created pane
- [x] Double-clicking a window tab renames the window (`rename-window`); the tab label reflects the new name

## Scope

### In scope

- Move app shutdown off the per-pane PTY loop and onto genuine session end (`ConnectionStatus::Disconnected`)
- Window tab bar: a `+` suffix emitting `new-window`; a per-tab `x` suffix and middle-click emitting `kill-window -t <id>`
- Pane sidebar: a left, fixed-width vertical list scoped to the active window. One row per pane, label from `pane_current_command` (fallback: pane id), active pane highlighted. Row click → `select-pane -t <id>`; per-row `x` → `kill-pane -t <id>`; header controls `|` → `split-window -h -t <active>`, `-` → `split-window -v -t <active>`
- Window rename: double-click a tab → inline text input → `rename-window -t <id> <name>`
- All tmux mutations go through the existing `tmux_command_tx`; the next snapshot is the source of truth for the resulting layout, window set, and pane set

### Out of scope

- **Pane zoom (`resize-pane -Z`)** — previously Outcome 4 of `spec-terminal-interaction-fixes.md` (#42). Dropped: no value for the current workflow; the screen real estate goes to direct pane manipulation instead. That spec is archived COMPLETED on its remaining three outcomes (see Prior decisions).
- **Keyboard shortcuts for these actions** — rift-native bindings belong with `spec-tmux-keytable-mirroring.md`; this spec is mouse-driven only and must not pre-empt the prefix/key-table model.
- **Close confirmation / running-process guard** — closing a pane that runs an agent kills it without prompt. A harden concern with its own modal surface; a later spec owns it.
- **Tab/window reorder, pane move/swap** (`swap-pane`, `move-pane`), and any drag-and-drop rearrangement.
- **Multi-session switching** and **disconnected/reconnect UI** — the statusbar already shows connection status; the app quits on session end.
- **Agent-state / dev-server display in the sidebar** — pane awareness is plugin territory (`plugin-api`, Phase 3+). The sidebar shows only tmux-derived data (name/command) now and gains state columns from plugins later, never from agent-specific core code.

## Constraints

- Transport is tmux control mode (`-CC`). All lifecycle commands emit through the single narrow interface (`tmux_command_tx` → `send_command_async`). Do not reach into `alacritty_terminal::Term` internals for lifecycle.
- The snapshot (`layout::build_layout` from `%layout-change` / refresh) is the source of truth for layout, window set, and pane set. UI actions emit a command and let the snapshot redraw — no optimistic local mutation.
- App shutdown currently has exactly one path: `cx.quit()` at `crates/terminal/src/pane_view.rs:210`, fired when any pane's PTY read loop ends. It must be **replaced**, not merely removed, or the app never closes on session end.
- `ConnectionStatus::Disconnected` is sent only after `run_ssh_session` returns (control-mode `Exit` or SSH drop) — it is the correct, and only, signal for "the session is gone, quit".
- tmux split direction naming is inverted vs. the visual divider: side-by-side (vertical divider, `|`) = `split-window -h`; stacked (horizontal divider, `-`) = `split-window -v`.
- gpui-component `TabBar` exposes `.suffix()` (bar-level) and `Tab::suffix()` (per-tab); a `TabBar`-level `on_click` ignores per-`Tab` `on_click`, so the per-tab `x` must be a suffix element with its own mouse handler and `stop_propagation`.
- Pane labels derive from the existing `rift_pane_command` subscription / `pane_current_command`; no new subscription.
- Window rename inline edit uses gpui-component's text `Input` (already a dependency); no new dependency.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| App shutdown moves from per-pane to session-end | The per-pane `cx.quit()` (`pane_view.rs:210`) fires whenever any pane's PTY loop ends — i.e. on every non-last pane/window close — tearing down the whole app. Shutdown belongs to the one event that means the session is gone: `ConnectionStatus::Disconnected`. | 2026-06-07 |
| Pane zoom (#42) dropped, not implemented | `resize-pane -Z` delivered no value for the current workflow; the screen space goes to direct pane manipulation instead. `spec-terminal-interaction-fixes.md` Outcome 4 is superseded and that spec archived COMPLETED on its remaining three outcomes (#39, #40, #41). | 2026-06-07 |
| Pane management surfaces as a left sidebar, not in-pane chrome | The list is the natural home for the per-pane agent-state display that `plugin-api` will feed later (Phase 3+); building it now establishes the surface. It deliberately reserves modest horizontal space for data that is name-only today. | 2026-06-07 |
| Sidebar shows only tmux-derived data; agent state comes from plugins | Core stays agent-agnostic (architecture rule). The sidebar renders pane name/command from the snapshot/subscription now; agent/dev-server state is added by plugins via `plugin-api`, never by agent-specific core code. | 2026-06-07 |
| Mouse-only; keyboard bindings deferred to key-table mirroring | rift-native ad-hoc shortcuts would pre-empt the prefix/key-table model owned by `spec-tmux-keytable-mirroring.md`. This spec ships the mouse affordances; bindings arrive with that spec. | 2026-06-07 |
| Snapshot remains the single source of truth for lifecycle | Same discipline as drag-to-resize (#41): emit the tmux command, let `%layout-change` / snapshot redraw. No optimistic local pane/window mutation, keeping the Phase 3 transport swap a single seam. | 2026-06-07 |

## Implementation notes (non-binding)

Integration points surfaced during investigation, for the implementor:

- **App shutdown:** remove `cx.quit()` at `crates/terminal/src/pane_view.rs:210` (end of the PTY read loop — a loop ending now just means that pane was dropped). Add `cx.quit()` on `ConnectionStatus::Disconnected` in the connection-status loop at `crates/terminal/src/session_view.rs:187-203`.
- **Tab bar:** `crates/terminal/src/session_view.rs` `render()` (~line 600). Add `TabBar::suffix(plus_button)` → `new-window`; build per-window `Tab` with `.suffix(close_x)` whose handler emits `kill-window -t <id>` with `stop_propagation`; middle-click via `on_mouse_down(MouseButton::Middle)`. The `.children(labels)` form becomes per-`Tab` construction.
- **Pane sidebar:** restructure the root at `session_view.rs:651-659` from `flex_col[tab_bar, pane_area, statusbar]` to `flex_col[tab_bar, flex_row[sidebar, pane_area], statusbar]`. Build rows from the active `WindowState.pane_ids`, reading each `PaneView`'s `current_command` via `self.panes`; the active pane is `self.active_pane_id`. Emit `select-pane` / `kill-pane` / `split-window -{h,v} -t <pane>` via `tmux_command_tx`.
- **Window rename:** double-click handler on the tab; swap the label for a gpui-component `Input` seeded with the window name; on submit emit `rename-window -t <id> <name>`. The `rift_window_name` subscription already reflects the change in the label.
- **Command seam:** `tmux_command_tx` (`session_view.rs`) is the single emission point; `main.rs` `cmd_handle` forwards each command to `send_command_async`.

## Tracking

The decomposition into steps lives as GitHub issues, one per Outcome, grouped under a milestone. This spec owns the design; the issues own progress.

- Milestone: [Pane & window management](https://github.com/skrischer/rift/milestone/5)
- Issues: exit no longer quits the app (#68), window tab bar `+`/`x` (#69), pane sidebar (#70), window rename (#71) — one per Outcome above

Each issue references this spec path in its body. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [x] `cargo clippy --workspace -- -D warnings` passes
- [x] `cargo test --workspace` passes
- [x] In a multi-pane session, `exit` in a pane closes only that pane and the app stays open; closing the last pane (session end) quits rift
- [x] `+` in the tab bar creates a window; `x` and middle-click on a tab close that window; a parallel native client attached to the session sees the window set change
- [x] Clicking a pane row focuses it (`select-pane`); the row `x` closes it (`kill-pane`); `|` / `-` split the active pane into side-by-side / stacked panes; focus lands in the new pane
- [x] Double-clicking a tab renames the window (`rename-window`); the label updates
- [x] All changes persist in the tmux layout / window set (visible to a native client), driven by the snapshot

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Removing the per-pane `cx.quit()` leaves no shutdown path → app never closes | Replace, don't remove: quit on `ConnectionStatus::Disconnected` in the existing status loop; verify last-pane `exit` closes the app |
| `Disconnected` also fires on initial SSH failure → quitting hides the error | Acceptable: no session means nothing to render, and the failure is already logged. Revisit if a reconnect UI lands (out of scope) |
| The per-tab `x` click also triggers the tab's `select-window` | Render `x` as a suffix element with its own `on_mouse_down` + `stop_propagation`; the `TabBar`-level `on_click` does not see it |
| `kill-pane` / `kill-window` on a pane running an agent loses work silently | Out of scope here (close-guard is a separate concern); documented so a later harden spec owns it |
| Inline rename input steals focus or leaks keystrokes into terminal input | Scope the `Input` to the tab; commit on Enter, cancel on Escape/blur; restore pane focus afterward |
| Sidebar width competes with terminal space | Fixed, modest width now; revisit when plugin-driven agent-state columns land |

## Decision log

- 2026-06-07: Spec created. Scope set to mouse-driven lifecycle (exit fix, tab `+`/`x`, pane sidebar, window rename). Pane zoom (#42) dropped; keyboard bindings, close-guard, and reorder/move deferred with reasons above.
- 2026-06-08: Exit fix (#68, PR #87). `cx.quit()` removed from the PTY read loop (`pane_view.rs`); a loop ending now just means that pane was dropped from the snapshot. Quit moved to `ConnectionStatus::Disconnected` in the connection-status loop (`session_view.rs`). `cx.update` returns the closure value directly (not `Result`), so the loop returns the inner `this.update(...)` Result for its existing `is_err()` break check.
- 2026-06-08: Tab bar (#69, PR #88). `+` as a bar-level `TabBar::suffix` → `new-window`; per-tab `x` as `Tab::suffix` with its own `on_mouse_down` + `stop_propagation`, plus middle-click, both → `kill-window -t <id>`. Confirmed a bar-level `TabBar::on_click` is wired onto every tab and overrides per-`Tab` `on_click`.
- 2026-06-08: Pane sidebar (#70, PR #89). Left, fixed-width (`PANE_SIDEBAR_WIDTH = 160.0`) vertical list; root restructured to `flex_col[tab_bar, flex_row[sidebar, pane_area], statusbar]`. Lifecycle commands extracted as pure helpers (`split_command`/`select_pane_command`/`kill_pane_command`) with unit tests. **H1 fix:** the sidebar width is subtracted before the tmux column count — `total_cols = ((viewport.width - PANE_SIDEBAR_WIDTH) / cell_size.width).floor()` — so the terminal grid matches the space it actually occupies.
- 2026-06-08: Window rename (#71, PR #90). Double-click a tab → inline gpui-component `Input` seeded with the name; Enter → `rename-window -t <id>` with `quote_tmux_name` quoting and a trimmed-empty guard; Escape/blur cancels; `needs_focus` hands focus to the input. **Integration with #69:** select-window dispatch moved onto the per-`Tab` `on_click` (which exposes `click_count()` for double-click) instead of a bar-level `on_click`, so rename coexists with the `x` close suffix, middle-click kill, and the `+` suffix on the same `TabBar`.
- 2026-06-08: Review follow-ups noted for later specs (none blocking; all out of this spec's scope): quit-on-`Disconnected` could become an auto-reconnect prompt once a reconnect UI exists; the 1px sidebar border is not yet subtracted in the column math (sub-cell, harmless); the existing `select-pane`-on-focus call site could reuse the new `select_pane_command` helper; the sidebar could hide at ≤1 pane and the pane list could scroll for many panes; closing the last pane / a pane running an agent still has no confirmation (owned by a future close-guard spec).
