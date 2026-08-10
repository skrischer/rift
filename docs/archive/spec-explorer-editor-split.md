# Spec: Independent Explorer/Editor visibility

> Status: COMPLETED (2026-08-10)
> Created: 2026-08-01

Split the combined `Area::ExplorerEditor` rail item into two independently
toggleable areas — Explorer (left dock) and Editor (center) — reversing Phase
39's deliberate unification. Roadmap Phase 56.

## Outcome

- [ ] `Area::ExplorerEditor` is split into `Area::Explorer` and `Area::Editor`; each toggles independently from the activity rail (two rail items) and via its own action / command-palette entry.
- [ ] Toggling Explorer opens/closes only the left dock; toggling Editor shows/hides only the center editor tab — neither affects the other, and the Terminal-fill / focus-rehome / grid-resize invariants are preserved.
- [ ] Each area's rail item shows its own visible/hidden state; a distinct icon per area indicates the target (resolved at the spec-acceptance gate).
- [ ] Solo works per area (Explorer-solo and Editor-solo are distinct); focus re-homes correctly when either is hidden.
- [ ] An older persisted window-state carrying `"explorer_editor"` loads as **both** Explorer and Editor visible (and solo likewise) — not silently dropped to neither.

## Scope

### In scope

- Replace `Area::ExplorerEditor` (`workspace.rs:363`) with `Area::Explorer` + `Area::Editor`; update `Area::ALL` (`:374`) and every match site (the ~20 listed: `apply_area_visibility` dispatch `:1696`, `apply_center_visibility` `:1747`, `focused_area` `:1981`, `rehome_focus_if_hidden` `:2035`, `preferred_focus_area` `:2058`, rail wiring `:2614`/`:2623`, action handlers `:2704`/`:2725`).
- Split the fused dock application in `apply_center_visibility` (`workspace.rs:1746-1775`) and the mirrored initial construction (`:1275-1332`): the left dock opens on the Explorer flag; the center `h_split` editor half shows on the Editor flag. The current 2-input `(explorer_editor, terminal)` center decision becomes a 3-input `(explorer, editor, terminal)` decision — Explorer routes to the left dock, Editor + Terminal to the center reconciler.
- Activity rail: split `RailState.explorer_editor_visible` (`activity_rail.rs:64`) into `explorer_visible` + `editor_visible`; add the fifth rail button + `on_toggle` closure (`render` signature `:255`, button block `:263`, call site `workspace.rs:2610`); a distinct icon per area (gate-resolved).
- Actions: keep `ToggleExplorer`; add `ToggleEditor` (struct + `on_action` handler + command-registry + command-palette entries, mirroring `ToggleExplorer` at `workspace.rs:126`/`command_registry.rs`/`command_palette.rs:252`). Split `SoloExplorerEditor` into `SoloExplorer` (dispatched by `FileTree`) + `SoloEditor` (dispatched by `EditorView`) with their handlers (`workspace.rs:2724`).
- Persistence migration: map a legacy persisted `"explorer_editor"` → `{Explorer, Editor}` on load (and `solo_area: "explorer_editor"` likewise), instead of the current tolerant-drop of the unknown variant (`window_state.rs:166-186`); a regression test asserts the expand (alongside `window_state.rs:884`).

### Out of scope

