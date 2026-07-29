# Spec: Global UI font size

> Created: 2026-07-29

A UI font-size control that resizes the editor, explorer, and chrome (not only the
terminal), persisted across restarts, and a corrected label for the existing
terminal-only "Font size" setting. Roadmap Phase 54.

## Outcome

- [ ] A "UI font size" setting resizes the theme-driven interface surfaces — the editor and dock panels (`mono_font_size`) and the explorer rows (`text_sm`/`text_xs`, which resolve against the theme base `font_size` via gpui-component's `set_rem_size`) — live, without restart.
- [ ] The UI font size is persisted and restored across app restarts.
- [ ] The terminal PTY grid keeps its own separate size control (today's "Font size"), and that control is relabelled to name the terminal (no longer the misleading "Base size for editor and terminal").
- [ ] Changing the UI font size does not change the terminal grid size, and vice versa.

## Scope

### In scope

- A new UI font-size setter (analogous to `set_ui_font_persisted` in `crates/app/src/lib.rs:314`) that sets the active gpui-component theme's base `font_size` and `mono_font_size` at runtime, applies it live, and persists it.
- A "UI font size" control in the settings "Font & Size" group (`crates/app/src/settings.rs:219-266`).
- Persisting the UI font size in `WindowState` (`crates/app/src/window_state.rs`) — a new field distinct from the existing terminal `font_size_px`.
- Relabelling the existing "Font size" control (which mutates only `SessionView::font_size`, the terminal grid — `settings.rs:250-265`) to name the terminal.

### Out of scope

- Wiring the terminal PTY grid to the UI font-size setting — the grid is pinned to a Nerd Font and its size ties to cell-size measurement; it stays a separate control (settings.rs comment).
- Per-surface font sizes (explorer vs editor vs chrome independently) — one global UI size cascades via the theme; per-surface is deferred behind a real need (constitution: no premature abstraction).
- New font families or a font picker (families already exist as separate settings).
- Fixed-`px` chrome that does not read the theme rem size — the title bar (`title_bar.rs`), status bar (`status_bar.rs`), and pre-connection screens (connection card, session/root pickers) use hardcoded `px(...)` and will NOT scale with the UI font size; making them scale is a known follow-up, out of scope here (QA must not fail on an unchanged title/status bar).

## Constraints

- The existing "Font size" number input mutates only `SessionView::font_size()` / `set_font_size()` — the terminal PTY grid (`settings.rs:250-265`), despite its "editor and terminal" label. The editor and dock panels render at `cx.theme().mono_font_size`; the explorer and chrome use `text_sm`/`text_xs`, which derive from the theme's base `font_size`. So a "UI font size" must set the theme's base `font_size` (+ `mono_font_size`) at runtime, the same mutation path `set_ui_font_persisted` uses for `font_family`.
- No existing size plumbing to reuse: `set_ui_font_persisted(name, window, cx)` takes a `Window`, not a size (`lib.rs:314`) — the size setter is new.
- Persistence follows the existing `WindowState` pattern (targeted read-modify-write savers like `save_theme` / `save_ui_font_family`); `WindowState` already holds the terminal `font_size_px`, `ui_font_family`, `mono_font_family` — add a UI font-size field alongside.
- UI and terminal sizes are independent by design; the terminal keeps `font_size_px` and its Ctrl+=/Ctrl+- zoom.
- Agent-agnostic, client-only (no daemon/protocol change); reuse gpui-component theming, no new dependency.

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 54 row: a UI font-size setting driving the editor/explorer/chrome (today's slider only mutates the terminal PTY grid), persisted via the phase-9 window-state store; separate UI size from the terminal grid size.
- [Category 1: GPUI Applications & Components](prior-art.md#category-1-gpui-applications--components) — gpui-component Theme (`font_size` / `mono_font_size` tokens) and rift's own `set_ui_font_persisted` pattern (the theme-mutation + persist path this reuses); Zed `SettingsStore` as the hierarchical-settings reference.

## Human prerequisites

- none

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| One global UI font size driving the theme base `font_size` + `mono_font_size`; no per-surface sizes | Cascades to editor/explorer/chrome via the theme; per-surface is premature (constitution) | 2026-07-29 |
| The terminal PTY grid stays a SEPARATE size control, not folded into the UI size | The grid is Nerd-Font-pinned and cell-size-measured; the settings code already scopes this out | 2026-07-29 |
| Relabel the existing "Font size" to name the terminal; the misleading "editor and terminal" copy is corrected | It only mutates the terminal grid today; the label is factually wrong | 2026-07-29 |
| Persist the UI font size in `WindowState` as a new field, restored on launch | Mirrors the existing terminal `font_size_px` / font-family persistence | 2026-07-29 |
| The UI font-size setter defines its own min/max bounds; it does not borrow the terminal-scoped `MIN/MAX_FONT_SIZE` | Those bounds are for the terminal grid; the UI needs its own sensible range | 2026-07-29 |
| OPEN — Ctrl+= / Ctrl+- scope: keep them zooming the TERMINAL font only (status quo) vs make them context-aware (zoom the UI when a non-terminal surface is focused, the terminal when a terminal pane is focused) | resolved at the spec-acceptance gate | — |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged (one per implementable step)

## Verification

- [ ] `just ci` equivalent green for `app` (fmt + clippy `-D warnings` + tests); the crate compiles warm-target clean.
- [ ] QA: changing "UI font size" live-resizes the editor text, dock panels, explorer rows, and chrome; the terminal grid is unchanged.
- [ ] QA: changing the (relabelled) terminal font size resizes only the terminal grid; the UI is unchanged.
- [ ] QA: both sizes survive an app restart.
- [ ] QA: the relabelled terminal control no longer claims to affect the editor.
- [ ] QA (per the Ctrl+= decision): the zoom keys adjust the surface agreed at the gate.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Mutating the theme's base `font_size` at runtime does not re-layout all surfaces | Follow the `set_ui_font_persisted` family-mutation path (which already re-renders on font change); verify explorer/editor/chrome all re-layout in QA |
| `text_sm`/`text_xs` are fixed steps that do not scale with the base `font_size` | Confirm in the implementing PR that gpui-component's `text_sm`/`text_xs` derive from the theme base; if any surface uses a hardcoded `px(...)`, note it as a follow-up rather than expanding scope |
| Users confuse the two size controls | The relabel + distinct "UI font size" / terminal labels; descriptions state exactly which surfaces each affects |

## Decision log

- 2026-07-29: Scoped from a develop-code read — the current "Font size" mutates only the terminal grid; a UI font size must drive the theme base `font_size` + `mono_font_size` (new setter, no existing size plumbing) and persist in `WindowState`, kept separate from the terminal grid size. The only open point is the Ctrl+= / Ctrl+- zoom scope.
