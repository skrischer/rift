# Spec: Terminal Rendering Migration (termy_terminal_ui)
> Status: COMPLETED
> Created: 2026-05-06
> Completed: 2026-05-07

Replaced custom terminal rendering in `crates/terminal/` with termy's `terminal_ui` crate (~14,900 LOC of production-grade rendering, mouse, shell integration). SSH architecture unchanged.

## Outcome (delivered)
- [x] GPUI pinned to termy's Zed git rev, API breakage fixed
- [x] `termy_terminal_ui` added as workspace dependency
- [x] `TerminalElement` replaced with `TerminalGrid`; old `grid.rs` deleted
- [x] `mouse.rs` replaced with `encode_mouse_report()`; AltGr support added
- [x] OSC interception (OSC 7/133), shell integration (`CommandLifecycle`), link detection (Ctrl+hover/click), render metrics
- [x] Structured debug-level tracing for OSC events, resize, PTY lifecycle, mouse mode, links
- [x] Final structure: `view.rs` (~830 LOC), `keyboard.rs`, `colors.rs` -- everything else from termy

## Key decisions
| Decision | Rationale | Date |
|---|---|---|
| Adopt `terminal_ui` wholesale instead of building rendering from scratch | 14,900 lines MIT-licensed, zero coupling to termy app | 2026-05-06 |
| Skip `runtime.rs` / `pane_terminal.rs` / locale modules | SSH-based architecture; local PTY irrelevant | 2026-05-06 |
| Keep our own `keyboard.rs` | termy's `keystroke_to_input` is `pub(crate)`, not usable externally | 2026-05-07 |
| Keep `colors.rs` | termy expects Hsla input; we map from alacritty's Color enum | 2026-05-07 |

## Known limitations
- File-path link detection does not work over SSH (`fs::canonicalize` checks local filesystem, not remote host)
- termy's keyboard API is `pub(crate)` -- we maintain our own `keyboard.rs`

## Decision log
- 2026-05-06: Decided to adopt terminal_ui. MIT licensed, zero termy-app coupling. Only blocker was GPUI version alignment.
- 2026-05-06: Confirmed SSH architecture unaffected -- terminal_ui operates on `alacritty_terminal::Term` state and byte streams, not PTY details.
- 2026-05-07: Migration complete (Phases 1-6). Filed known limitations for file-path links and keyboard API.