- Any change to what the Explorer or Editor panels contain or how they render internally — this is purely splitting their visibility toggle.
- New docking regions or moving the Explorer/Editor out of their current homes (left dock / center) — they stay where they are; only the toggle is decoupled.
- A settings surface for the split; the areas are toggled from the rail / actions / palette as the other areas are.
- Foundation-doc changes — this is an app-internal UI design-contract update authored in this spec PR (it supersedes Phase 39's rail unification; roadmap Phase 56 line), not a vision/constitution/architecture edit.

## Constraints

- **Phase 39 deliberately unified them** (`docs/archive/spec-workspace-visibility-rail.md:33-34,199-202`) *because* they live in two different dock regions (left dock vs center); its decision log (`:136`) records "today they are separate … this phase unifies their toggle." This spec reverses that, sanctioned by the roadmap (Phase 56 "supersedes the Phase-39 rail's Explorer+Editor unification — a design-contract update authored in Phase 56's spec PR"). The reversal is recorded in this spec's decision log and Phase 39's spec is left as the historical record.
- **The dock split is the hard part.** `apply_center_visibility` (`workspace.rs:1746`) fuses the left dock and the center editor half under one flag; the initial construction (`:1275`) mirrors it. Splitting must keep the "hidden = not rendered, entity kept alive" contract, the Terminal-symmetry (editor hidden → terminal fills), the focus re-home, and the grid-resize-on-reshow invariants that thread through the same function.
- **The persisted enum is a semantic rename, not additive.** The tolerant deserializer (`window_state.rs:166-175`) silently drops unknown variants, so a naive rename degrades an upgrading user to neither panel. Migration must expand `"explorer_editor"` → both areas, and it must happen at the `serde_json::Value` layer **inside** `deserialize_tolerant_areas` / `deserialize_tolerant_solo_area` (`window_state.rs:166-186`) — NOT in `Visibility::from_persisted` (`workspace.rs:416`), which receives an already-typed `&[Area]` after the deserializer has dropped the legacy token, and which cannot yield *two* areas from one value anyway (a per-variant `#[serde(alias)]` maps one string to one variant, so it also cannot expand). `SCHEMA_VERSION` (`window_state.rs:24`) does NOT need a bump: the Value-layer expand migrates old files regardless of version.
- `RailState` uses per-area boolean fields (not a set), so each new area is a new struct field, and `render` takes per-area `on_toggle` closures — the split adds one field + one closure, not a data-driven row.
- Agent-agnostic, client-only; no protocol/daemon change; reuse the existing rail/dock/visibility machinery.

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 56 row: split the combined `Area::ExplorerEditor` into two independently-toggled areas with a filled-region icon per target; references VS Code Activity Bar view-containers (independent view toggles) and rift's own `Area` enum + activity rail; reverses the Phase-39 unification (a design-contract update in the spec PR, not a foundation change).
- rift's own Phase 39 (`docs/archive/spec-workspace-visibility-rail.md`) — the unification being reversed; the source of the fused `apply_center_visibility` and the rail `Area` model.

## Human prerequisites

- [ ] **A custom filled-region split-panel icon asset** (the gate elected this path): two states of a panel glyph at the current `PanelLeft` 30:70 proportion — one with the narrow left/sidebar region filled (Explorer), one with the wide main region filled (Editor). Authored in the project's Paper design file (`docs/design.md`) and exported as local SVG(s) under `file_icons/` (the existing `Icon::empty().path(...)` mechanism, e.g. `activity_rail.rs:287`), referenced from the rail wiring issue. **Pending** — the rail issue is blocked until this asset is delivered (it carries `blocked:human` until then).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Split into two areas `Explorer` + `Editor`; both default visible (in `Area::ALL`) | Matches today's all-visible default; the split only decouples their toggles | 2026-08-01 |
| Explorer routes to the left dock, Editor to the center editor half; the center decision becomes 3-input `(explorer, editor, terminal)` | That is where the two panels actually live; preserves the Terminal-fill / focus / resize invariants of `apply_center_visibility` | 2026-08-01 |
| Legacy persisted `"explorer_editor"` expands to `{Explorer, Editor}` on load (and solo likewise), not dropped | The current tolerant-drop would leave an upgrading user with neither panel; expand preserves their intent | 2026-08-01 |
| Split `SoloExplorerEditor` into `SoloExplorer` (FileTree) + `SoloEditor` (EditorView); add a `ToggleEditor` action + command entry | The two panels dispatch from different views; a per-area solo/toggle is the point of the split | 2026-08-01 |
| Client-only; no foundation-doc change; Phase 39's spec is left as the historical record | The reversal is an app-internal design-contract update the roadmap already sanctions | 2026-08-01 |
| The rail uses a **custom filled-region split icon** — one panel glyph whose region is filled to show the target, at a **30:70 proportion matching the current `PanelLeft` icon** (a narrow left sidebar region + a wide main region), NOT a 50:50 split. The Explorer item fills the narrow left/sidebar region; the Editor item fills the wide main region | Accepted at the gate. Matches the roadmap seed ("filled-region icon indicating the target") and stays visually consistent with today's `PanelLeft` panel layout; the 30:70 proportion reads as "sidebar vs main," which maps naturally to Explorer (left dock) vs Editor (center) | 2026-08-01 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged — the `Area` split + dock reroute (`apply_center_visibility` + initial construction), the rail + actions + command-palette wiring (with the gate-chosen icon), and the persistence migration + regression test. Dependency edges in the issue bodies (rail/actions depend on the `Area` split; migration is independent but shares the enum change).

## Verification

- [ ] `just ci` equivalent green for `app` (fmt + clippy `-D warnings` + tests); the crate compiles warm-target clean.
- [ ] Unit test: legacy `visible_areas: ["explorer_editor"]` (and `solo_area: "explorer_editor"`) deserializes to both `Explorer` and `Editor` (not dropped), alongside the existing unknown-variant-dropped test.
- [ ] Unit test: toggling `Explorer` changes only the left-dock open state; toggling `Editor` changes only the center editor half; solo/​focus-rehome behave per area.
- [ ] QA: the rail shows two items; hiding Explorer keeps the Editor + Terminal; hiding Editor lets the Terminal fill the center; hiding both leaves the empty-tabs/terminal state as today.
- [ ] QA: focus re-homes sensibly when either area is hidden; the chosen icon clearly indicates each target.
- [ ] QA: an upgrade from an older window-state (Explorer+Editor visible) opens with both visible.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The dock split breaks the Terminal-fill / focus / grid-resize invariants in `apply_center_visibility` | Preserve the function's existing contract; unit-test the 3-input center decision table exhaustively; QA all visibility combinations |
| The persistence migration silently drops `"explorer_editor"` (current behavior) | Explicit expand-on-load in `from_persisted` with a regression test; do not rely on the tolerant-drop path |
| Reversing a deliberate Phase-39 decision reintroduces the desync it avoided | The desync risk was "toggling as one must not desync individual panel state" — splitting removes the coupling entirely, so each panel owns its own state; QA independent toggles |
| Rail icon ambiguity (which item is which) | Resolved at the gate; whichever representation, QA that each item's target is unmistakable |

## Decision log

- 2026-08-01: Scoped from `workspace.rs` / `activity_rail.rs` / `window_state.rs` (Explore agent). The `Area::ExplorerEditor` fusion lives in `apply_center_visibility` (`:1746`), which drives the left dock AND the center editor half from one flag — Phase 39 unified them precisely because they occupy two dock regions. Splitting reroutes the two flags and turns the center decision 3-input; the persisted `"explorer_editor"` must expand to both on load rather than being tolerant-dropped. The single open item — the rail icon representation (two distinct icons vs. a custom filled-region split icon) — carried to the gate.
- 2026-08-01: Spec-acceptance gate (PR #938) — accepted after one review round. Review returned REQUEST_CHANGES on a single blocking finding: the migration injection point was mis-stated as `Visibility::from_persisted` (which receives an already-typed `&[Area]` after the tolerant deserializer has dropped the legacy token, and cannot yield two areas from one value); corrected to the `serde_json::Value` layer inside `deserialize_tolerant_areas`/`deserialize_tolerant_solo_area` (matching the Scope + decision log), and `SCHEMA_VERSION` baked as no-bump (the Value-layer expand migrates regardless). All other claims verified (the fused `apply_center_visibility`, the silent tolerant-drop, per-area `RailState` fields, dual-view `SoloExplorerEditor`) and the Phase-39 reversal confirmed roadmap-sanctioned. Gate decision: a **custom filled-region split-panel icon at the current `PanelLeft` 30:70 proportion** (Explorer fills the narrow sidebar region, Editor the wide main region) — a Paper design asset that is a pending human prerequisite blocking the rail issue.
- 2026-08-10: Icon asset (issue #941 human prerequisite) — the two filled-region glyphs were **hand-authored as SVG** (`file_icons/panel-explorer.svg`, `file_icons/panel-editor.svg`), not drawn in Paper, at the developer's explicit direction: the target is a stroke glyph that must sit pixel-consistent beside the built-in `IconName::PanelLeft` (lucide `panel-left`), which is more exactly met by copying that geometry directly than by a design-tool export. Each SVG keeps `panel-left`'s exact outer `rect 3,3 18x18 rx2` + divider `M9 3v18` (the 30:70 proportion, verified: left region 6px, right 12px) at the same `stroke-width="2"`, adding one `fill="currentColor"` region path — the narrow left region for Explorer, the wide right region for Editor (arc sweep flags checked to yield convex rounded outer corners). `currentColor` lets the rail's existing tint machinery (`area_hue`, both areas → `theme().blue`) color them per state, so no theme token is hardcoded in the asset.
- 2026-08-10: Rail wiring (issue #941) — the Editor rail item sits **between Explorer and Terminal** (the split pair stays adjacent); `render` gains a fifth `on_toggle_editor` closure (7 args total, at clippy's threshold, no `too_many_arguments`). `ToggleEditor` is registered right after `ToggleExplorer` in `command_registry` (palette order), and `SoloExplorerEditor` is replaced by dedicated `SoloExplorer` (dispatched by `FileTree`) + `SoloEditor` (dispatched by `EditorView`) — each panel now solos itself, resolving the interim where `EditorView`'s solo button soloed the Explorer. The dock split (`apply_center_visibility` 3-input `(explorer, editor, terminal)`) and the persistence expand were already landed by #939/#985, so this issue was purely the rail item + action + solo split + the icon asset.
