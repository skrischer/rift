# Spec: gpui-component adoption

> Status: COMPLETED
> Created: 2026-06-01
> Completed: 2026-06-05

Adopt `longbridge/gpui-component` as rift's UI primitive layer and migrate the Phase 2d chrome (tab bar, statusbar, theme) onto it, instead of hand-rolling further UI.

## Outcome

- [x] `gpui-component` is a workspace dependency, building cleanly alongside `gpui` and `termy_terminal_ui` on a single shared GPUI git revision
- [x] The app renders inside gpui-component's `Root`/theme context; a theme is applied app-wide (#33 / PR #34)
- [x] The window tab bar is rendered with gpui-component's tab/dock component, replacing the hand-rolled tab bar in `session_view.rs`, with no regression in window switching (click) behavior (#27 / PR #35)
- [x] The statusbar is rebuilt on gpui-component primitives, ready to host the Phase 2d data displays (git branch, command, session/window name, connection status) (#28 / PR #37)
- [x] `cargo deny check licenses` passes (gpui-component is Apache-2.0)

## Scope

### In scope

- Add `gpui-component` (and `gpui-component-assets` if required) as a git dependency, pinned alongside a single GPUI revision
- Converge `gpui`, `termy_terminal_ui`, and `gpui-component` on one GPUI git rev (the central compatibility work)
- Wire gpui-component `Root` + `Theme` at the app root
- Migrate the window tab bar to gpui-component
- Rebuild the statusbar container using gpui-component primitives (the data wiring for individual fields stays in the Phase 2d issues)

### Out of scope

- File explorer / `VirtualList` (Phase 3)
- Dock splits / resizable panels for the terminal grid (Phase 3 — terminal layout stays driven by tmux for now)
- Code editor, LSP, diff views (later phases)
- Replacing the terminal widget itself — `termy_terminal_ui` stays as the terminal renderer

## Constraints

- `gpui` and `gpui_platform` are git dependencies from `zed-industries/zed`, **bare-tracked** (no `rev` in any `Cargo.toml`); the committed `Cargo.lock` pins the exact commit. zed extracted `gpui_platform` out of `gpui` on 2026-02-19, so the app constructs the platform via `gpui_platform::current_platform(false)` and `Application::with_platform(...)` (post-split API).
- `termy_terminal_ui` is a git dependency from the rift-owned fork `skrischer/termy`, with its `gpui` dependency bare-tracked too. terminal_ui is self-contained (only `gpui`, `alacritty_terminal`, `flume`, `anyhow`, `dirs`, `polling`) and compiles unchanged against current post-split gpui — the fork delta is a one-line "bare-ize the gpui pin".
- `gpui-component` floats its `gpui` dependency (it dropped its rev lock on 2025-12-18) and is consumed **natively — no fork**. It commits a `Cargo.lock` pinning the zed rev it was validated against; that rev is the convergence target.
- Cargo cannot link two incompatible `gpui` versions — all three crates MUST resolve to one GPUI rev. Convergence is achieved by everyone sharing the **same bare git reference** to zed (so cargo unifies to one source) with the committed `Cargo.lock` pinning the commit. Bump deliberately via `cargo update -p gpui --precise <rev>`; never float to `HEAD`.
- License: gpui-component is Apache-2.0 (compatible with rift's GPL-3.0). Must pass `cargo deny check licenses`.
- Minimal-dependency policy: adopting gpui-component is justified because it replaces hand-rolled tab/dock/list/theme primitives rift would otherwise maintain (see prior-art.md).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Adopt gpui-component as the UI primitive layer | Top-priority dependency in prior-art.md; ships dock, virtualized list/table, theme, scrollbar — avoids rebuilding primitives | 2026-06-01 |
| Adopt now and migrate 2d UI (not defer to Phase 3) | User decision; prevents accumulating more custom UI code | 2026-06-01 |
| Keep `termy_terminal_ui` as terminal renderer | Production-grade, already integrated; gpui-component does not replace the terminal widget | 2026-06-01 |
| GPUI-rev compatibility is a hard gate before migration | Two GPUI versions cannot interoperate; convergence must be proven first | 2026-06-01 |

## Tracking

Step decomposition lives as GitHub issues under the milestone.

- Milestone: [gpui-component adoption](https://github.com/skrischer/rift/milestone/2)
- Issues: created from the task outline below

Provisional step outline (becomes issues, not kept here once created):
1. GPUI-rev compatibility spike — prove `gpui` + `termy_terminal_ui` + `gpui-component` build on one shared rev. **Gate PASSED (2026-06-02):** full `cargo build --workspace`, one `gpui` entry, 0 errors. See decision log.
2. Add gpui-component dependency + wire `Root`/`Theme` at app root
3. Migrate window tab bar to gpui-component, preserving click-to-switch
4. Rebuild statusbar container on gpui-component primitives

## Verification

- [x] `cargo build --workspace` succeeds with exactly one `gpui` entry in `Cargo.lock`
- [x] `cargo clippy --workspace -- -D warnings` passes
- [x] `cargo test --workspace` passes
- [x] `cargo deny check licenses` passes
- [x] Tab bar renders via gpui-component; clicking a tab switches windows (no regression vs current behavior)
- [x] App renders inside gpui-component theme context; statusbar visible and themed (theme context via #34; statusbar rebuilt on gpui-component primitives via #28)
- [x] No second `gpui` version pulled in transitively (verified in `Cargo.lock`)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gpui-component pins a GPUI rev incompatible with rift's `83de8a25e0` and/or termy's rev | RESOLVED (2026-06-02): gpui-component does not pin gpui (floats since 2025-12-18). Convergence achieved by bare-tracking zed everywhere + `Cargo.lock` pinning gpui-component's own validated rev (`4bee412`). No gpui-component fork needed; termy fork delta is one line. |
| GPUI pre-1.0 churn breaks the build later | Pin exact git revs; bump deliberately, never floating |
| Scope creep into Dock splits / file explorer | Explicitly out of scope; those are Phase 3 with their own specs |
| Tab bar migration regresses window switching | Verification requires click-to-switch parity before close |

## Decision log

- 2026-06-01: Spec created. GPUI-rev convergence identified as the make-or-break constraint; compatibility spike mandated as step 1.
- 2026-06-02: Compatibility spike PASSED. Full `cargo build --workspace` green — `gpui` + `gpui_platform` + `termy_terminal_ui` + `gpui-component` + all rift crates on one zed rev, exactly one `gpui` entry in `Cargo.lock`, 0 errors / 0 warnings.
  - **Chosen rev:** zed `4bee412118dafea3bbd491cd044d354f16b3d665` — sourced from gpui-component HEAD's committed `Cargo.lock` (the rev they validated against). gpui-component consumed at `9ad30e6` (HEAD).
  - **Convergence architecture:** all consumers bare-track `zed-industries/zed` (no `rev` in `Cargo.toml`); the committed `Cargo.lock` pins the commit. Same bare git reference -> cargo unifies to one source. Bump via `cargo update -p gpui --precise <rev>`.
  - **No gpui-component fork:** gpui-component floats gpui by design (rev lock removed upstream 2025-12-18). Earlier spike attempts to pin it (path-patch / re-add rev lock in a fork) were workarounds and were discarded.
  - **termy fork delta is one line:** terminal_ui is self-contained and compiles unchanged against post-split gpui; the only change is bare-izing its `gpui` pin. termy stays otherwise upstream-clean.
  - **rift's own post-split adaptation is one line:** `Application::new()` -> `Application::with_platform(gpui_platform::current_platform(false))`, plus a `gpui_platform` dependency. The terminal UI (`rift-terminal`: pane_view, session_view) compiled unchanged.
  - **R-discovery is deterministic:** read gpui-component's committed `Cargo.lock` for the zed rev to target on every future bump.
  - **Ergonomics caveat:** adding a new dependency from the zed source re-floats the bare resolution to `HEAD`; re-apply `cargo update -p gpui --precise <rev>` after such changes. Rare once `Cargo.lock` is committed.
  - **Strategic reframe:** the real future constraint is termy lagging gpui, not "finding a shared rev". rift owns the termy fork and brings it current on rift's cadence; gpui-component then floats in cleanly.
- 2026-06-04: Step 3 (window tab bar) implemented (#27 / PR #35).
  - **Tab variant:** gpui-component `TabBar` Default variant — closest to the prior hand-rolled look, lowest regression risk.
  - **Tab bar visibility:** always rendered, even with a single window (was: shown only with >1 window). Gives a constant layout with no height jump when a second window opens.
  - **Click-to-switch:** preserved via index -> window-id mapping; `TabBar::on_click` sends `select-window -t {id}` over `tmux_command_tx`, unchanged from before.
- 2026-06-04: Outcome "a theme is applied app-wide" completed (#33 / PR #34). #26 wired `Root`/`init` structurally but never applied a palette, so gpui-component widgets defaulted to the built-in light theme (surfaced as black-on-white tabs in #27).
  - **Fix:** register a Catppuccin Mocha theme in gpui-component's native `ThemeRegistry` (JSON asset, `crates/app/assets/themes/catppuccin-mocha.json`) alongside the built-in Light/Dark, then activate it at startup via `Theme::change`.
  - **Single theme by design:** a selectable multi-theme system + runtime switcher is explicitly out of scope here and deferred to its own future spec. The registry/JSON approach keeps that extension rework-free (the switcher only needs to enumerate `ThemeRegistry::themes()`).
  - **Source-of-truth note:** `crates/terminal/src/colors.rs` stays the terminal ANSI palette (cell colors); the JSON defines the UI-chrome `ThemeColor` tokens. They overlap only on a few base colors.
- 2026-06-05: Step 4 (statusbar container) implemented (#28 / PR #37) — completes the spec.
  - **Rebuild on `h_flex()` + `cx.theme()`:** the hand-rolled `div()` statusbar with hardcoded Catppuccin `Hsla` locals is replaced by a `gpui_component::h_flex().justify_between()` container reading the active theme via the `ActiveTheme` trait — same shape as `TitleBar`, no dedicated `StatusBar` component exists.
  - **1:1 token mapping (no visual regression):** `colors::SURFACE0` -> `cx.theme().tab_bar` (bg, #313244), `colors::SUBTEXT0` -> `cx.theme().muted_foreground` (text, #a6adc8), `colors::SURFACE1` -> `cx.theme().border` (top border, #45475a). Visually confirmed identical on the GPU station.
  - **Slots established, data deferred:** left group (connection/session/window) and right group (command/git) are marked as the homes for the Phase 2d fields (#18-#21); only the container is built here, the field wiring stays in each 2d issue.
  - **Root frame + pane-split border themed too:** root container bg `colors::BACKGROUND` -> `cx.theme().background`; the `render_layout` pane-split border (also `colors::SURFACE1`) was switched to `cx.theme().border` in the same PR — `render_layout` now threads `border_color: Hsla` from `cx.theme().border` at the call site.
  - **`colors.rs` reduced to the ANSI palette:** `SURFACE0`, `SUBTEXT0`, `SURFACE1` removed (dead after migration); the `use crate::colors;` import dropped from `session_view.rs`. `colors.rs` now holds only `FOREGROUND`/`BACKGROUND` + the ANSI `PALETTE`/`to_gpui_color` (still used by `pane_view.rs`).
