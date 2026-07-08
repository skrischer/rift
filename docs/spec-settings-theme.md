# Spec: Settings shell + theme unification

> Status: READY
> Created: 2026-07-08
> Completed: —

Bring settings and theming to the Paper "Settings" artboard: a full
sidebar-nav settings shell (replacing today's cramped modal), a terminal
palette driven by the **active theme** instead of a second hardcoded palette,
and a systematic sweep replacing every remaining non-token color across the
app with `gpui-component` theme tokens. Phase 26 of the v1.0 polish cut
([roadmap.md](roadmap.md)) — it closes the standing hardcoded-terminal-palette
tech debt.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not
activities.

- [ ] The settings surface matches the Paper "Settings" artboard anatomy: a
      left sidebar (title, "Search settings…" input, page nav) beside a
      grouped content page, rendered near-full-window — not the current
      720×480 single-page dialog. Built on `gpui-component`'s multi-page
      `setting::Settings` widget (its `Sidebar` + search + `pages()`), never a
      hand-rolled shell.
- [ ] The **Appearance** page is fully wired to real client state: a
      **Theme** group rendering one selectable card per theme registered in
      the `ThemeRegistry` (preview swatch derived from that theme's own
      tokens, active card marked), a **Font & size** group (UI font, editor/
      terminal mono font, font-size stepper). Every control reads and writes
      the same live state the command palette and `Ctrl+=/Ctrl+-` already
      mutate, and persists via the window-state store (no config file).
- [ ] The **terminal ANSI palette follows the active theme**: the 16 named/
      indexed<16 colors plus default foreground/background resolve from
      `cx.theme()` tokens (base red/green/blue/yellow/magenta/cyan + their
      `_light` brights, `foreground`, `background`, `border`, `muted`), not
      the hardcoded `PALETTE`/`FOREGROUND`/`BACKGROUND` constants. Switching
      the theme at runtime restyles the terminal grid live, with no restart.
- [ ] The xterm 6×6×6 color-cube and grayscale ramp (indices ≥ 16) stay
      exact xterm RGB — those are a standard, not a theme palette — and remain
      covered by the existing `colors.rs` tests.
- [ ] A **systematic hex→token sweep** leaves no product-path rendering color
      that is not a theme token: the terminal palette (above), stray
      `white()`/literal colors, and any surface not reading `ActiveTheme` are
      converted; a lightweight regression guard fails CI if a raw color
      literal reappears in a rendering path. (The editor-renders-light
      symptom is the *tactical* fix in issue #598 — see coordination note; this
      phase owns the *systematic* token unification, not that single fix.)
- [ ] Agent-agnostic and local: settings are pure client UI state; no
      telemetry, no agent detection, no per-agent settings section, no remote
      or file-based config layer introduced.

## Scope

### In scope

- **Settings shell** (`crates/app/src/settings.rs`, `workspace.rs` host):
  expand the current single-page dialog into the Paper multi-section shell
  using `gpui-component`'s `setting::Settings` in multi-page mode — the
  vendored `Sidebar` (title + `Input` search with the `Search` icon prefix +
  `SidebarMenu` of pages) and the active-page content column it already
  renders. Host it near-full-window (large `Root` dialog or a dedicated
  full-size surface), replacing the fixed 720×480 modal. The `OpenSettings`
  action, its `ctrl-,`/`cmd-,` keybindings, and the activity-rail/title-bar
  entry points stay as they are.
- **Appearance page** (`crates/app`): the theme-card group (one card per
  registered theme, preview swatch composed from that theme's tokens, radio/
  active selection driving `set_theme_persisted`) and the font/size group (UI
  font + editor/terminal mono font dropdowns, font-size number field) — the
  three-plus preferences that exist as client state today, laid out to the
  artboard's group/item rhythm.
- **Theme-driven terminal palette** (`crates/terminal/src/colors.rs`,
  `pane_view.rs`): replace the hardcoded `PALETTE`/`FOREGROUND`/`BACKGROUND`
  with a small `TerminalPalette` value built from a `gpui_component` theme
  (`ThemeColor`), mapping the 16 ANSI slots + default fg/bg to tokens (mapping
  table in Prior decisions). `pane_view.rs` builds the palette from
  `cx.theme()` once per render and resolves each cell's fg/bg through it; the
  cube/grayscale/indexed-≥16 logic is unchanged. Tests build a palette from a
  fixed `ThemeColor` so the existing cube/ramp assertions still hold.
- **Systematic hex→token sweep** (`crates/app`, `crates/terminal`): audit
  every rendering path for colors not derived from `cx.theme()`; convert them
  to the matching token (e.g. the terminal window-tab "!" badge's `white()` →
  `danger_foreground`). Add a regression guard — a workspace test or a small
  CI grep step — that fails if a raw color constructor (`rgb(`/`rgba(`/
  `hsla(`/`Rgba {`/`Hsla {` with literals/`white()`/`black()`) appears in a
  product rendering path, with a narrow allowlist for the genuinely
  non-themeable (documented xterm cube constants, test fixtures).

### Out of scope

- **The editor-renders-light bug itself** — issue #598 is the tactical fix
  (wire the editor container + gutter/line-number/selection to dark theme
  tokens). This phase does not duplicate it; it owns the workspace-wide token
  unification and the regression guard that keeps #598 (and every other
  surface) from regressing. If #598 has not merged when the sweep runs, the
  sweep subsumes its editor-token wiring; otherwise it verifies it.
