# Spec: Terminal scrollback scrollbar

> Created: 2026-07-28

A visible vertical scrollbar overlay on a terminal pane that shows the viewport's
position within the scrollback and autohides when idle, so scrolling through
history has the same glanceable affordance every other rift list already has.
Roadmap Phase 49.

## Outcome

- [ ] When a terminal pane has scrollback above the viewport, a vertical scrollbar overlay is shown while scrolling or hovering, with a thumb whose size and position reflect the viewport within the total scrollback.
- [ ] The scrollbar reflects the pane's *composite* scroll position — the live `Term`'s own scrollback (`display_offset`) plus the pre-attach history block (`history_scroll` / `history_size` / `block_rows`) — not just one of them.
- [ ] The scrollbar autohides when the pane is idle at the bottom (live view) and reappears on scroll/hover.
- [ ] In alt-screen mode (a full-screen TUI, e.g. a coding agent) where there is no scrollback to traverse, no scrollbar is shown.
- [ ] Dragging the thumb scrolls the pane to the corresponding position across the whole composite range (live scrollback + pre-attach history block).

## Scope

### In scope

- A vertical scrollbar overlay for the terminal pane (`crates/terminal/src/pane_view.rs`), driven by the composite scroll state.
- Autohide / show-on-activity behaviour (final policy resolved at the gate).
- Drag-to-scroll on the thumb, IF accepted at the gate.

### Out of scope

- Horizontal scrollbar (the terminal grid wraps; there is no horizontal scroll).
- The session-strip / picker / panel scrollbars — already delivered (issue #804, gpui-component `Scrollbar` over a `ScrollHandle`).
- Changing the scroll model itself or the search-bar scroll (`scroll_to_match`) behaviour.
- A minimap-style overview — that is the editor's concern (Phase 55), not the terminal.

## Constraints

- The terminal's scroll is **composite and custom**, NOT a gpui-component `ScrollHandle`: scrolling drives alacritty's `term.grid().display_offset()` via `scroll_display(Scroll::Delta)` for the live scrollback and a separate `history_scroll` over a pre-attach history block (`crates/terminal/src/pane_view.rs`, `handle_scroll` around :704-734, whose doc comment calls it the "composite scroll"). So the vendored `Scrollbar::vertical(&handle)` + `.track_scroll` pattern the pickers use (session_picker.rs, root_picker.rs, #804) does **not** directly apply — the thumb geometry and position must be computed from the composite offset. Two viable implementations, an implementer's call (recorded as a prior decision, not a gate): a bespoke overlay element drawn from the composite offset, OR mirroring the composite offset into a synthetic `gpui::ScrollHandle` purely to drive the vendored `Scrollbar`'s rendering. Prefer whichever reuses gpui-component more without distorting the scroll model.
- Total scrollback length = `history_size` (live `Term` scrollback depth) + the pre-attach history `block_rows`; current position = `display_offset` + `history_scroll`. The scrollbar reads these; it does not own scroll state. The pre-attach block is fetched async (tmux capture) and is `None` until it arrives, so before capture the total uses `history_size` alone and the thumb proportion is provisional, updating when the block lands.
- Alt-screen panes carry no scrollback (`alt_screen` is already tracked in the composite scroll read) → no scrollbar.
- Agent-agnostic: a pure rendering affordance over PTY-derived scrollback; no agent awareness. Constitution-clean; no new dependency (gpui-component is already vendored).

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 49 row: reuse gpui-component's `Scrollbar` tied to the scroll position; `ScrollbarShow` for overflow-only visibility; the #804 picker-scrollbar precedent.
- [Category 1: GPUI Applications & Components](prior-art.md#category-1-gpui-applications--components) — gpui-component `crates/ui/src/scroll/` `Scrollbar` / `ScrollbarShow` (already vendored, used by the pickers/panels).

## Human prerequisites

- none

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The scrollbar is a read-only view over the existing composite scroll state; it does not introduce a new scroll owner | The terminal already owns scroll via `display_offset` + `history_scroll`; the bar renders position, the pane keeps authority | 2026-07-28 |
| Implementation: bespoke overlay from the composite offset OR a synthetic `ScrollHandle` mirroring it to drive the vendored `Scrollbar` — implementer's call, whichever reuses gpui-component more cleanly without distorting the composite model | The vendored `Scrollbar` assumes a `ScrollHandle`; the terminal's scroll is composite/custom, so a straight reuse is not guaranteed | 2026-07-28 |
| No scrollbar in alt-screen | No scrollback to traverse; `alt_screen` is already known at the composite-scroll read | 2026-07-28 |
| Drag-to-scroll on the thumb (v1) — dragging maps to the composite offset across the whole range | Accepted at the gate; real scrollbar behaviour, not just an indicator | 2026-07-29 |
| Autohide visibility — the bar shows on scroll/hover and fades when idle at the bottom (live view) | Accepted at the gate; terminal-conventional, least chrome | 2026-07-29 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged (one per implementable step)

## Verification

- [ ] `just ci` equivalent green for the `terminal`/`app` crates (fmt + clippy `-D warnings` + tests); locally the crates compile warm-target clean.
- [ ] QA: scrolling a terminal with scrollback shows the scrollbar; the thumb size is proportional to viewport/total and its position tracks the composite offset (both live scrollback and the pre-attach history block).
- [ ] QA: at the bottom (live view) the scrollbar autohides per the accepted policy; it reappears on scroll/hover.
- [ ] QA: a full-screen alt-screen app (e.g. a running agent TUI) shows no scrollbar.
- [ ] QA (if drag accepted): dragging the thumb scrolls the pane to that position across the whole composite range.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The vendored `Scrollbar` can't be driven cleanly by the composite scroll → wasted effort | The prior decision allows a bespoke overlay; the implementer picks the cleaner path after a short spike, does not force the vendored widget |
| Thumb geometry wrong at the live/pre-attach history boundary | Verification explicitly checks position across BOTH the live scrollback and the history block |
| Overlay steals mouse events from the terminal grid (selection, links) | The bar occupies only its own narrow track; hit-testing stays within it, mirroring the picker overlay pattern |

## Decision log

- 2026-07-28: Scoped from a develop-code read — the terminal scroll is composite/custom (alacritty `display_offset` + pre-attach `history_scroll`), so the #804 `Scrollbar`-over-`ScrollHandle` reuse is not guaranteed; the bar renders the composite position, with drag-to-scroll and visibility policy carried to the gate.
- 2026-07-29: Spec-acceptance gate — accepted. Drag-to-scroll (v1) and autohide visibility. Spec review (PR #914) returned APPROVE with only non-blocking nits, addressed.
