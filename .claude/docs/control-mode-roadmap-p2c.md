# Phase 2c: Multi-Pane Awareness — Implementation Plan (COMPLETED)

> Completed 2026-05-20. All steps implemented. SessionView + PaneView architecture, pane-scoped channels, snapshot-driven lifecycle, split-tree layout, focus routing.

Phase 2c replaces the current single-pane terminal view with a multi-pane architecture that renders all visible tmux panes in their tmux-native layout. Each pane is an independent GPUI view with its own VTE parser, arranged via GPUI's flex layout system.

## Research basis

Patterns drawn from three reference implementations:

| Project | Key takeaway for Phase 2c |
|---|---|
| **termy** | Per-pane `PaneTerminal` (public, MIT). Snapshot-diff for pane lifecycle. Tab-based window switching. Linear pane lookup. |
| **tmuxy** | Flat `HashMap<pane_id, PaneState>` for O(1) output routing. `%layout-change` as lifecycle source of truth. Early output buffering for `%output` arriving before pane creation. Adaptive throttling (4ms typing, 32ms bulk). |
| **iTerm2** | Counter-based resize loop prevention. Eager initial snapshot + lazy incremental updates. Layout string → binary split tree for native tab/split mapping. Pane registration callbacks for race-free creation. |

## Architecture overview

### Current (Phase 2b)

```
TerminalView (monolith, 1175 LOC)
├── terminal: Arc<Mutex<Term<Listener>>>     ← single VTE parser
├── input_tx: Sender<Vec<u8>>               ← keyboard → tmux
├── size_changed_tx: Sender<TermSize>
├── selection, paint_cache, osc, ...        ← per-pane concerns
└── statusbar (ssh label, cwd, size)

PtyChannels (in crates/app)
├── pty_tx: Sender<Vec<u8>>                 ← all output, no pane_id
├── input_rx, size_changed_rx
└── snapshot_tx: Sender<TmuxSnapshot>

Poll thread: TmuxNotification::Output { pane_id, bytes }
  → ignores pane_id
  → sends bytes via pty_tx
```

### Target (Phase 2c)

```
SessionView (new, container)
├── panes: HashMap<String, Entity<PaneView>>
├── windows: Vec<WindowState>                ← from TmuxSnapshot
├── active_window_id: String
├── active_pane_id: String
├── early_output_buffer: HashMap<String, Vec<u8>>
├── focus_handle: FocusHandle
└── statusbar

PaneView (refactored from TerminalView)
├── pane_terminal: PaneTerminal              ← termy's public type
├── input_tx: Sender<PaneInput>              ← keyboard → tmux (with pane_id)
├── focus_handle: FocusHandle
├── selection, paint_cache, ...              ← per-pane rendering state
└── pane_id: String

PtyChannels (updated)
├── pane_output_tx: Sender<PaneOutput>       ← output with pane_id
├── input_rx: Receiver<PaneInput>            ← input with pane_id
├── size_changed_rx: Receiver<TermSize>
└── snapshot_tx: Sender<TmuxSnapshot>
```

### Data flow

```
tmux server
  → %output %1 <data>
    → termy TmuxClient (internal parsing, octal decode)
      → TmuxNotification::Output { pane_id: "%1", bytes }
        → poll thread sends PaneOutput { pane_id: "%1", bytes }
          → SessionView receives via async channel
            → looks up pane "%1" in HashMap
              → PaneView.pane_terminal.feed_output(bytes)
                → GPUI render reads cell grid from PaneTerminal

tmux server
  → %layout-change (or other structural notification)
    → termy translates to NeedsRefresh
      → poll thread triggers refresh_snapshot()
        → sends TmuxSnapshot via snapshot_tx
          → SessionView diffs against known panes
            → creates/removes PaneViews
            → updates flex layout proportions
```

## Step 1: Refactor TerminalView → SessionView + PaneView

### Goal

Split `TerminalView` (1175 LOC) into two components. After this step, the app behaves identically — one pane, same rendering — but the architecture supports multiple panes.

### What moves to PaneView

Everything that is per-pane state or per-pane rendering:

| Field | Moves to |
|---|---|
| `terminal: Arc<Mutex<Term<Listener>>>` | Replaced by `pane_terminal: PaneTerminal` |
| `selection`, `selecting`, `prev_selection` | PaneView |
| `paint_cache` | PaneView |
| `cursor_blink_visible` | PaneView |
| `mouse_mode_active`, `hovered_link` | PaneView |
| `cell_size`, `grid_size` | PaneView |
| `command_lifecycle` | PaneView |

