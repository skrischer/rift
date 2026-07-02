# Spec: Phase 17 — Theme & settings

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

Runtime theme selection (light / dark / system + `gpui-component`'s named themes) replacing today's hardcoded dark, plus a thin settings surface for the handful of client UI preferences — all persisted as **client state** (extending the window-state store), honoring the project's standing "knobs are env vars, no new config layer" decision. The final phase of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The user can **switch theme at runtime**: light / dark (and, best-effort, follow-system), and pick a named theme from `gpui-component`'s `ThemeRegistry`; the whole UI restyles live (it already reads `ActiveTheme`), replacing the hardcoded dark set at startup (`crates/app/src/lib.rs`).
- [ ] The theme choice (mode + named theme) and font scale **persist across restarts** — stored as client state in the window-state store (Phase 9's versioned, forward-compatible schema, extended here for these preferences), per dogfooding channel; a fresh/corrupt store falls back to the current default (dark).
- [ ] Theme switching is reachable from the **command palette** (Phase 16) — "Toggle light/dark", "Select theme…" commands — and/or a small settings surface; no separate config file to edit.
- [ ] A **thin settings surface** exposes the client UI preferences that exist (theme, font scale) in one place; there is **no user-editable settings-file layer** (deployment knobs stay env vars, per the standing decision).
- [ ] Agent-agnostic and local: theming/preferences are pure client UI state; no telemetry, no agent detection, no remote settings.

## Scope

### In scope

- **Runtime theme selection** (`crates/app`): use `gpui-component`'s `ThemeRegistry` (global; holds `default_themes` + named `themes`) to switch the active theme live — assigning the chosen `ThemeConfig` to `Theme::global_mut(cx).light_theme`/`dark_theme` then calling `Theme::change(mode, …, cx)` (exactly the shape `crates/app/src/lib.rs`'s existing `apply_theme` uses). Mode is light/dark; "follow system" is best-effort (see the system-mode note). Replaces the startup hardcode. The selectable set is small: `gpui-component` bundles Light + Dark, and rift already loads its own Catppuccin Mocha (`load_themes_from_str`) — so ~3 themes in v1 (custom theme authoring is out of scope).
- **Persistence as client state**: extend the window-state store (Phase 9 — versioned, per-channel, atomic) with the theme mode + named-theme name (font scale already lives there); tolerant load with the dark default on missing/corrupt/unknown.
- **Command-palette integration** (Phase 16): register theme commands ("Toggle light/dark", "Select theme…") in the command registry so they are discoverable/dispatchable.
- **A minimal settings surface built on `gpui-component`'s `setting::Settings`** (`SettingPage`/`SettingGroup`/`SettingItem` + the ready-made `Dropdown`/`Switch`/`Number` field types): a small settings panel/modal exposing the client UI preferences (theme-mode dropdown, named-theme dropdown, font-scale field) with live controls — the *view* over the same state, reusing the vendored settings widget rather than hand-rolling a form. It writes to the state store, not a config file.

### Out of scope

- **A user-editable settings-file layer** (zed-style `settings.json`, hierarchical `SettingsStore`) — **flatly out of scope**, per the project's standing decision "knobs are env vars, no new config layer" (`spec-dogfooding-channels.md`, reaffirmed by `spec-window-state-persistence.md`). This spec does not re-litigate that decision; v1 persists preferences as **state**, not a config file. (Note: `gpui-component`'s `setting::Settings` *widget* is reused for the settings **view** — see In scope — which is a UI surface over state, not a config-file layer.)
- **Deployment/config knobs** (host, port, session, SSH key) — these stay environment variables with working defaults, unchanged.
- **Per-project / per-worktree settings**, workspace-scoped overrides — post-v1.0.0.
- **Keybinding customization / a keymap editor** — not in v1; the command palette (Phase 16) is the discoverability surface.
- **Custom user theme authoring / importing theme files** — v1 uses `gpui-component`'s bundled themes; authoring is a later refinement.
- **Telemetry / analytics settings** — there is no telemetry to configure (constitution).

## Human prerequisites

