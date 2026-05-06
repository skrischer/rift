# Terminal Roadmap

Feature backlog for `crates/terminal/`. Tracked across sessions.

## Termy Reference

Local checkout: `/home/developer/CascadeProjects/termy/`
Key files for reference:

| Feature | Termy file |
|---------|-----------|
| Mouse protocol | `crates/terminal_ui/src/mouse_protocol.rs` |
| OSC interception | `crates/terminal_ui/src/osc_intercept.rs` |
| Shell integration | `crates/terminal_ui/src/shell_integration.rs` |
| Keyboard (Kitty) | `crates/terminal_ui/src/keyboard.rs` |
| Grid / box-drawing | `crates/terminal_ui/src/grid.rs` |
| Link detection | `crates/terminal_ui/src/links.rs` |
| Search | `src/terminal_view/search.rs` |
| Scrollbar | `src/terminal_view/scrollbar.rs` |
| Render metrics | `crates/terminal_ui/src/render_metrics.rs` |
| tmux control mode | `crates/terminal_ui/src/tmux/` |

## Legend

- [ ] Not started
- [x] Done
- [~] In progress

---

## P0 — Security / Blocker

- [x] **SSH server key verification** — known_hosts parsing, TOFU, mismatch rejection. PR #8, merged 2026-05-06.

## P1 — Core Terminal Features

- [~] **Mouse protocol** — SGR/UTF-8 mouse encoding, send events to PTY. Required for tmux mouse mode, vim mouse, etc. Multi-step:
  - [x] Mouse event encoding (SGR + UTF-8 + Normal modes, 40 tests). PR #7, merged 2026-05-06.
  - [ ] Terminal mode tracking (mouse reporting modes)
  - [ ] View integration (route mouse events to PTY vs selection)

- [ ] **OSC interception** — Parse and act on Operating System Command sequences. Multi-step:
  - [ ] OSC 7: working directory tracking
  - [ ] OSC 133: shell integration (prompt/command lifecycle)
  - [ ] OSC 8: hyperlink support
  - [ ] OSC 9/777: desktop notifications

- [ ] **Error handling hardening** — Replace 14x `.expect()` on mutex locks in `view.rs` with graceful recovery. One-shot.

## P2 — Quality of Life

- [ ] **In-terminal search** — Regex search through scrollback with match highlighting. Multi-step:
  - [ ] Search engine (regex matching across grid + scrollback)
  - [ ] Search UI (input bar, match counter, next/prev)
  - [ ] Scrollbar match markers

- [ ] **Link detection** — Recognize URLs/file paths, make clickable on hover. Multi-step:
  - [ ] URL/path recognition with LRU cache
  - [ ] Hover highlight + click handler

- [ ] **Configurable scrollback** — Expose scrollback size as config value (currently hardcoded 1000 lines). One-shot.

- [ ] **Missing text attributes** — Render Dim, Blink, Conceal, Overline. One-shot per attribute.

## P3 — Polish

- [ ] **Resize throttling** — Debounce resize events (~32ms). One-shot.
- [ ] **Box-drawing as geometry** — Render box-drawing chars (U+2500..U+257F) as quads instead of glyphs for pixel-perfect lines.
- [ ] **Selection improvements**:
  - [ ] Double-click to select word
  - [ ] Triple-click to select line
  - [ ] Right-click context menu
- [ ] **Scrollbar UI** — Visible scrollbar with hover/fade, position indicator.
- [ ] **Render metrics** — Debug mode for cache hit/miss stats.

## P4 — Future / Phase 3+

- [ ] **tmux control mode** — Native tmux integration for pane/session management.
- [ ] **Shell integration UI** — Command lifecycle display, elapsed time, exit codes.
- [ ] **Image protocols** — Sixel, iTerm2 inline images, kitty graphics.
- [ ] **Kitty keyboard protocol** — Enhanced keystroke mode.