PaneView implements `Render` and produces a `TerminalGrid` for its pane, exactly as `TerminalView` does today.

PaneView receives an `input_tx: Sender<PaneInput>` where `PaneInput { pane_id: String, bytes: Vec<u8> }`. Keyboard and mouse input is tagged with the pane_id before sending.

### What stays in SessionView

| Field | Purpose |
|---|---|
| `panes: HashMap<String, Entity<PaneView>>` | All known panes |
| `active_pane_id: String` | Which pane has focus |
| `windows: Vec<WindowState>` | tmux window metadata (name, index, active) |
| `active_window_id: String` | Which window's panes are visible |
| `working_directory: Option<String>` | From active pane's snapshot |
| `ssh_label: SharedString` | Connection label for statusbar |
| `size_changed_tx: Sender<TermSize>` | Window-level resize |
| `focus_handle: FocusHandle` | Container focus |

SessionView implements `Render` and produces: window tab bar + flex layout of PaneViews + statusbar.

### PaneTerminal integration

Replace the manual `Arc<Mutex<Term<Listener>>>` + `Processor` + damage tracking with termy's `PaneTerminal`:

```rust
// termy_terminal_ui::PaneTerminal (public API)
pub fn new(size: TerminalSize, options: TerminalOptions) -> Self;
pub fn feed_output(&self, bytes: &[u8]);
pub fn resize(&self, new_size: TerminalSize);
pub fn size(&self) -> TerminalSize;
pub fn cloned_term_arc(&self) -> Arc<FairMutex<Term<VoidListener>>>;
```

The `cloned_term_arc()` gives access to the `Term` for cell extraction during rendering, same pipeline as today.

Note: `PaneTerminal` uses `VoidListener` (no event forwarding). Our current `Listener` only handles `ClipboardStore`. We need to check if we can hook clipboard events through a different path or extend PaneTerminal. If not, we keep our own `Term` wrapper initially and migrate later.

### OscInterceptor

Currently `TerminalView` uses `OscInterceptor` to extract CWD from OSC 7 before feeding bytes to `Term`. In tmux control mode, CWD comes from snapshots, not OSC 7. However, some programs emit OSC 7 regardless. Decision: keep `OscInterceptor` in PaneView for now — it's harmless and may provide faster CWD updates than snapshot polling.

### Files changed

- `crates/terminal/src/view.rs` — deleted, replaced by `session_view.rs` + `pane_view.rs`
- `crates/terminal/src/lib.rs` — replace `TerminalView` exports with `SessionView`, `PaneView`
- `crates/app/src/main.rs` — use SessionView instead of TerminalView

### Validation

App starts, single pane renders, input works, statusbar shows CWD. Identical to Phase 2b.

## Step 2: Channel type change

### PaneOutput and PaneInput

```rust
// crates/terminal/src/lib.rs (or types.rs)
pub struct PaneOutput {
    pub pane_id: String,
    pub bytes: Vec<u8>,
}

pub struct PaneInput {
    pub pane_id: String,
    pub bytes: Vec<u8>,
}
```

### TerminalHandle update

```rust
pub struct TerminalHandle {
    pub pane_output_tx: flume::Sender<PaneOutput>,   // was: pty_tx: Sender<Vec<u8>>
    pub input_rx: flume::Receiver<PaneInput>,         // was: Receiver<Vec<u8>>
    pub size_changed_rx: flume::Receiver<TermSize>,
    pub snapshot_tx: flume::Sender<TmuxSnapshot>,
}
```

### Poll thread update (crates/app/src/main.rs)

```rust
TmuxNotification::Output { pane_id, bytes } => {
    if pane_output_tx.send(PaneOutput { pane_id, bytes }).is_err() {
        should_exit = true;
        break;
    }
}
```

### Input thread update

```rust
while let Ok(input) = input_rx.recv() {
    if tmux_client.send_input(&input.pane_id, &input.bytes).is_err() {
        break;
    }
}
```

This eliminates the `Arc<Mutex<String>>` for active pane ID in the input thread — the pane_id is now part of the message. The active pane decision moves to PaneView/SessionView (GPUI side), which tags input with the focused pane's ID.

### Files changed

- `crates/terminal/src/lib.rs` — new types
- `crates/terminal/src/session_view.rs` — receive PaneOutput, route to PaneView
- `crates/terminal/src/pane_view.rs` — send PaneInput with pane_id
- `crates/app/src/main.rs` — PtyChannels updated, poll thread sends PaneOutput, input thread reads PaneInput

