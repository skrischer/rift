# Spec: editor chrome

> Status: READY
> Created: 2026-07-05
> Completed: —

Bring the editor surface to the Paper design: breadcrumb with the enclosing
symbol, gutter severity dots, the inline diagnostic card, hover-card anatomy
with an action row, a real references panel, an outline panel, the
file-changed-on-disk dialog — and (gate decision) a minimap strip.

## Outcome

- [ ] A 30px breadcrumb renders under the editor tabs: mono path segments
      (`crates › terminal › src › session_view.rs`) plus the ENCLOSING SYMBOL
      at the cursor (`› fn render`), live from a new document-symbol stream.
- [ ] Error/warning lines carry a severity dot in the gutter left of the line
      number; the primary diagnostic under the cursor line renders as the
      design's inline card (bg popover token, border, radius 8: severity
      glyph + message + muted source/code) anchored under the line.
- [ ] The hover card matches §3 anatomy: code block (signature + truncated
      preview), hairline, doc body, action row `› Definition F12` ·
      `Q References ⇧F12` (Rename is consciously OMITTED — LSP rename is
      post-v1; no dead controls).
- [ ] References AND multi-target definitions open in a right-dock panel per
      §3 (header names the mode — "References"/"Definitions" — search-context
      chip, "N results · M files", file groups with count badges, match rows
      with the symbol highlighted, active row accent) — replacing the
      jump-list overlay for BOTH of its consumers (#485 — still open — adds
      an interim dismiss; this panel removes the overlay entirely).
- [ ] An OUTLINE panel (left dock, §3 anatomy: kind glyph lanes + mono names,
      selection follows cursor, click jumps) renders from document symbols.
- [ ] The dirty-buffer conflict surfaces as the design's dialog ("File
      changed on disk", body copy, secondary "Keep mine" + primary "Reload
      from disk") — upgrading the #433 banner; same two actions.
- [ ] A minimap strip (~14px) renders on the editor's right edge: line-length
      marks from the shaped-line cache, diagnostic tints, viewport slab,
      click-to-jump — damage-only redraw, explicitly NOT a pixel code render
      (gate decision 2026-07-06: build).
- [ ] All colors/typography via theme tokens; no dead controls.

## Scope

### In scope

- `protocol`/`daemon`/`lsp` (deliberate, minimal API change): a
  `DocumentSymbolRequest { id, path }` → `DocumentSymbolResponse` pair
  (flattened symbol list: name, kind, range, selection_range, depth) via the
  existing nav-request machinery with drop-stale ids and PER-CONNECTION
  replies — the DocumentSymbol issue carries `Depends on: #482` (open
  papercut: today nav responses broadcast to all clients; the new pair must
  not inherit that). The lsp crate advertises
  `hierarchical_document_symbol_support` at initialize; the Flat
  (`SymbolInformation`) response shape is normalized too (fallbacks:
  selection_range = range, depth from container nesting). Serves breadcrumb
  AND outline. Version bump per the fingerprint policy.
- `app` (editor.rs): breadcrumb bar (path from the open tab, symbol =
  innermost symbol containing the cursor, updated on cursor move against the
  cached symbol tree — one request per open/change, not per keystroke);
  gutter severity dots as an app-side overlay aligned via the input widget's
  visible_row_range/line_height APIs (the pinned widget has no gutter
  decoration API); inline diagnostic card for the cursor line — the widget's
  own mouse-hover DiagnosticPopover is suppressed for that line so one
  diagnostic never renders twice; current-line highlight if not already
  present.
- `app`: hover card restyle to §3 (code block + doc + action row wired to
  the existing GoToDefinition / FindReferences actions with kbd hints).
  Anchoring/dismissal mechanics stay owned by the open papercut #486
  (mouse-position anchor, no re-open after dismiss) — the restyle issue
  carries `Depends on: #486` to avoid double implementation.
- `app`: references panel in the right dock (gpui-component Panel like
  SourceControl/DiffView), fed by the existing ReferencesResponse; grouped by
  file, count badges, click jumps, Escape/× closes; the overlay path is
  removed.
- `app`: outline panel in the left dock (toggle via palette + the phase-21
  rail gains its icon in a follow-up there), fed by the document-symbol
  cache of the active editor.
- `app`: conflict dialog via gpui-component modal (the #420 dirty-close
  confirm pattern), actions identical to the #433 banner remedies.
- Minimap (gate decision: IN): a marks strip (~14px: line-length marks,
  diagnostic tints, viewport slab, click-to-jump) as its own issue — marks
  from the existing shaped-line cache, damage-only redraw, no pixel-perfect
  code render.

### Out of scope

- LSP rename, code actions, formatting, completion (post-v1 per the roadmap).
- Problems-panel redesign (its wave-1 gaps ride the papercut track).
- Editor-tab strip anatomy (already close to design; dirty dot exists).
- Search panel.

## Constraints

