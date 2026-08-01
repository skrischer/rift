# Spec: Independent Explorer/Editor visibility

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
- **The persisted enum is a semantic rename, not additive.** The tolerant deserializer (`window_state.rs:166-175`) silently drops unknown variants, so a naive rename degrades an upgrading user to neither panel. Migration must expand `"explorer_editor"` → both areas; `Visibility::from_persisted` (`workspace.rs:416`) is the injection point. `SCHEMA_VERSION` (`window_state.rs:24`) — decide whether the semantic rename warrants a bump or is handled by the expand-alias alone.
- `RailState` uses per-area boolean fields (not a set), so each new area is a new struct field, and `render` takes per-area `on_toggle` closures — the split adds one field + one closure, not a data-driven row.
- Agent-agnostic, client-only; no protocol/daemon change; reuse the existing rail/dock/visibility machinery.

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 56 row: split the combined `Area::ExplorerEditor` into two independently-toggled areas with a filled-region icon per target; references VS Code Activity Bar view-containers (independent view toggles) and rift's own `Area` enum + activity rail; reverses the Phase-39 unification (a design-contract update in the spec PR, not a foundation change).
- rift's own Phase 39 (`docs/archive/spec-workspace-visibility-rail.md`) — the unification being reversed; the source of the fused `apply_center_visibility` and the rail `Area` model.

## Human prerequisites

- If the gate elects a custom "filled-region" split icon, a design asset is authored in the project's Paper design file (`docs/design.md`) and referenced from this spec before the rail issue starts; if the gate elects two distinct existing `IconName`s, no asset is needed.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Split into two areas `Explorer` + `Editor`; both default visible (in `Area::ALL`) | Matches today's all-visible default; the split only decouples their toggles | 2026-08-01 |
| Explorer routes to the left dock, Editor to the center editor half; the center decision becomes 3-input `(explorer, editor, terminal)` | That is where the two panels actually live; preserves the Terminal-fill / focus / resize invariants of `apply_center_visibility` | 2026-08-01 |
| Legacy persisted `"explorer_editor"` expands to `{Explorer, Editor}` on load (and solo likewise), not dropped | The current tolerant-drop would leave an upgrading user with neither panel; expand preserves their intent | 2026-08-01 |
| Split `SoloExplorerEditor` into `SoloExplorer` (FileTree) + `SoloEditor` (EditorView); add a `ToggleEditor` action + command entry | The two panels dispatch from different views; a per-area solo/toggle is the point of the split | 2026-08-01 |
| Client-only; no foundation-doc change; Phase 39's spec is left as the historical record | The reversal is an app-internal design-contract update the roadmap already sanctions | 2026-08-01 |
| **OPEN — resolved at the spec-acceptance gate:** the rail representation — two distinct standard icons (e.g. `PanelLeft` for Explorer + a document/file icon for Editor) vs. a single custom "filled-region" split icon (the roadmap seed phrasing) that fills the Explorer or Editor half to show the target | Two distinct icons need no new asset and read clearly per area; the filled-region icon matches the roadmap seed but is a new Paper design asset. Neither constraint nor prior art settles the UX choice | 2026-08-01 |

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
