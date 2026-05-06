# Terminal Migration: termy_terminal_ui

Step-by-step plan to replace our custom terminal rendering with termy's `terminal_ui` crate.

## Context

- Termy checkout: `/home/developer/CascadeProjects/termy/`
- Crate: `termy_terminal_ui` (MIT, zero internal termy dependencies)
- We use SSH, not local PTY. termy's `runtime.rs` / `Terminal` struct is irrelevant to us.
- Our SSH crate and `main.rs` wiring stay unchanged.

## What we take from terminal_ui

| Module | Lines | Replaces our... |
|--------|------:|-----------------|
| `grid.rs` (TerminalGrid, CellRenderInfo, paint cache, box-drawing geometry) | 3,665 | `view.rs` TerminalElement + `grid.rs` CellRenderInfo |
| `keyboard.rs` (keystroke_to_input, Kitty protocol) | 1,372 | `keyboard.rs` (563L) |
| `mouse_protocol.rs` (encode_mouse_report) | 305 | `mouse.rs` (943L) |
| `osc_intercept.rs` (OSC 7/8/9/133/777) | 432 | nothing (new feature) |
| `shell_integration.rs` (command lifecycle) | 252 | nothing (new feature) |
| `links.rs` (URL/path detection) | 527 | nothing (new feature) |
| `render_metrics.rs` | 220 | nothing (new feature) |
| `tmux/` (control mode client, parser, sessions) | 4,313 | nothing (was Phase 3+ roadmap item) |

## What we DON'T take

| Module | Reason |
|--------|--------|
| `runtime.rs` (Terminal struct, local PTY) | We use SSH, not fork/exec |
| `pane_terminal.rs` | termy-specific integration layer |
| `locale.rs`, `path_env.rs` | Local-system utilities, not needed over SSH |

## What stays unchanged

- `crates/ssh/` -- untouched, still provides the byte stream
- `main.rs` wiring -- SSH -> `pty_tx`, `input_rx` -> SSH, resize -> SSH
- `alacritty_terminal::Term` -- we still feed bytes into it and read grid state

---

## Phase 1: GPUI Upgrade

**Goal:** Pin GPUI to termy's rev so we can depend on `terminal_ui`.

Current: `gpui = "0.2.2"` (crates.io)
Target: `gpui = { git = "https://github.com/zed-industries/zed", rev = "83de8a25..." }`

Steps:
1. Update workspace `Cargo.toml` to use termy's GPUI git rev
2. `cargo build --workspace` -- fix compile errors from API changes
3. Our GPUI surface is ~2,700 lines across 3 crates (app, terminal, ssh). SSH doesn't use GPUI, so only app + terminal need fixes.
4. `cargo clippy --workspace -- -D warnings` + `cargo test --workspace`

Risk: GPUI API changes between 0.2.2 and the Zed commit. Likely affected:
- Element trait signature (request_layout, prepaint, paint)
- Window/App context API
- Text shaping API

Mitigation: termy's `grid.rs` shows exactly what the new API looks like.

## Phase 2: Add terminal_ui dependency

**Goal:** Add the crate without changing any existing code yet.

Steps:
1. Add to workspace `Cargo.toml`:
   ```toml
   termy_terminal_ui = { git = "<termy-repo>", path = "crates/terminal_ui" }
   ```
2. Add to `crates/terminal/Cargo.toml` as dependency
3. `cargo build --workspace` -- confirm it compiles alongside our existing code
4. No code changes. Both implementations coexist.

## Phase 3: Replace rendering

**Goal:** Swap our `TerminalElement` for termy's `TerminalGrid`.

This is the core migration step. Our `view.rs` goes from ~800 lines to ~200.

### What gets deleted from our code:
- `TerminalElement` struct + `impl Element` + `impl IntoElement` (~370 lines)
- `TerminalPrepaintState` struct
- `CachedBgSpan`, `CachedRow`, `RowCacheState` structs + impl (~90 lines)
- `grid.rs`: `CellRenderInfo`, `DamageSnapshot`, `DirtySpan`, `extract_row_cells` (replaced by termy types)

