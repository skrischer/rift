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
| `keyboard.rs` (keystroke_to_input, Kitty protocol) | 1,372 | -- (termy's is `pub(crate)`, we keep ours) |
| `mouse_protocol.rs` (encode_mouse_report) | 305 | `mouse.rs` (deleted) |
| `osc_intercept.rs` (OSC 7/8/9/133/777) | 432 | nothing (new feature) |
| `shell_integration.rs` (command lifecycle) | 252 | nothing (new feature) |
| `links.rs` (URL/path detection) | 527 | nothing (new feature) |
| `render_metrics.rs` | 220 | nothing (new feature) |
| `tmux/` (control mode client, parser, sessions) | 4,313 | nothing (future) |

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

## Phase 1: GPUI Upgrade -- DONE

Pinned GPUI to termy's Zed git rev. Fixed API changes in app + terminal crates.

## Phase 2: Add terminal_ui dependency -- DONE

Added `termy_terminal_ui` to workspace and terminal crate.

## Phase 3: Replace rendering -- DONE

Replaced `TerminalElement` with `TerminalGrid`. Deleted `grid.rs`. Adapter function converts `alacritty_terminal::Term` cells to `CellRenderInfo`.

## Phase 4: Replace input handling -- DONE

Replaced `mouse.rs` with `encode_mouse_report()`. Kept `keyboard.rs` (termy's keyboard API is `pub(crate)`). Added AltGr support for German keyboard layouts.

## Phase 5: Wire new features -- DONE

- OSC interception: `OscInterceptor` filters bytes before VTE parser, extracts OSC 7/133 events
- Shell integration: `CommandLifecycle` tracks command phase (idle/prompt/input/executing)
- Link detection: Ctrl+hover detects URLs (regex + OSC 8 hyperlinks), Ctrl+click opens them
- Render metrics: Damage computation timed, grid paint/shaping/cache metrics tracked automatically
- Structured logging: debug-level tracing for OSC events, resize, PTY lifecycle, mouse mode, links

## Phase 6: Cleanup -- DONE

- No dead files remaining (grid.rs, mouse.rs already deleted in Phase 3-4)
- `colors.rs` still needed (termy expects Hsla input, we map from alacritty's Color enum)
- `keyboard.rs` still needed (termy's is pub(crate))
- ARCHITECTURE.md updated with termy_terminal_ui references
- lib.rs exports updated with TerminalUiRenderMetricsSnapshot

### Final file structure of `crates/terminal/`:
```
src/
  lib.rs          -- public exports (TermSize, TerminalHandle, TerminalView, RenderMetrics)
  view.rs         -- TerminalView (~830 lines: render, OSC, links, shell integration, mouse, selection)
  keyboard.rs     -- keystroke encoding (termy's is pub(crate), so we keep ours)
  colors.rs       -- Catppuccin Mocha theme, alacritty Color -> Rgba mapping
```

Everything else comes from `termy_terminal_ui`.

---

## Decision log

- 2026-05-06: Decided to adopt `terminal_ui` instead of building rendering from scratch. Rationale: 14,900 lines of production-grade rendering, keyboard, mouse, tmux, and shell integration. MIT licensed, zero coupling to termy app. Only blocker is GPUI version alignment.
- 2026-05-06: SSH architecture is unaffected: `terminal_ui` operates on `alacritty_terminal::Term` state and byte streams, not on PTY implementation details. We skip `runtime.rs` (local PTY) entirely.
- 2026-05-07: Migration complete. Phases 1-6 done. File-path link detection limited over SSH (canonicalize runs locally). termy's keyboard API is pub(crate) so we keep our own keyboard.rs.

## Known limitations

- File-path link detection doesn't work over SSH (termy's `find_link_in_line` uses `fs::canonicalize` which checks the local filesystem, not the remote host)
- termy's `keystroke_to_input` is `pub(crate)`, so we maintain our own keyboard encoding in `keyboard.rs`

## Not in scope

These features live in termy's app layer (`src/terminal_view/`), not in `terminal_ui`:
- In-terminal search (search.rs) -- build ourselves when needed
- Scrollbar UI (scrollbar.rs) -- build ourselves when needed
- Tabs / pane management -- our own architecture
- Image protocols (sixel, kitty graphics) -- future
