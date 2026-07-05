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
- [ ] References open in a right-dock panel per §3 (search-context chip,
      "N references · M files", file groups with count badges, match rows
      with the symbol highlighted, active row accent) — replacing the
      jump-list overlay (#485 gave it a dismiss path; this replaces it).
- [ ] An OUTLINE panel (left dock, §3 anatomy: kind glyph lanes + mono names,
      selection follows cursor, click jumps) renders from document symbols.
- [ ] The dirty-buffer conflict surfaces as the design's dialog ("File
      changed on disk", body copy, secondary "Keep mine" + primary "Reload
      from disk") — upgrading the #433 banner; same two actions.
- [ ] Minimap per gate decision (see Prior decisions once resolved).
- [ ] All colors/typography via theme tokens; no dead controls.

## Scope

### In scope

- `protocol`/`daemon`/`lsp` (deliberate, minimal API change): a
  `DocumentSymbolRequest { id, path }` → `DocumentSymbolResponse` pair
  (flattened symbol list: name, kind, range, selection_range, depth) via the
  existing nav-request machinery (drop-stale ids, per-connection reply per
  #482's routing fix); serves breadcrumb AND outline. Version bump per the
  fingerprint policy.
- `app` (editor.rs): breadcrumb bar (path from the open tab, symbol =
  innermost symbol containing the cursor, updated on cursor move against the
  cached symbol tree — one request per open/change, not per keystroke);
  gutter severity dots; inline diagnostic card for the cursor line (replaces
  the plain inline text row); current-line highlight if not already present.
- `app`: hover card restyle to §3 (code block + doc + action row wired to
  the existing GoToDefinition / FindReferences actions with kbd hints).
- `app`: references panel in the right dock (gpui-component Panel like
  SourceControl/DiffView), fed by the existing ReferencesResponse; grouped by
  file, count badges, click jumps, Escape/× closes; the overlay path is
  removed.
- `app`: outline panel in the left dock (toggle via palette + the phase-21
  rail gains its icon in a follow-up there), fed by the document-symbol
  cache of the active editor.
- `app`: conflict dialog via gpui-component modal (the #423 close-confirm
  pattern), actions identical to the #433 banner remedies.
- Minimap: per gate decision — if IN: a non-interactive-scroll marks strip
  (~14px: line-length marks, diagnostic tints, viewport slab, click-to-jump)
  as its own issue; if OUT: recorded deviation from the artboard, revisited
  post-v1.

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
| References panel replaces the jump-list overlay | The design shows a persistent right panel; the overlay was the interim (its dismiss fix #485 stays useful until this lands) | 2026-07-05 |
| Conflict UI = modal dialog on the #423 confirm pattern with the #433 actions | Design §7 shows a dialog; the banner was the papercut-scale interim | 2026-07-05 |
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