- **New feature toggles the artboard shows but that have no backing state** —
  minimap on/off, render-whitespace, font ligatures. Minimap is always-on
  today, whitespace/ligature rendering do not exist; wiring a toggle means
  building the feature and a new persisted preference. Each is its own future
  phase. This phase does not ship dead controls ("no dead icons", per
  spec-cockpit-chrome.md).
- **Nav sections without real settings** — an "Agents" settings section is
  flatly excluded (agent-specific config violates the agent-agnostic
  constitution rule); Connection/Keybindings/Editor/Terminal/General/About
  sections are shell structure only, populated in later phases as real state
  appears. This phase populates **Appearance**; the shell supports the rest
  without committing their content.
- **Custom theme authoring / import** ("Custom…" card) — deferred at Phase 17
  and still deferred; the card set is data-driven from the registry
  (Light / Dark / Catppuccin Mocha in v1), not the artboard's hardcoded
  Catppuccin family.
- **A user-editable config-file layer** (zed-style `settings.json`,
  hierarchical `SettingsStore`) — flatly out per the standing "knobs are env
  vars, no new config layer" decision (`spec-dogfooding-channels.md`, Phase 9,
  Phase 17). Preferences persist as **state** in the window-state store, not a
  file. Deployment knobs stay env vars.
- **Extending `gpui-component`'s theme schema with an explicit 16-color ANSI
  block** — not needed; the six `base` tokens + their `_light` variants + fg/
  bg/border/muted cover the 16 slots (mapping in Prior decisions). Forking the
  vendored theme structs is forbidden (constitution).
- **The terminal PTY grid font stays the pinned JetBrainsMono Nerd Font
  Mono** — its icon glyphs and cell-metric measurement depend on it; the
  Appearance mono-font control governs the editor and GUI panes only, not the
  terminal grid.

## Human prerequisites

None. Client-side UI + theme mapping; no new dependency (`gpui-component`'s
theme system and `setting::Settings` widget are already vendored and in use),
no protocol change, no secrets. The Paper "Settings" artboard (node `1SO-0` in
the `rift` design file) is the visual contract, verified at the milestone QA
gate.

## Constraints

- **Honor "no new config layer"** (`spec-dogfooding-channels.md`, Phase 9,
  Phase 17): every persisted preference is **state** in the versioned,
  per-channel, atomic window-state store (the Phase 9 schema extends
  forward-compatibly), never a user-editable file. Deployment knobs remain env
  vars.
- **Reuse the vendored widgets, never fork them** (constitution): the shell is
  `gpui-component`'s multi-page `setting::Settings` (`Sidebar` + search +
  `SettingPage`/`SettingGroup`/`SettingItem` + the ready `dropdown`/`bool`/
  `number` field types); theming stays `ThemeRegistry` + `Theme::change`; the
  terminal palette reads `ThemeColor` — no new theme structs, no hand-rolled
  form or sidebar.
- **Theme tokens only, Catppuccin Mocha default** (constitution): no hardcoded
  hex in any product rendering path; the sweep's regression guard enforces it.
  The terminal default theme stays Catppuccin Mocha (rift's registered
  default).
- **Live restyle, no restart**: `pane_view.rs` reads the palette from
  `cx.theme()` each render, so `Theme::change` (which already forces a
  repaint) restyles the terminal grid for free — same live-restyle guarantee
  as the rest of the UI.
- **Agent-agnostic, no telemetry** (constitution): settings are local client
  state derived from nothing agent-specific; no analytics, no agent detection,
  no per-agent section.
- **Crate boundaries** (constitution): the `TerminalPalette` type and its
  theme-mapping live inside `crates/terminal` (already a `gpui-component`
  dependent) and are exposed through `lib.rs` if used across the crate; no new
  crate dependency, no leak of app types into `terminal`.
