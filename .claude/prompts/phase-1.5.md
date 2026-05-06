# rift — Phase 1.5: Native GPU rendering with GPUI

## Goal

Replace the Tauri + xterm.js stack from P1 with GPUI + alacritty_terminal. When done, rift is a native GPUI application that connects via SSH to a remote host, attaches to tmux, and renders terminal output through GPU-accelerated native rendering — no WebView, no browser-based terminal emulation.

The SSH connection logic from `crates/ssh/` stays. The entire frontend and rendering layer changes.

## Current state

Basic rendering works. The GPUI gate passed on Linux/X11 with GPUI 0.2.2. The worktree `feat+phase-1.5-gpui` has a functional terminal that connects via SSH, attaches to tmux, and renders output. What's missing is everything that makes it *usable*: resize propagation, proper keyboard encoding, selection, scrollback, cursor styles, and performance (no damage tracking, full repaint every frame).

## Reference projects

Three open-source GPUI terminal projects were analyzed. **termy is the primary reference** — MIT licensed, identical stack (GPUI + alacritty_terminal 0.26), production-quality.

### termy (MIT) — primary reference

Repository: `https://github.com/termy-org/termy`

| Feature | File | LOC | What to study |
|---------|------|-----|---------------|
| Terminal grid rendering | `crates/terminal_ui/src/grid.rs` | 3665 | Custom GPUI `Element` impl, damage-based paint, row cache, box drawing as quads |
| Render pipeline | `src/terminal_view/render.rs` | 3911 | Three-tier cache strategy (reuse/partial/full), cell → `CellRenderInfo` conversion |
| Term wrapper | `crates/terminal_ui/src/pane_terminal.rs` | — | `FairMutex<Term<VoidListener>>` + `FairMutex<Processor>`, damage snapshot extraction |
| Terminal runtime | `crates/terminal_ui/src/runtime.rs` | 2549 | PTY lifecycle, event loop, `TerminalEvent` enum, resize handling |
| Keyboard encoding | `crates/terminal_ui/src/keyboard.rs` | 1372 | Keystroke → PTY bytes, app mode, Kitty protocol, CSI u, all edge cases |
| tmux integration | `crates/terminal_ui/src/tmux/` | — | Control mode client, protocol parser, pane output routing |

**Key architectural patterns from termy:**

1. **Damage-based rendering.** alacritty_terminal tracks dirty cells via `Term::damage()`. termy extracts this into `TerminalDamageSnapshot` (Full / Partial with row+col spans). Only dirty rows get repainted. This is the single biggest performance win.

2. **Row-level paint cache.** Each row has `CachedRowPaintOps` containing background spans, text draw ops, and cached `ShapedLine` instances. Text shaping is expensive — caching shaped lines across frames is critical.

3. **Text batching.** Adjacent cells with identical style merge into a single `TextBatch`. Avoids per-cell text shaping calls. rift already does this (TextRun merging in `view.rs:381-393`).

4. **Custom Element, not div trees.** `TerminalGrid` implements `gpui::Element` directly with `request_layout()` / `paint()`. Never create one GPUI element per cell — that's O(cols*rows) elements per frame.

5. **Box drawing as pixel quads.** Unicode block elements (U+2580..U+259F), box drawing (U+2500..U+257F), and Braille patterns (U+2800..U+28FF) are rendered as pixel-snapped quads instead of font glyphs. This avoids anti-aliasing artifacts when line_height > 1.0. Same approach as Ghostty.

6. **PaneTerminal wrapper.** Thin bridge between GPUI and `Term<VoidListener>`:
   ```rust
   pub struct PaneTerminal {
       inner: FairMutex<PaneTerminalInner>,
       parser: FairMutex<ansi::Processor>,
   }
   ```
   `feed_output(&self, bytes: &[u8])` locks parser + term, advances parser. `take_damage_snapshot()` extracts dirty cells. `with_term()` provides read-only access for rendering.

7. **Three-tier cache strategy** (in `render.rs:118-145`):
   - **REUSE**: Damage empty + cache valid → skip completely
   - **PARTIAL**: alacritty reports partial damage → patch only dirty rows
   - **FULL**: Cache invalid or full damage → rebuild everything

### zTerm (CC BY-NC 4.0) — architecture reference only

Repository: `https://github.com/zerx-lab/zTerm`

