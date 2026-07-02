# Spec: Phase 17 — Theme & settings

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

Runtime theme selection (light / dark / system + `gpui-component`'s named themes) replacing today's hardcoded dark, plus a thin settings surface for the handful of client UI preferences — all persisted as **client state** (extending the window-state store), honoring the project's standing "knobs are env vars, no new config layer" decision. The final phase of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The user can **switch theme at runtime**: light / dark / follow-system, and pick a named theme from `gpui-component`'s `ThemeRegistry`; the whole UI restyles live (it already reads `ActiveTheme`), replacing the hardcoded dark set at startup (`crates/app/src/lib.rs`).
- [ ] The theme choice (mode + named theme) and font scale **persist across restarts** — stored as client state in the window-state store (the versioned schema Phase 9 reserved for exactly such preferences), per dogfooding channel; a fresh/corrupt store falls back to the current default (dark).
- [ ] Theme switching is reachable from the **command palette** (Phase 16) — "Toggle light/dark", "Select theme…" commands — and/or a small settings surface; no separate config file to edit.
- [ ] A **thin settings surface** exposes the client UI preferences that exist (theme, font scale) in one place; there is **no user-editable settings-file layer** (deployment knobs stay env vars, per the standing decision).
- [ ] Agent-agnostic and local: theming/preferences are pure client UI state; no telemetry, no agent detection, no remote settings.

## Scope

### In scope

- **Runtime theme selection** (`crates/app`): use `gpui-component`'s `ThemeRegistry` (global; holds `default_themes` + named `themes` from the bundled `ThemeSet`) and `Theme::change(mode, …, cx)` to switch mode (light/dark/system) and named theme live; replace the startup hardcode in `crates/app/src/lib.rs`.
- **Persistence as client state**: extend the window-state store (Phase 9 — versioned, per-channel, atomic) with the theme mode + named-theme name (font scale already lives there); tolerant load with the dark default on missing/corrupt/unknown.
- **Command-palette integration** (Phase 16): register theme commands ("Toggle light/dark", "Select theme…") in the command registry so they are discoverable/dispatchable.
- **A minimal settings surface**: a small panel or modal listing the client UI preferences (theme mode, named theme, font scale) with live controls — the *view* over the same state, not a new config system.

### Out of scope

- **A user-editable settings-file layer** (zed-style `settings.json`, hierarchical `SettingsStore`) *(OPEN — resolved at the spec-acceptance gate; recommended: not in v1)*: the project's standing decision is "knobs are env vars, no new config layer" (`spec-dogfooding-channels.md`, reaffirmed by `spec-window-state-persistence.md`). v1 persists preferences as **state**, not a config file.
- **Deployment/config knobs** (host, port, session, SSH key) — these stay environment variables with working defaults, unchanged.
- **Per-project / per-worktree settings**, workspace-scoped overrides — post-v1.0.0.
- **Keybinding customization / a keymap editor** — not in v1; the command palette (Phase 16) is the discoverability surface.
- **Custom user theme authoring / importing theme files** — v1 uses `gpui-component`'s bundled themes; authoring is a later refinement.
- **Telemetry / analytics settings** — there is no telemetry to configure (constitution).

## Human prerequisites

None. Client-side theming + preference state; no new dependency (`gpui-component`'s theme system is already vendored and in use), no protocol change, no secrets.

## Constraints

- **Honor "no new config layer"** (`spec-dogfooding-channels.md`, `spec-window-state-persistence.md`): preferences persist as **state** in the window-state store (versioned schema, per-channel, atomic writes), not as a user-editable config file. Deployment knobs remain env vars.
- **Reuse `gpui-component`'s theme system** (constitution: don't rebuild primitives): `ThemeRegistry` + `Theme::change` + the bundled `ThemeSet`; rift already imports `Theme`/`ThemeMode`/`ThemeRegistry` (`crates/app/src/lib.rs`) and every surface reads `ActiveTheme` — switching is swapping the active `ThemeConfig` and calling `Theme::change`, not new theming code.
- **Extends the Phase 9 window-state store**: theme mode + named theme are new fields in that versioned schema (Phase 9 explicitly reserved the schema for future preferences). Milestone depends on Phase 9 (the store) and Phase 10 (the shell / settings surface). If Phase 9's store is not yet in code when this lands, the store work is a shared prerequisite, not duplicated.
- **Live restyle, no restart**: `Theme::change` restyles the running app (surfaces read `ActiveTheme`); no relaunch to apply a theme.
- **Tolerant load, safe default**: an unknown named theme or a corrupt store falls back to the dark default (today's behavior); never a crash, never a blank theme.
- **Agent-agnostic, no telemetry** (constitution): preferences are local client state; no analytics, no agent detection.
- **No `.unwrap()` in library code**; no `todo!()`; a persisted theme that no longer exists in the registry degrades to the default.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-17 index row anchors this spec.

- **`gpui-component` Theme / `ThemeRegistry` — reuse** (already vendored, already used): `ThemeRegistry` (global, `default_themes` + named `themes` from a bundled `ThemeSet`), `Theme` (global `mode`/`light_theme`/`dark_theme`), `Theme::change(mode, …, cx)`. rift already sets a dark theme this way at startup (`crates/app/src/lib.rs`); Phase 17 makes it runtime-selectable + persisted.
- **`zed` `crates/settings` — reference, deliberately NOT adopted wholesale** (GPL-3.0): the hierarchical `SettingsStore` is the archetype of the config-file layer the project's standing decision rejects; studied for what *not* to build in v1. rift persists preferences as state instead.
- rift-local grounding: `crates/app/src/lib.rs` hardcodes dark (`Theme::change(ThemeMode::Dark, None, cx)`); `spec-window-state-persistence.md` (Phase 9) built the versioned per-channel state store this extends (font scale already persisted there); the "no new config layer" decision is in `spec-dogfooding-channels.md`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Preferences persist as client state (window-state store), not a config file** | Standing project decision ("knobs are env vars, no new config layer", `spec-dogfooding-channels.md`, reaffirmed by Phase 9); theme/font are state like window bounds, and Phase 9's schema was reserved for exactly this. | 2026-07-02 |
| **Reuse `gpui-component`'s `ThemeRegistry` + `Theme::change`; no new theming code** | Constitution: don't rebuild primitives; rift already themes via `ActiveTheme` and sets the theme this way at startup. | 2026-07-02 |
| **Runtime theme = mode (light/dark/system) + a named theme from the bundled `ThemeSet`** | The registry ships named themes; exposing them is free. Live restyle via `Theme::change`, no restart. | 2026-07-02 |
| **Theme switching surfaced via the command palette (Phase 16) + a minimal settings view** | Reuses the discoverability surface just built; the settings view is a *view* over the state, not a config system. | 2026-07-02 |
| **Extends the Phase 9 window-state store (depends on Phase 9 + Phase 10)** | The store is the persistence mechanism; Phase 9 reserved its schema for future preferences. Phase 10 provides the shell/palette to host the switcher. | 2026-07-02 |
| **A user-editable settings-file layer** | **OPEN — resolved at the spec-acceptance gate.** Recommended: not in v1 (honor the standing no-config-layer decision; persist as state). In scope only if the developer chooses to override that decision and add a config file. | OPEN |

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
| Depends on the Phase 9 window-state store, which may not be implemented yet | The store is a shared prerequisite; if Phase 9 hasn't landed, the theme-persistence issue carries the minimal store work (same versioned pattern), not a parallel store. Milestone depends-on makes the ordering explicit. |
| A persisted named theme no longer exists in the registry after a `gpui-component` bump | Tolerant load: unknown theme → dark default; a QA/round-trip test covers it. |
| "Settings" scope-creeps into a full config layer | The standing no-config-layer decision governs; the OPEN gate item makes the scope an explicit choice, not a drift. |
| System-follow mode needs OS appearance detection | If `gpui`/`gpui-component` exposes system appearance, follow it; otherwise ship light/dark explicit and treat "system" as a later refinement (note if dropped). |
| PR size | Small phase; decompose: (1) runtime theme selection (registry + `Theme::change`, replace the hardcode) + palette commands; (2) persist theme/font in the (extended) window-state store; (3) the minimal settings surface. ~200-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 17, the last v1.0.0 cockpit phase). Grounded on `gpui-component`'s `ThemeRegistry`/`Theme::change` (already used to hardcode dark in `crates/app/src/lib.rs`), Phase 9's versioned window-state store (the persistence mechanism, reserved for future preferences), and the standing "no new config layer" decision. Constraint/precedent-determined: preferences persist as state (not a config file); reuse the vendored theme system; runtime mode + named theme with live restyle; surface via the palette + a minimal settings view; extends the Phase 9 store (depends on Phase 9 + Phase 10). One genuinely-open item carried to the gate: whether a user-editable settings-file layer is in v1 (recommended: no — honor the standing decision).