- **No `.unwrap()` in library code**; no `todo!()`; a token the mapping cannot
  find degrades to a sensible neighbor (fg/bg/border), never a panic or a
  blank cell.
- **Backwards-compatible store load**: a fresh/corrupt/older window-state
  store falls back to the current defaults (Catppuccin Mocha, current font
  scale) without crashing — the tolerant-load discipline Phase 9/17 set.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-26 row of the v1.0 polish
index anchors this spec.

- **`gpui-component` `setting::Settings` (multi-page) — reuse** (already
  vendored, pinned `9ad30e6`): the widget already renders exactly the Paper
  shell — a `Sidebar` with a search `Input` (`IconName::Search` prefix) and a
  `SidebarMenu` of pages, beside the active `SettingPage` (`filtered_pages`
  implements the search). rift uses it single-page in a small dialog today
  (`crates/app/src/settings.rs`); Phase 26 uses its multi-page mode at full
  size. No new UI code.
- **`gpui-component` `ThemeColor` base tokens — the terminal palette source**:
  `ThemeColor` exposes `red/green/blue/yellow/magenta/cyan` + each `_light`
  variant, plus `foreground/background/border/muted_foreground` — enough to
  map all 16 ANSI slots + default fg/bg. rift's Catppuccin Mocha JSON already
  defines the `base.*` keys (matching today's palette almost exactly; the one
  intentional shift is ANSI magenta → theme mauve `#cba6f7`). No schema
  extension, no fork.
- **`zed` terminal theme (`ThemeColors.terminal_ansi_*`) — reference, not
  adopted**: zed carries an explicit 16-slot ANSI block in its theme. rift
  deliberately maps onto `gpui-component`'s existing base tokens instead of
  adding a parallel block, to avoid forking the vendored theme structs (the
  same don't-rebuild-primitives call Phase 17 made).
- rift-local grounding: `crates/terminal/src/colors.rs` is today's hardcoded
  palette (the tech debt this phase resolves); `crates/terminal/src/
  pane_view.rs` consumes it (`colors::BACKGROUND`, `colors::to_gpui_color`);
  `crates/app/src/settings.rs` is the Phase 17 single-page dialog this phase
  grows; issue #598 is the tactical editor-dark fix this phase's systematic
  sweep backstops.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **The terminal palette is derived from `gpui_component::ThemeColor`, not a parallel 16-slot ANSI theme block** | The six `base` tokens + `_light` variants + fg/bg/border/muted cover all 16 slots; adding an ANSI block would fork the vendored theme structs (forbidden). Live restyle comes free because `pane_view` already renders with `cx`. | 2026-07-08 |
| **ANSI→token mapping**: 0 black→`border`, 1 red→`red`, 2 green→`green`, 3 yellow→`yellow`, 4 blue→`blue`, 5 magenta→`magenta`, 6 cyan→`cyan`, 7 white→`muted_foreground`, 8 bright-black→`border` blended toward `foreground` (or `muted`), 9–14 bright→the matching `*_light`, 15 bright-white→`foreground`; default fg→`foreground`, default bg→`background` | Matches today's Catppuccin values closely (border `#45475a` = today's ANSI black; base red/green/yellow/blue/cyan are exact); the neutral slots (black/white/bright-black/bright-white) map to structural tokens so every theme yields a coherent 16-color set. Magenta intentionally becomes the theme's mauve. | 2026-07-08 |
| **The xterm 6×6×6 cube and grayscale ramp (indices ≥ 16) stay exact xterm RGB, not theme-derived** | Those indices are a terminal standard applications rely on, not a palette; only the 16 named/indexed<16 slots + default fg/bg are themed. The existing cube/ramp tests remain valid unchanged. | 2026-07-08 |
| **Settings persist as client state (window-state store), not a config file** | Standing project decision ("knobs are env vars, no new config layer"), reaffirmed at Phase 9 and Phase 17; theme/font are state like window bounds. Not re-opened here. | 2026-07-08 |
| **Theme cards are data-driven from the `ThemeRegistry` (v1: Light / Dark / Catppuccin Mocha); "Custom…" is out of scope** | The registry is the source of truth for available themes; custom authoring was deferred at Phase 17 and stays deferred. The card preview swatch is composed from each theme's own tokens (theme-driven, not hardcoded). | 2026-07-08 |
| **The settings shell is the multi-page `setting::Settings` widget at full size; the Appearance page is the only one populated this phase** | Don't-rebuild-primitives: the widget already ships the sidebar + search + page layout. Other nav sections are shell structure with no invented content; feature toggles without backing state (minimap/whitespace/ligatures) and an agent-specific "Agents" section are out of scope. | 2026-07-08 |
| **#598 (editor renders light) is the tactical fix; this phase owns the systematic token pass + regression guard** | A single-surface bug fix and a workspace-wide unification are different in kind; #598 may land independently. The sweep converts every remaining non-token color and adds a guard so no surface (editor included) regresses to a hardcoded color. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
step under the Phase 26 milestone. Created once this spec is `READY` and merged
to `develop`.