### Validation

Same as Step 1 — single pane, but data flows through PaneOutput/PaneInput with pane_id. Active pane ID no longer tracked via mutex in app thread.

## Step 3: Pane lifecycle from snapshots

### Snapshot diff

When SessionView receives a TmuxSnapshot:

1. Collect all pane IDs from the new snapshot (across all windows).
2. Compare against known panes in `self.panes`.
3. **New panes** (in snapshot but not in HashMap):
   - Create `PaneTerminal::new(size, options)` with dimensions from `TmuxPaneState`
   - Create `PaneView` entity via `cx.new()`
   - Replay early output buffer if any bytes were buffered for this pane_id
   - Add to `self.panes`
4. **Removed panes** (in HashMap but not in snapshot):
   - Drop the `Entity<PaneView>`
   - Remove from `self.panes`
5. **Existing panes** (in both):
   - Update layout position/size if changed (resize PaneTerminal)
   - Update is_active flag
6. Update `active_window_id`, `active_pane_id`, `working_directory`
7. Rebuild GPUI layout tree (re-render)

### Early output buffer

In the SessionView's output receiver (async task spawned in `new()`):

```rust
match self.panes.get(&output.pane_id) {
    Some(pane_view) => {
        // Route to existing pane
        pane_view.update(cx, |pane, cx| {
            pane.feed_output(&output.bytes);
            cx.notify();
        });
    }
    None => {
        // Pane doesn't exist yet — buffer for replay after snapshot
        self.early_output_buffer
            .entry(output.pane_id.clone())
            .or_default()
            .extend_from_slice(&output.bytes);
    }
}
```

On pane creation (from snapshot diff), check and drain the buffer:

```rust
if let Some(buffered) = self.early_output_buffer.remove(&pane_id) {
    pane_terminal.feed_output(&buffered);
}
```

### Window state tracking

```rust
struct WindowState {
    id: String,
    name: String,
    index: i32,
    is_active: bool,
    pane_ids: Vec<String>,   // which panes belong to this window
}
```