**License is CC BY-NC 4.0 — no code reuse allowed.** Study for architecture patterns only:
- Clean crate separation: terminal model (no GPUI dependency) vs. UI crate
- Uses `gpui-component` library for UI chrome (scrollbars, context menus, tabs)
- Tab management at app level, not within terminal view

### onetcli — deferred

Repository: `https://github.com/nicepkg/onetcli`

SSH + SFTP + Terminal in one GPUI app. Relevant for later phases (file sync, SFTP). Not analyzed in detail yet.

## Prerequisite — hard gate

**PASSED.** GPUI 0.2.2 compiles and renders on Linux/X11.

## Context

Read these files before writing any code:
- `AGENTS.md` — coding principles, repo layout, architectural rules
- `ARCHITECTURE.md` — full system architecture
- `VISION.md` — project vision ("native performance, no Electron, no web runtime overhead")

### Architectural shift

P1.5 changes the rendering architecture. Previously (ARCHITECTURE.md): "The daemon sends pre-parsed cell data. No VTE parsing on the client side." Now: VTE parsing happens on the client via alacritty_terminal. The daemon is not part of P1.5 — the app connects directly via SSH (same as P1). When the daemon is introduced later (P3+), it may take over VTE parsing again, or the split may remain. That decision is deferred.

**Update ARCHITECTURE.md** at the end of this phase to reflect the current state.

### Why GPUI

GPUI is Zed's GPU-accelerated UI framework, licensed under Apache-2.0. Published as `gpui = "0.2.2"` on crates.io with an active third-party ecosystem (gpui-form, gpui-ui-kit, gpui-markup, etc.). Supports macOS (Metal), Linux (Vulkan/X11), and Windows (DirectX). We use GPUI for the rendering layer only.

### Key constraint — license boundary

Zed's application crates (`crates/terminal/`, `crates/project_panel/`, `crates/diagnostics/`, etc.) are GPL-3.0 and depend on 15+ Zed-internal crates. We cannot use, copy, or derive from them.

termy (MIT) is freely usable as reference and for pattern adaptation. Write all code independently — no copy-paste — but the patterns, data structures, and architectural decisions are fair game.

## Crate mapping — what happens to the existing workspace

Current workspace has 7 crates + Tauri app. Here's what changes:

| Crate | P1.5 action |
|-------|-------------|
| `crates/ssh/` (from PR #2) | **Keep unchanged.** SSH connection + PTY stream. This is the data source. |
| `crates/terminal/` | **Repurpose.** Currently empty (error types only). Becomes the GPUI terminal widget wrapping alacritty_terminal. |
| `crates/daemon/` | **Keep shell.** Not used in P1.5. Will be used in P3+. |
| `crates/tmux-core/` | **Keep shell.** Not used in P1.5. |
| `crates/explorer/` | **Keep shell.** Not used in P1.5. |
| `crates/protocol/` | **Keep.** Not used in P1.5, but don't delete — needed later for daemon communication. |
| `crates/plugin-api/` | **Keep shell.** Not used in P1.5. |
| `app/` (Tauri) | **Remove entirely.** Replace with `crates/app/` — pure Rust GPUI binary. |

Resulting active crates for P1.5:

```
rift/
├── Cargo.toml                # workspace
├── crates/
│   ├── app/                  # NEW — GPUI application binary
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   ├── ssh/                  # UNCHANGED — SSH connection + PTY (from PR #2)
│   │   ├── Cargo.toml
│   │   └── src/
│   ├── terminal/             # REPURPOSED — GPUI terminal widget + alacritty_terminal
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs        # public API: TerminalView, TerminalHandle
│   │       ├── view.rs       # TerminalView (impl Render), TerminalElement (impl Element)
│   │       ├── colors.rs     # color palette, alacritty Color → GPUI Hsla conversion
│   │       ├── keyboard.rs   # keystroke → PTY byte encoding
│   │       └── grid.rs       # CellRenderInfo, damage tracking, row paint cache
│   ├── daemon/               # kept, not used
│   ├── tmux-core/            # kept, not used
│   ├── explorer/             # kept, not used
│   ├── protocol/             # kept, not used
│   └── plugin-api/           # kept, not used
├── AGENTS.md
├── VISION.md
├── ARCHITECTURE.md
└── CLAUDE.md
```

## Scope — what to build

### 1. Remove Tauri, add GPUI app crate

Delete the `app/` directory (Tauri frontend, package.json, node_modules, TypeScript). P1's rendering stack is fully replaced — this is intentional and acknowledged.

Create `crates/app/` with:
- `Cargo.toml` depending on `gpui`, `rift-terminal` (crates/terminal), and `rift-ssh` (crates/ssh)
- `src/main.rs` — GPUI application entry point

### 2. Terminal widget (`crates/terminal/`)

This is the core of P1.5. Split into multiple files by responsibility.

#### 2a. TerminalView + TerminalElement (`view.rs`)

**TerminalView** is the GPUI `Render` implementor — the top-level component:
- Owns the `Term` state (via a wrapper, see 2c)
- Holds the `FocusHandle` for keyboard routing
- Attaches `on_key_down`, `on_mouse_down`, `on_mouse_move`, `on_mouse_up`, `on_scroll` listeners
- Returns a `TerminalElement` from `render()`

**TerminalElement** is the custom GPUI `Element` — the low-level render primitive:
- Implements `request_layout()`: requests size = parent bounds (fill available space)
- Implements `prepaint()`: locks `Term`, extracts visible cells, builds paint operations
- Implements `paint()`: draws background rects, shaped text, cursor, selection highlight

This two-struct pattern (View delegates to Element) matches termy's `TerminalView` → `TerminalGrid` split. The View handles state + events; the Element handles pixel output.

```
TerminalView (impl Render)
├── on_key_down → keyboard.rs → encode_keystroke() → input_tx channel
├── on_mouse_down/move/up → selection state management
├── on_scroll → scrollback offset adjustment
└── render() → TerminalElement {
        prepaint():
        ├── measure cell size (font metrics)
        ├── detect grid resize → notify PTY via size_changed_tx
        ├── lock Term, read damage snapshot
        ├── if damage == None → reuse cached paint state
        ├── if damage == Partial → rebuild only dirty rows
        ├── if damage == Full → rebuild all rows
        ├── per row: build CellRenderInfo[], shape text, cache ShapedLine
        └── collect: bg_rects, shaped_lines, cursor_quad, selection_rects

        paint():
        ├── paint background rects (batched by color)
        ├── paint shaped text lines
        ├── paint selection overlay
        └── paint cursor
    }
```

#### 2b. Keyboard encoding (`keyboard.rs`)

Convert GPUI `Keystroke` events to PTY input bytes. The current implementation (~45 LOC) handles only basics. This needs expansion to cover all terminal edge cases.

**Reference:** termy's `crates/terminal_ui/src/keyboard.rs` (1372 LOC) — the definitive implementation for GPUI keystroke → terminal bytes.

Required mappings:

| Input | Output | Notes |
|-------|--------|-------|
| Printable character | UTF-8 bytes | Direct passthrough |
| Enter | `\r` | |
| Shift+Enter | `\n` | |
| Tab | `\t` | |
| Shift+Tab | `\x1b[Z` | Back-tab |
| Escape | `\x1b` | |
| Backspace | `\x7f` | DEL, not BS |
| Ctrl+A..Z | `\x01`..`\x1a` | Control characters |
| Ctrl+C | `\x03` | SIGINT (via byte, not signal) |
| Alt+key | `\x1b` + key | Meta prefix |
| Arrow keys (normal) | `\x1b[A/B/C/D` | |
| Arrow keys (app mode) | `\x1bOA/OB/OC/OD` | When DECCKM is set |
| Home/End | `\x1b[H` / `\x1b[F` | |
| Page Up/Down | `\x1b[5~` / `\x1b[6~` | |
| Insert/Delete | `\x1b[2~` / `\x1b[3~` | |
| F1-F4 | `\x1bOP`..`\x1bOS` | SS3 form |
| F5-F12 | `\x1b[15~`..`\x1b[24~` | CSI form with gaps |

**Application cursor mode** is critical for tmux, Neovim, and any TUI. The `Term` tracks this state via `mode().contains(TermMode::APP_CURSOR)`. The encoder must check this flag.

**Kitty keyboard protocol** (progressive enhancement) is deferred — implement standard xterm encoding first.

#### 2c. Grid + damage tracking (`grid.rs`)

The performance-critical module. Manages the bridge between alacritty_terminal's `Term` and the GPUI render pipeline.

**CellRenderInfo** — the per-cell render data extracted from `Term`:

```rust
pub struct CellRenderInfo {
    pub col: usize,
    pub row: usize,
    pub ch: char,
    pub fg: Hsla,
    pub bg: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub wide: bool,           // first cell of a double-width character
    pub wide_spacer: bool,    // second cell of a double-width character (skip rendering)
    pub selected: bool,
}
```

**Reference:** termy's `CellRenderInfo` in `crates/terminal_ui/src/grid.rs` — same idea, adds `search_current`, `search_match`, `uses_terminal_default_bg`.

**Damage tracking:**

alacritty_terminal's `Term::damage()` returns an iterator of `LineDamageBounds` indicating which rows (and column ranges) changed since the last call to `Term::reset_damage()`.

```rust
pub enum DamageSnapshot {
    Full,
    Partial(Vec<DirtySpan>),
}

pub struct DirtySpan {
    pub row: usize,
    pub left: usize,
    pub right: usize,
}
```

**Reference:** termy's `TerminalDamageSnapshot` in `crates/terminal_ui/src/runtime.rs:~L200` — identical concept.

The render pipeline checks damage before rebuilding:
1. `take_damage_snapshot()` from Term
2. If `DamageSnapshot::Full` → rebuild all rows
3. If `DamageSnapshot::Partial(spans)` → rebuild only rows in span list
4. After rendering, call `term.reset_damage()`

**Row paint cache:**

Each row's paint output is cached as `CachedRowPaintOps`:
- Background color spans (contiguous cells with same bg → one rect)
- `ShapedLine` (the expensive GPUI text shaping output)
- Special character draw ops (box drawing quads, if implemented)

Cache is keyed by row index. On partial damage, only dirty rows are re-shaped.

**Reference:** termy's `CachedRowPaintOps` in `crates/terminal_ui/src/grid.rs:~L100`.

#### 2d. Colors (`colors.rs`)

Already implemented: Catppuccin Mocha palette with 256-color + truecolor support. Keep as-is. The conversion from alacritty's `Color` types (Named, Spec, Indexed) → GPUI `Hsla` is correct.

#### 2e. Selection (`view.rs` — mouse event handlers)

Mouse-based text selection for copy/paste:

- `on_mouse_down`: record start position (screen coords → grid col/row)
- `on_mouse_move` (with button held): update end position, mark cells as selected
- `on_mouse_up`: finalize selection
- Copy: Ctrl+Shift+C or selection → clipboard on mouse up
- Paste: Ctrl+Shift+V → read clipboard → write to PTY as input

Grid coordinate conversion:
```
col = (mouse_x - grid_origin_x) / cell_width
row = (mouse_y - grid_origin_y) / cell_height
```

**Reference:** termy handles selection client-side (not in alacritty). Selection state is separate from `Term` and influences `CellRenderInfo.selected` during cell extraction.

#### 2f. Scrollback (`view.rs`)

Scroll through terminal history with mouse wheel:

- Track `scroll_offset: usize` (0 = live view, >0 = scrolled into history)
- `on_scroll` event: adjust offset, clamp to `[0, term.history_size()]`
- When scrolled back: render from `term.grid().display_offset()` instead of live grid
- When new output arrives while scrolled: keep position (don't snap to bottom)
- Snap to bottom on keyboard input

alacritty_terminal manages scrollback buffer internally. Use `term.scroll_display(Scroll::Delta(n))` to adjust the viewport.

#### 2g. Cursor rendering

Support all three cursor styles:
- **Block**: filled rectangle at cursor position (character visible with inverted colors)
- **Underline**: thin rect at bottom of cell
- **Bar/Line**: thin vertical rect at left of cell (current implementation, 2px)

The active style comes from `term.cursor_style()`. Terminals switch styles via `DECSCUSR` escape sequence.

Add cursor blink: toggle visibility on a timer (typically 500ms on/500ms off). GPUI likely has a mechanism for periodic redraws.

**Reference:** termy's `TerminalCursorStyle` enum + cursor painting in `crates/terminal_ui/src/grid.rs`.

### 3. GPUI application (`crates/app/`)

Minimal application that:

- Opens a single window with TerminalView filling the entire area
- On startup, reads SSH config from environment variables (same as P1: `RIFT_SSH_HOST`, `RIFT_SSH_USER`, `RIFT_SSH_PORT`, `RIFT_SSH_KEY`)
- Establishes SSH connection via `crates/ssh/`
- Runs `tmux new-session -A -s rift` on the remote
- Wires PTY stream to the TerminalView
- Spawns background tasks for bidirectional PTY I/O

### 4. Tokio <> GPUI async bridge

GPUI has its own async executor. `crates/ssh/` uses Tokio. These must be bridged explicitly.

Implementation approach:
- Spawn a dedicated OS thread running a Tokio runtime (`tokio::runtime::Runtime::new()`)
- Use `flume` (MIT/Apache-2.0) for bidirectional communication between runtimes
- PTY reader (Tokio side) → `pty_tx` channel → GPUI side reads and feeds to `PaneTerminal.feed_output()`
- Keyboard input (GPUI side) → `input_tx` channel → Tokio side writes to PTY
- Resize events (GPUI side) → `size_tx` channel → Tokio side calls `pty.resize()`

Do NOT attempt to run Tokio inside GPUI's executor or vice versa. Two runtimes, one channel bridge.

**Known bug in current implementation:** `size_changed_tx` is created but never sent to. The resize is detected in `prepaint()` and `term.resize()` is called locally, but the remote PTY is never notified. The SSH side awaits `size_changed_rx` but never receives. Fix: send the new dimensions through the channel when grid size changes.

### 5. Box drawing characters (optional, quality improvement)

Unicode block elements and box drawing characters rendered as pixel-snapped quads instead of font glyphs. This eliminates gaps and anti-aliasing artifacts in TUI borders (tmux pane borders, Neovim splits, etc.).

Character ranges:
- Box drawing: U+2500..U+257F
- Block elements: U+2580..U+259F
- Braille patterns: U+2800..U+28FF

**Reference:** termy's special character handling in `crates/terminal_ui/src/grid.rs:~L2400` — detects these ranges and renders as `PaintQuad` instead of shaped text. Same approach as Ghostty.

Defer this if it delays the core implementation. Font-based rendering works, just looks slightly worse in TUIs.

### 6. Update CI

- Remove any Tauri/Node.js build steps
- Remove `app/src-tauri` from workspace members
- Add `crates/app` to workspace members
- Ensure GPUI compiles in CI (Ubuntu runner needs: `libxkbcommon-x11-dev`, `libfreetype-dev`, `libxcb1-dev`, `libxkbcommon-dev`)
- Keep excluding the app crate from CI clippy/test if GPU libs cause issues on headless runners (same pattern as before with `--exclude rift-app`)

### 7. Update ARCHITECTURE.md

After implementation is complete, update ARCHITECTURE.md to reflect:
- GUI framework is now GPUI (not Tauri)
- VTE parsing happens client-side (via alacritty_terminal)
- No WebView, no TypeScript, no Node.js
- The daemon architecture is unchanged in concept but deferred to P3+

## Scope — what NOT to build

- No multi-pane support (single fullscreen terminal)
- No connection UI (environment variables, same as P1)
- No file explorer, diagnostics, LSP
- No tmux control mode parsing (raw PTY only)
- No custom color scheme configuration (hardcoded palette)
- No search-in-scrollback (termy has this, defer)
- No ligature support
- No Kitty keyboard protocol (standard xterm encoding first)
- No daemon binary

## Key dependencies

| Dependency | Version | Purpose | License |
|---|---|---|---|
| `gpui` | 0.2.2 | UI framework, window, rendering | Apache-2.0 |
| `alacritty_terminal` | 0.26.0 | VTE parsing, terminal state machine, damage tracking | Apache-2.0 |
| `russh` | 0.46.x | SSH client (existing, in crates/ssh) | Apache-2.0 |
| `tokio` | 1.x | Async runtime for SSH I/O | MIT |
| `flume` | 0.12.x | Channel bridge between Tokio and GPUI | MIT/Apache-2.0 |

All dependencies pass `cargo deny check licenses`.

## Definition of done

1. `cargo run -p rift-app` — a native window opens (no WebView)
2. The app connects to a preconfigured remote host via SSH
3. A tmux session starts or reattaches
4. The remote shell prompt is rendered natively via GPUI
5. Interactive shell works (type commands, see output)
6. Resizing the window resizes the terminal grid correctly **and notifies the remote PTY**
7. ANSI colors render correctly (256-color + truecolor)
8. Cursor is visible, moves correctly, and reflects the style requested by the terminal (block/underline/bar)
9. Keyboard input works for all standard keys including Ctrl+combinations, Alt+combinations, function keys, and application cursor mode
10. Text selection via mouse drag works, Ctrl+Shift+C copies to clipboard
11. Ctrl+Shift+V pastes from clipboard into the terminal
12. Mouse wheel scrolls through scrollback history
13. Damage-based rendering: only dirty rows repaint (verify with render metrics or visual inspection during idle — cursor blink should not repaint the full grid)
14. `cargo clippy --workspace -- -D warnings` passes (excluding rift-app if needed for headless CI)
15. `cargo test --workspace` passes
16. `app/` directory is gone, no Tauri/Node.js artifacts remain
17. ARCHITECTURE.md is updated

## Risks and mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| GPUI rendering bugs on target platform | Medium | Document and workaround. File upstream issues. Gate already passed. |
| Tokio <> GPUI deadlock | Medium | Strict channel-only communication. No shared mutexes across runtimes. Term access only through `FairMutex` or `Mutex` within GPUI's thread. |
| alacritty_terminal API instability | Medium | Pin exact version (0.26.0). The crate is not semver-stable for external use — expect breakage on updates. termy also pins 0.26. |
| Font grid alignment (cell width/height precision) | Medium | Get this right before wiring PTY. Everything depends on pixel-perfect grid. Measure cell width via `text_system.shape_line()` on 'M', not via font metrics directly. |
| Damage tracking API | Low | alacritty_terminal 0.26 exposes `Term::damage()` and `Term::reset_damage()`. Confirmed working in termy. |
| GPUI on Windows headless CI | Low | Exclude app crate from CI checks (same pattern as Tauri). Test locally. |

## Plan B — if GPUI proves unviable

If GPUI has blocking issues (crashes, missing platform support, API too unstable):

1. **`wgpu` + custom renderer** — most control, highest effort. Alacritty itself uses this approach.
2. **`winit` + `vello`** — Google's GPU 2D renderer. Less mature but active development. MIT/Apache-2.0.
3. **Tauri v2 + Canvas renderer** — keep Tauri but replace xterm.js with a custom Canvas-based terminal renderer that receives pre-parsed cells. Lower performance ceiling but known-working platform.

Decision point: if GPUI blocks for >2 days on a platform-specific issue with no upstream fix, evaluate alternatives.

## Implementation order

Steps 1-5 are already done in the `feat+phase-1.5-gpui` worktree. Start from step 6.

| # | Step | Status | Notes |
|---|------|--------|-------|
| 1 | Gate check (GPUI opens window) | DONE | Linux/X11, GPUI 0.2.2 |
| 2 | Colored rect + text grid | DONE | JetBrains Mono 14pt, cell measurement via 'M' advance |
| 3 | alacritty_terminal integration | DONE | `Term<Listener>` + `Processor`, Arc<Mutex<>> |
| 4 | Tokio bridge | DONE | flume channels, dedicated OS thread |
| 5 | SSH PTY wiring + basic keyboard | DONE | SSH → tmux → shell works, basic keys functional |
| 6 | **Fix resize propagation** | TODO | `size_changed_tx` never sends. Wire it in `prepaint()` when grid dimensions change. |
| 7 | **Expand keyboard encoding** | TODO | New file `keyboard.rs`. Cover all mappings from table above. Check `TermMode::APP_CURSOR` for arrow keys. Ref: termy `keyboard.rs` |
| 8 | **Selection + copy/paste** | TODO | Mouse handlers on TerminalView. Grid coord conversion. Clipboard via GPUI API. Ref: termy selection in `terminal_view/` |
| 9 | **Scrollback** | TODO | `scroll_offset`, `on_scroll` handler, `term.scroll_display()`. Snap to bottom on input. |
| 10 | **Cursor styles + blink** | TODO | Read `term.cursor_style()`, render block/underline/bar. Timer for blink toggle. |
| 11 | **Damage tracking** | TODO | Extract `Term::damage()` before rendering. `DamageSnapshot` enum. Row paint cache with `ShapedLine` reuse. Ref: termy `grid.rs` + `render.rs` |
| 12 | **Box drawing quads** | OPTIONAL | Pixel-snapped rendering for U+2500..U+28FF. Only if time allows. |
| 13 | Cleanup | TODO | Remove Tauri `app/`, update CI, update ARCHITECTURE.md |