- Milestone: created at `READY` (Phase 260 — Settings shell + theme unification)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] The terminal grid renders from the active theme: switching theme at
      runtime (palette command or settings card) restyles the terminal live,
      with no restart; `colors.rs` no longer holds a hardcoded 16-color
      palette or fg/bg constants
- [ ] The existing `colors.rs` cube/grayscale/indexed tests still pass against
      a palette built from a fixed `ThemeColor`; a new test asserts the 16
      named slots + default fg/bg resolve to the mapped theme tokens
- [ ] The settings surface renders the Paper shell: sidebar (title + search +
      page nav) beside the Appearance page; theme cards select the theme and
      persist across restart; the font/size controls read and write the live
      client state
- [ ] The hex→token regression guard is in place and green: a `grep`/test
      confirms no raw color constructor survives in a product rendering path
      outside the documented allowlist (xterm cube constants, test fixtures)
- [ ] `grep` confirms no telemetry, no network I/O, no agent detection, and no
      new user-editable config-file layer introduced
- [ ] Milestone QA (dev channel, Paper "Settings" artboard `1SO-0`): the
      settings shell matches the artboard anatomy; switching each theme
      restyles the whole UI **and** the terminal grid coherently; the editor
      renders dark (no light-surface regression); a restart keeps the choice

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `gpui-component`'s `ThemeColor` lacks explicit black/white/bright-black/bright-white ANSI slots | Map the four neutral slots to structural tokens (`border`, `muted_foreground`, `foreground`, a `border`→`foreground` blend) — documented in the mapping decision; verified against today's Catppuccin values (border `#45475a` = today's ANSI black). |
| A theme's base tokens read poorly as a terminal palette (low contrast in some registered theme) | v1 ships three vetted themes (Light/Dark/Catppuccin Mocha), each QA'd at the milestone gate; the mapping favors the theme's own accents so contrast tracks the theme's design. Custom themes are out of scope, so no unvetted palette ships. |
| The sweep's regression guard is over-broad and flags legitimate non-themeable colors | A narrow, documented allowlist (xterm cube constants in `colors.rs`, `#[cfg(test)]` fixtures, the badge-contrast `danger_foreground` case); the guard targets product rendering paths only. |
| Settings shell scope-creeps into wiring every artboard toggle | The out-of-scope list is explicit: only Appearance is populated; toggles without backing state and the agent section are excluded, not deferred silently. Not re-opened at the gate. |
| Overlap with the in-flight #598 editor fix | Sequenced: the sweep runs after the palette + shell land; if #598 merged first the sweep verifies its editor tokens, else it subsumes them. Either way the guard is the durable backstop. |
| PR size | Three issues, each ~150–300 lines: (1) theme-driven terminal palette, (2) settings shell + Appearance page, (3) the systematic sweep + guard (depends on 1 and 2). |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 26, v1.0 polish
  cut). Grounded on `gpui-component`'s multi-page `setting::Settings` (already
  vendored, the Paper "Settings" shell verbatim), its `ThemeColor` base tokens
  (the terminal palette source — no schema fork), the standing "no config
  layer" decision (preferences persist as state), and the hardcoded
  `crates/terminal/src/colors.rs` palette (the tech debt this phase resolves).
  Constraint/precedent-determined: reuse the vendored widgets; map the 16 ANSI
  slots onto existing base tokens; keep xterm cube/ramp exact; #598 is the
  tactical editor fix, this phase the systematic pass; only the Appearance page
  is populated. No genuinely-open decisions surfaced (the config-layer
  exclusion and the theme-schema-fork exclusion are settled precedent, not
  re-opened; feature toggles without backing state are flatly out).
- 2026-07-08: Narrowed the Appearance mono-font control to the editor and GUI
  panes; it does not drive the terminal PTY grid. `rift_terminal`'s
  `session_view`/`pane_view` pin "JetBrainsMono Nerd Font Mono" because the
  terminal's icon glyphs and cell-metric measurement depend on that specific
  font — swapping it live is a structural, resize-math-sensitive change this
  spec never scoped. Ratified by the planner, sanctioning PR #634 (issue
  #608).