Built from `TmuxSnapshot.windows`. Used for:
- Tab bar rendering (Step 5)
- Filtering which panes are visible (only active window's panes)

### Files changed

- `crates/terminal/src/session_view.rs` — snapshot diff logic, early output buffer, window state
- `crates/terminal/src/pane_view.rs` — `feed_output()` method, resize support

### Validation

Start app → single pane works. SSH into remote, run `tmux split-window` → second PaneView appears (may not be laid out correctly yet — that's Step 4).

## Step 4: GPUI flex layout from snapshot

### Layout strategy

Each `TmuxPaneState` in the snapshot has `left, top, width, height` in cell coordinates. The total window size is known from the tmux client size. From these coordinates, we reconstruct a split tree.

### Split tree reconstruction

Given panes with coordinates, reconstruct a binary tree of horizontal/vertical splits:

```
Input: [
  { id: "%0", left: 0, top: 0, width: 65, height: 40 },
  { id: "%1", left: 66, top: 0, width: 64, height: 40 },
]
→ HSplit(PaneView[%0] 50%, PaneView[%1] 50%)

Input: [
  { id: "%0", left: 0, top: 0, width: 130, height: 20 },
  { id: "%1", left: 0, top: 21, width: 130, height: 19 },
]
→ VSplit(PaneView[%0] 50%, PaneView[%1] 50%)
```

Algorithm:
1. If only one pane in the set → return PaneView leaf
2. Try to split the set horizontally: find a vertical boundary `x` where all panes are either entirely left or entirely right of `x`
3. If found → HSplit with proportional flex_basis from widths
4. Else try vertical boundary `y`
5. Recurse on each half

### GPUI rendering

```rust
enum LayoutNode {
    Pane(String),                                    // pane_id
    Split { direction: Axis, children: Vec<(f32, LayoutNode)> }, // (proportion, child)
}

fn render_layout_node(&self, node: &LayoutNode, cx: &mut Context<Self>) -> impl IntoElement {
    match node {
        LayoutNode::Pane(pane_id) => {
            self.panes[pane_id].clone().into_any_element()
        }
        LayoutNode::Split { direction, children } => {
            let mut container = div().flex();
            container = match direction {
                Axis::Horizontal => container.flex_row(),
                Axis::Vertical => container.flex_col(),
            };
            for (proportion, child) in children {
                container = container.child(
                    div()
                        .flex_basis(relative(*proportion))
                        .child(self.render_layout_node(child, cx))
                );
            }
            container.into_any_element()
        }
    }
}
```

### Splitter rendering

Between adjacent panes, render a 1px border line. This is purely visual — no drag-resize (tmux owns the layout). Can be done with GPUI borders on the flex children.

### Inactive pane dimming

Panes that don't have focus get a subtle overlay or reduced opacity, following termy's pattern. Active pane shows blinking cursor, inactive panes show a block cursor or no cursor.

### Files changed

- `crates/terminal/src/session_view.rs` — layout tree construction from snapshot, recursive GPUI rendering
- `crates/terminal/src/layout.rs` (new) — split tree reconstruction algorithm

### Validation

`tmux split-window -h` → two panes side by side. `tmux split-window -v` → pane splits vertically. Layout proportions match tmux. Resize window → panes re-proportion correctly.

## Step 5: Window tabs

### Tab bar

Rendered by SessionView above the pane layout area. Shows one tab per tmux window from the snapshot.

```
[1: bash] [2: claude *] [3: vim]
```

Active window is highlighted. Tab shows `window.index: window.name`. Star or similar indicator for the currently active window.

### Switching

Click on a tab → SessionView calls `tmux_client.send_command_async("select-window -t @<window_id>")`. tmux emits notifications → NeedsRefresh → snapshot refresh → pane layout rebuilds for the new window's panes.

Keyboard shortcut: Ctrl+Shift+1..9 for window 1..9 (configurable later). No tmux prefix needed — GPUI intercepts the key event before it reaches the PTY.

### Inactive window pane state

Panes belonging to inactive windows are NOT rendered but their PaneTerminals stay in memory. `%output` continues to be routed to them. When the user switches back, the VTE state is current — no re-hydration needed.

### Files changed

- `crates/terminal/src/session_view.rs` — tab bar rendering, window switch command
- `crates/terminal/src/pane_view.rs` — no changes

### Validation

Create multiple tmux windows (`tmux new-window`). Tab bar shows them. Click to switch. Pane content is preserved when switching back.

## Step 6: Focus and input routing

### Focus model

Each PaneView has its own `FocusHandle`. SessionView manages which pane is focused:

- On click inside a PaneView → that PaneView gains GPUI focus
- SessionView detects focus change → updates `active_pane_id`
- Optionally sends `select-pane -t %<pane_id>` to tmux to sync server-side active pane

### Keyboard input routing

PaneView's keyboard handler tags input with its `pane_id`:

```rust
fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
    let bytes = keyboard::encode_key(event);
    let _ = self.input_tx.send(PaneInput {
        pane_id: self.pane_id.clone(),
        bytes,
    });
}
```

Since GPUI only delivers key events to the focused element, input automatically goes to the correct pane.

### Visual focus indicator

- Active pane: normal cursor (blinking if configured)
- Inactive panes: static block cursor or hidden cursor, optional dim overlay

### Files changed

- `crates/terminal/src/pane_view.rs` — focus-aware rendering, tagged input
- `crates/terminal/src/session_view.rs` — focus change detection, select-pane sync

### Validation

Two panes visible. Click on left pane → typing goes to left. Click on right pane → typing goes to right. Active pane has blinking cursor, inactive has static/dimmed cursor.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| PaneTerminal uses VoidListener — no clipboard events | Check if `Term::events()` or polling works. Fallback: keep custom Term wrapper for clipboard pane only. |
| Split tree reconstruction from coordinates is fragile | Start with simple cases (1-2 splits). Fall back to equal-flex if reconstruction fails. Can later parse tmux layout string instead. |
| GPUI flex layout doesn't match tmux cell sizes exactly | Accept minor pixel differences. tmux owns the layout; our rendering is approximate. |
| Performance with many panes (>10) | Unlikely in practice. Only render active window's panes. PaneTerminals for hidden windows are cheap (no rendering). |
| Early output buffer grows unbounded for leaked panes | Cap buffer at 1MB per pane_id. Drop oldest bytes if exceeded. Clean up entries older than 5 seconds. |

## Out of scope (Phase 2d+)

- Subscriptions for CWD (`refresh-client -B`) — stays snapshot-based
- Pane zoom (`resize-pane -Z`) handling
- Drag-resize splitters in GUI (tmux owns layout)
- Layout string parser (using coordinate-based reconstruction instead)
- Session switching (single session for now)
- Adaptive throttling (tmuxy pattern) — add if performance requires it