None. Client-side theming + preference state; no new dependency (`gpui-component`'s theme system is already vendored and in use), no protocol change, no secrets.

## Constraints

- **Honor "no new config layer"** (`spec-dogfooding-channels.md`, `spec-window-state-persistence.md`): preferences persist as **state** in the window-state store (versioned schema, per-channel, atomic writes), not as a user-editable config file. Deployment knobs remain env vars.
- **Reuse `gpui-component`'s theme system AND its settings widget** (constitution: don't rebuild primitives): `ThemeRegistry` + `Theme::change` for theming (rift already imports `Theme`/`ThemeMode`/`ThemeRegistry` in `crates/app/src/lib.rs` and every surface reads `ActiveTheme` — switching is assigning the active `ThemeConfig` then calling `Theme::change`), and `setting::Settings` (`SettingPage`/`SettingGroup`/`SettingItem` + `Dropdown`/`Switch`/`Number` fields) for the settings form — neither is new UI code.
- **Extends the Phase 9 window-state store**: theme mode + named theme are new fields in that versioned, forward-compatible schema (Phase 9 designed it to extend without migration). **All three dependency phases are still unimplemented** (`READY` specs, no code): Phase 9 (the store), Phase 10 (the shell hosting the settings surface), Phase 16 (the command registry the theme commands register into). Milestone depends on Phase 9, Phase 10, **and Phase 16**. Where a prerequisite has not landed, its slice is a shared prerequisite carried by the dependent issue (the store's versioned pattern; the registry entry), not duplicated — issue ordering across the three milestones is explicit via `Depends on`.
- **Live restyle, no restart**: `Theme::change` restyles the running app (surfaces read `ActiveTheme`); no relaunch to apply a theme.
- **Tolerant load, safe default**: an unknown named theme or a corrupt store falls back to the dark default (today's behavior); never a crash, never a blank theme.
- **Agent-agnostic, no telemetry** (constitution): preferences are local client state; no analytics, no agent detection.
- **No `.unwrap()` in library code**; no `todo!()`; a persisted theme that no longer exists in the registry degrades to the default.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-17 index row anchors this spec.

- **`gpui-component` Theme / `ThemeRegistry` + `setting::Settings` — reuse** (already vendored): `ThemeRegistry` (global, `default_themes` + named `themes`), `Theme` (global `mode`/`light_theme`/`dark_theme`), `Theme::change(mode, …, cx)` — rift already sets a dark theme this way at startup (`crates/app/src/lib.rs`), so Phase 17 makes it runtime-selectable + persisted. The bundled `ThemeSet` ships only Light + Dark; rift already loads its own Catppuccin Mocha via `load_themes_from_str` — a ~3-theme v1 picker. The settings **view** reuses `gpui-component`'s `setting::Settings` (`SettingPage`/`SettingGroup`/`SettingItem` + `fields/{dropdown,bool,…}`), which `prior-art.md` lists as reusable chrome — a near-exact fit for a theme-mode/named-theme/font-scale form.
- **`zed` `crates/settings` — reference, deliberately NOT adopted wholesale** (GPL-3.0): the hierarchical `SettingsStore` is the archetype of the config-file layer the project's standing decision rejects; studied for what *not* to build in v1. rift persists preferences as state instead.
- rift-local grounding: `crates/app/src/lib.rs` hardcodes dark (`Theme::change(ThemeMode::Dark, None, cx)`); `spec-window-state-persistence.md` (Phase 9) built the versioned per-channel state store this extends (font scale already persisted there); the "no new config layer" decision is in `spec-dogfooding-channels.md`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Preferences persist as client state (window-state store), not a config file** | Standing project decision ("knobs are env vars, no new config layer", `spec-dogfooding-channels.md`, reaffirmed by Phase 9); theme/font are state like window bounds, and Phase 9's versioned schema extends forward-compatibly to hold them. | 2026-07-02 |
| **Reuse `gpui-component`'s `ThemeRegistry` + `Theme::change` (theming) and `setting::Settings` (settings form); no new UI code** | Constitution: don't rebuild primitives; rift already themes via `ActiveTheme`, and `setting::Settings` ships the dropdown/switch/number fields the settings surface needs. | 2026-07-02 |
| **Runtime theme = mode (light/dark) + a named theme (~3: bundled Light/Dark + rift's Catppuccin Mocha)** | Live restyle by assigning the `ThemeConfig` then `Theme::change`, no restart. "Follow system" is best-effort only (see below). | 2026-07-02 |
| **"Follow system" mode is best-effort, not a committed persisted mode** | `gpui-component`'s `ThemeMode` has no `System` variant; "follow system" is a one-shot `sync_system_appearance` from `window.appearance()`, not a live-tracked persisted mode. v1 ships explicit light/dark reliably; system-follow is a best-effort startup convenience, dropped if the appearance API is unavailable (noted at QA). | 2026-07-02 |
| **Theme switching surfaced via the command palette (Phase 16) + a minimal settings view** | Reuses the discoverability surface just built; the settings view is a *view* over the state, not a config system. Depends on Phase 16's command registry existing. | 2026-07-02 |
| **Extends the Phase 9 window-state store (depends on Phase 9 + Phase 10 + Phase 16)** | The store is the persistence mechanism; Phase 9's versioned schema extends forward-compatibly. Phase 10 provides the shell/settings host; Phase 16 provides the command registry the theme commands register into. | 2026-07-02 |
| **No user-editable settings-file layer; preferences persist as state** | The standing "no new config layer" decision (`spec-dogfooding-channels.md`, reaffirmed by Phase 9) governs and is not re-opened here. The settings *view* reuses `gpui-component`'s `setting::Settings` widget but writes to the state store, not a config file. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 17 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 170 — Theme & settings)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] Switching mode (light/dark/system) and named theme restyles the running UI live, with no restart; the startup hardcode is gone
- [ ] The theme choice + font scale persist across a restart (per dogfooding channel); a missing/corrupt/unknown-theme store falls back to the dark default without crashing
- [ ] Theme commands appear in the command palette and switch the theme; the minimal settings surface reflects and changes the same preferences
- [ ] Store round-trip tests: the extended schema (theme mode + named theme + font) serializes/deserializes; an unknown theme name loads as the default; unknown/future fields tolerate
- [ ] `grep` confirms no telemetry, no network I/O, no agent detection, and no new user-editable config-file layer introduced
- [ ] Milestone QA (dev channel): switch themes from the palette and settings surface; restart and confirm the choice stuck; stable and dev channels keep independent theme state

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Depends on **three unimplemented phases** (9 store, 10 shell/settings host, 16 command registry) | Each is a shared prerequisite, not duplicated: the theme-persistence issue carries the minimal store slice if Phase 9 hasn't landed; the settings-surface issue needs the Phase 10 shell; the palette-command issue needs Phase 16's registry. Milestone `Depends on: #19, #24, #30` and per-issue `Depends on` make the ordering explicit; runtime theme switching (issue 1) is decoupled from the palette (issue) so it is not gated on Phase 16's timeline. |
| "Follow system" mode may be infeasible (`ThemeMode` has no `System` variant) | Best-effort only: ship explicit light/dark reliably; wire `sync_system_appearance` from `window.appearance()` as a startup convenience if available, else drop system-follow (noted at QA) — not a committed persisted mode. |
| A persisted named theme no longer exists in the registry after a `gpui-component` bump | Tolerant load: unknown theme → dark default; a QA/round-trip test covers it. |
| "Settings" scope-creeps into a full config layer | The standing no-config-layer decision governs; the flat out-of-scope exclusion makes it explicit, not a drift (not re-opened at the gate). |
| PR size | Small phase; decompose: (1) runtime theme selection (registry + assign `ThemeConfig` + `Theme::change`, replace the hardcode) — **independent of Phase 16**; (2) persist theme/font in the (extended) window-state store; (3) the minimal settings surface on `setting::Settings`; (4) palette theme commands (into Phase 16's registry — split out so #1 is not gated on Phase 16). ~150-200-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Review gate (fresh-context Agent review) — `REQUEST_CHANGES`, three blocking findings addressed. (1) The config-file-layer was wrongly framed as an `OPEN` gate item despite the standing "no new config layer" decision being cited three times in the same spec as settled — **moved to a flat out-of-scope/constraint exclusion**, not gated (re-opening a merged architectural decision at every adjacent spec is exactly the risk the standing decision closed off). (2) The settings surface now **reuses `gpui-component`'s `setting::Settings`** widget (`SettingPage`/`SettingGroup`/`SettingItem` + dropdown/switch/number fields) instead of a hand-rolled form (don't-rebuild-primitives). (3) The dependency coverage now names **all three** unimplemented prerequisites (Phase 9 store, Phase 10 shell, Phase 16 registry) with mitigations and explicit ordering; Phase 16 added to milestone depends-on; runtime theme switching decoupled from the palette issue. Non-blocking folded in: softened the Phase-9 "reserved for exactly such preferences" citation; right-sized the theme set (~3: bundled Light/Dark + rift's Catppuccin Mocha); "follow-system" demoted to a best-effort non-committed mode (`ThemeMode` has no `System` variant); aligned the `Theme::change` mechanism wording. Result: **no genuinely-open decisions** remain — the gate is acceptance + human-prerequisites (none) only.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 17, the last v1.0.0 cockpit phase). Grounded on `gpui-component`'s `ThemeRegistry`/`Theme::change` (already used to hardcode dark in `crates/app/src/lib.rs`), Phase 9's versioned window-state store (the persistence mechanism), and the standing "no new config layer" decision. Constraint/precedent-determined: preferences persist as state (not a config file); reuse the vendored theme system + settings widget; runtime mode + named theme with live restyle; surface via the palette + a minimal settings view; extends the Phase 9 store (depends on Phase 9 + Phase 10 + Phase 16).
