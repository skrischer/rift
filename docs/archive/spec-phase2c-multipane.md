# Spec: Phase 2c -- Multi-Pane Awareness
> Status: COMPLETED
> Created: 2026-05-08
> Completed: 2026-05-20

Replaced monolithic `TerminalView` with `SessionView` + `PaneView` architecture. Each tmux pane gets its own VTE parser, laid out via GPUI flex from tmux coordinates. Snapshot-driven lifecycle handles pane creation/removal.

## Outcome (delivered)
- [x] `TerminalView` split into `SessionView` (container, pane map, window state) + `PaneView` (per-pane rendering, input, selection)
- [x] Pane-scoped channels: `PaneOutput { pane_id, bytes }` and `PaneInput { pane_id, bytes }` replace untagged byte streams
- [x] Per-pane `alacritty_terminal::Term` instances fed by pane-specific `%output`
- [x] Snapshot-driven pane lifecycle: diff `TmuxSnapshot` against known panes, create/remove `PaneView` entities
- [x] Early output buffering for `%output` arriving before pane creation (drained on pane init)
- [x] Split-tree layout reconstruction from tmux pane coordinates (binary H/V split detection, GPUI flex_basis proportions)
- [x] Focus routing: click-to-focus, keyboard input tagged with focused pane_id
- [x] Per-pane working directory from snapshot
- [x] Active pane ID no longer tracked via `Arc<Mutex<String>>` in app thread -- moved to message tagging

## Key decisions
| Decision | Rationale | Date |
|---|---|---|
| Coordinate-based split tree reconstruction (not layout string parser) | Simpler, works with existing `TmuxPaneState` fields; layout string parser deferred | 2026-05-08 |
| Keep `OscInterceptor` in PaneView despite CWD coming from snapshots | Harmless, may provide faster CWD updates than snapshot polling | 2026-05-08 |
| Early output buffer with cap | `%output` can arrive before snapshot creates the pane; buffer prevents data loss | 2026-05-10 |
| PaneTerminal with VoidListener | termy's public API; clipboard events handled via alternate path | 2026-05-10 |
| GPUI flex layout (not absolute positioning) | Proportional flex matches tmux layout; minor pixel differences accepted | 2026-05-12 |

## Known limitations
- No tab bar for window switching (deferred to Phase 2d)
- No drag-resize of splitters (tmux owns the layout)
- No pane zoom (`resize-pane -Z`) handling
- No adaptive output throttling (add if performance requires it)
- Split tree reconstruction may fail for exotic layouts (>3 splits); falls back to equal-flex

## Decision log
- 2026-05-08: Research completed across termy, tmuxy, and iTerm2 for multi-pane patterns.
- 2026-05-08: Chose snapshot-diff lifecycle over event-driven pane creation (simpler, avoids races).
- 2026-05-10: Implemented early output buffer to handle `%output` before pane materialization.
- 2026-05-12: Split-tree reconstruction working for H/V splits; complex layouts fall back gracefully.
- 2026-05-20: Phase 2c completed. All 6 steps implemented and validated.