- Symbol requests ride the existing nav seam (request id correlation,
  drop-stale, per-connection reply); never per-keystroke — request on open
  and on buffer-change debounce, resolve breadcrumb/outline selection
  client-side from the cached tree.
- The references panel and outline panel are dock panels (gpui-component),
  not floating overlays; both must work with dock resize/toggle.
- Theme tokens only; mono for code/paths/line numbers, Inter for UI labels;
  severity colors from §0.
- Constitution: no `.unwrap()` in libs; agent-agnostic (symbols come from
  LSP, an agent-independent signal); crate boundaries.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| One flattened DocumentSymbol pair serves breadcrumb + outline | Two consumers, one stream, one cache; mirrors how diagnostics fan out client-side | 2026-07-05 |
| Rename is omitted from the hover action row | LSP rename is explicitly post-v1 (roadmap); a dead Rename button violates the no-dead-controls bar — conscious deviation from the §3 artboard, revisited with the rename feature | 2026-07-05 |
| The results panel takes over BOTH overlay consumers (references AND multi-target definitions, #198 path) | One mechanism per surface; leaving definitions on a removed overlay would orphan them (spec-review finding) | 2026-07-05 |
| Conflict UI = modal dialog on the #420 confirm pattern with the #433 actions | Design §7 shows a dialog; the banner was the papercut-scale interim | 2026-07-05 |
| Symbol resolution is client-side against a cached tree | One request per open/change-debounce keeps the seam cheap; cursor moves are local | 2026-07-05 |

## Prior art

- `docs/prior-art.md` → Phases 19–26 index, Phase 23 row: `zed`
  `crates/editor` (hover popover, breadcrumbs), `crates/outline_panel`;
  gpui-component code-editor story (reference anatomy only).

## Human prerequisites

None.

## Tracking

- Milestone: created after this spec merges (phase 23) — `Depends on
  milestone: none` (dock panels exist; rail icon is a phase-21 follow-up,
  not a blocker).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Protocol tests for the DocumentSymbol pair (valid + malformed)
- [ ] Behavioral: breadcrumb symbol tracks the cursor through nested fns/
      impls without per-keystroke requests (verified via daemon log cadence)
- [ ] Behavioral: an agent-introduced error shows the gutter dot + inline
      card; fixing it clears both within the diagnostics cadence
- [ ] Hover action row jumps (Definition) and opens the references panel
      (References) with correct grouping/counts; Escape closes the panel
- [ ] Outline: click jumps, selection follows the cursor
- [ ] Conflict dialog: agent edit vs dirty buffer → dialog; both actions
      behave exactly like the #433 banner did
- [ ] Visual match vs the Editor — LSP Navigation artboard at the QA gate

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Symbol trees on huge files inflate the response | Flattened list with a depth cap + daemon-side truncation guard (documented limit), matching the diagnostics-payload discipline |
| rust-analyzer's hierarchical vs flat symbol responses differ | The lsp crate normalizes both DocumentSymbol shapes into the flattened protocol form; tested with fixtures of both |
| Minimap (if IN) becomes a perf sink on large buffers | Marks derived from the already-shaped line cache, redrawn on damage only; explicitly NOT a pixel-perfect code render |

## Decision log

- 2026-07-05: Spec drafted from the wave-1 editor gap analysis (breadcrumb/
  minimap/gutter dots/inline card/references panel/outline all CONFIRMED
  missing; hover card partial) and the design distillation §1/§3/§7.
- 2026-07-05: Fresh-context review (PR #524): blocking findings baked in —
  the panel takes over multi-target definitions too (#198 overlay consumer),
  the stale "#485 gave it a dismiss path" claim corrected (open, interim),
  #482 becomes an issue-level prerequisite of the DocumentSymbol issue, and
  hover anchoring/dismissal stays owned by #486 (dependency edge).
  Non-blocking adoptions: #420 (not #423) as the dialog pattern, diagnostic
  double-surface suppression, capability advertisement + Flat-shape
  fallbacks, gutter overlay render strategy.
- 2026-07-07 (#530): the outline panel is opt-in, added to (and removed from)
  the left dock's existing `TabPanel` at runtime via `DockArea::add_panel`/
  `remove_panel` (a `ToggleOutline` command), rather than always-mounted
  alongside the explorer with only a whole-dock toggle (the `ToggleExplorer`/
  `ToggleProblems`/`ToggleSourceControl` precedent) — the spec calls for a
  *palette toggle specific to the outline panel*, and gpui-component's
  `DockArea` has no API to switch which tab is active within an
  already-mounted `TabPanel`, so add/remove is the mechanism that gives the
  outline panel its own show/hide affordance without forking the dock. Kind
  glyphs are single ASCII letters (theme-token colored by a 4-bucket
  category), not icon-asset glyphs — matching the file tree's own precedent
  of text glyphs over `IconName` assets (`crate::file_tree`'s directory
  twisty), since the shipped binary does not embed gpui-component's SVG
  icon set.