### What gets written:
- Adapter function: read `alacritty_terminal::Term` grid -> produce `termy_terminal_ui::CellRenderInfo` rows + damage info -> build `TerminalGrid`
- Updated `TerminalView::render()`: build a `TerminalGrid` instead of a `TerminalElement`

### Key mapping:

| Ours | termy |
|------|-------|
| `grid::CellRenderInfo` | `termy_terminal_ui::CellRenderInfo` |
| `grid::DamageSnapshot` | `termy_terminal_ui::TerminalGridPaintDamage` |
| `RowCacheState` | `termy_terminal_ui::TerminalGridPaintCacheHandle` |
| `TerminalElement` | `termy_terminal_ui::TerminalGrid` |

Verify: `cargo test --workspace`, run app, check rendering visually (htop, lazygit for box-drawing).

## Phase 4: Replace input handling

**Goal:** Use termy's keyboard + mouse encoding.

### Keyboard:
- Replace `keyboard::encode_keystroke()` calls with `termy_terminal_ui::keystroke_to_input()`
- Delete `crates/terminal/src/keyboard.rs`
- Gains: Kitty keyboard protocol support

### Mouse:
- Replace `mouse::encode_mouse_event()` with `termy_terminal_ui::encode_mouse_report()`
- Delete `crates/terminal/src/mouse.rs`
- Wire mouse events in `TerminalView::render()`: check terminal mouse mode, route to PTY or local selection
- Gains: Mouse events actually reach the PTY (vim mouse, tmux mouse, etc.)

Verify: `cargo test --workspace`, test keyboard in vim/tmux, test mouse in tmux.

## Phase 5: Wire new features

**Goal:** Enable features that come free with terminal_ui.

These are independent and can be done in any order:

### OSC interception
- Instantiate `OscInterceptor` in the VTE processing loop
- Handle OSC 7 (working directory tracking)
- Handle OSC 133 (shell integration / prompt markers)

### Link detection
- Use `find_link_in_line()` on hover
- Add click handler for detected URLs

### Shell integration
- Expose `CommandLifecycle` events (prompt start, command start, command end + exit code)
- Foundation for future UI (command duration, exit status indicators)

### Render metrics
- Wire up in debug builds for performance profiling

## Phase 6: Cleanup

**Goal:** Remove dead code, update docs.

- Delete remaining unused files from `crates/terminal/src/`
- Update `crates/terminal/src/lib.rs` exports
- Update ARCHITECTURE.md if crate responsibilities changed
- Remove `crates/tmux-core/` from roadmap -- covered by `terminal_ui::tmux`
- `colors.rs`: evaluate if still needed or if termy's color handling covers it

### Final file structure of `crates/terminal/`:
```
src/
  lib.rs          -- public exports
  view.rs         -- TerminalView (Render impl, event handlers, ~200 lines)
  colors.rs       -- theme mapping (if still needed)
```

Everything else comes from `termy_terminal_ui`.

---

## Decision log

- 2026-05-06: Decided to adopt `terminal_ui` instead of building rendering from scratch. Rationale: 14,900 lines of production-grade rendering, keyboard, mouse, tmux, and shell integration. MIT licensed, zero coupling to termy app. Only blocker is GPUI version alignment.
- SSH architecture is unaffected: `terminal_ui` operates on `alacritty_terminal::Term` state and byte streams, not on PTY implementation details. We skip `runtime.rs` (local PTY) entirely.

## Not in scope

These features live in termy's app layer (`src/terminal_view/`), not in `terminal_ui`:
- In-terminal search (search.rs) -- build ourselves when needed
- Scrollbar UI (scrollbar.rs) -- build ourselves when needed
- Tabs / pane management -- our own architecture
- Image protocols (sixel, kitty graphics) -- future
