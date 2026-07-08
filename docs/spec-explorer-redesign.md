# Spec: Explorer redesign

> Status: READY
> Created: 2026-07-08
> Completed: —

Establish the explorer's new visual language — the "Explorer — Redesign" Paper
artboard as the binding visual contract, plus the client-side implementation of
the redesigned chrome, row anatomy, and density in `file_tree.rs` that ships now
and reserves the structural slots the explorer-overhaul phases 28–31 fill.

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. This is the **visual-foundation** phase of the explorer overhaul: it
supersedes the "Cockpit — IDE" artboard's explorer panel with the new
"Explorer — Redesign" artboard, and it re-lays-out and re-densifies the shipped
Phase-11/Phase-25 tree so it reads as that artboard's **A Default** column. It
does **not** add file-type icons, a context menu, file operations, or search —
those are their own phases (28–31); Phase 27 only prepares the slots they land
in, and ships **no dead control** (a control with no capability yet is simply
absent, matching the Phase-25 discipline).

- [ ] The **"Explorer — Redesign" artboard** (Paper file `rift`) is the
      explorer's visual contract, superseding the "Cockpit — IDE" explorer
      panel. It is a **design artifact only** — no constitution, architecture,
      or protocol change (recorded in the roadmap's phase-27 foundation-impact
      note and in Prior decisions below).
- [ ] The explorer renders the artboard's **row anatomy**: after the fixed-width
      chevron/spacer slot, a **fixed-width icon slot** (reserved as structure;
      it carries a neutral placeholder — real file-type SVG glyphs are Phase 28,
      not this phase), then the name in the flexible middle, then the trailing
      decoration cluster (diagnostic dot + right-aligned git-letter lane) carried
      over from Phase 25 and re-spaced to the artboard. Every slot is
      `flex_shrink_0` so names and the git lane column-align across rows and
      depths, exactly as the shipped tree already aligns its trailing lane.
- [ ] The explorer renders the artboard's **row density**: the artboard's block
      padding, row radius, row height, and indent-per-level lanes replace the
      shipped constants, so the tree reads at the redesigned rhythm. Values track
      the artboard (layout pixels, not theme colors); no magic color literals.
- [ ] **Hover** and **selected** rows match the artboard's treatment on the new
      density: hover is the base surface tint, selected keeps the Phase-25
      inset accent bar + active-surface fill, both via theme tokens. (The
      artboard's discrete **multi-select** treatment is documented in the
      contract but **not implemented** here — multi-select is a Phase-31
      capability, so rendering an unreachable multi-selected state would be dead
      code.)
- [ ] The explorer renders the artboard's **header band + action row**: the
      redesigned `EXPLORER` band at the artboard height, with the right-aligned
      action cluster re-laid-out into fixed-width icon-slots at the artboard gap.
      It ships **exactly the two live actions that already ship** — Collapse all
      / Expand all (a toggle) and Reveal active file — as text-glyph buttons.
      The artboard's search/filter toggle (Phase 31) and new-file glyph
      (Phase 30) are **absent**, not dimmed-and-dead. The redesigned
      **workspace-root (`RIFT`) row** re-densifies to match.
- [ ] The **loading** and **empty-root** placeholders (the Phase-25 split on
      `WorktreeModel::root()`) are restyled to the redesigned visual language —
      quiet, centered, muted, no action surface — so the empty panel reads as
      the redesign, not the old chrome.
- [ ] The panel's `render()` shell keeps the header / root-row / scrollable-tree
      stack, so a Phase-31 filter band can be inserted between the header and the
      tree without a re-layout. Phase 27 adds **no** empty filter element (that
      would be dead structure); it only keeps the seam clean.
- [ ] The explorer stays **agent-agnostic** and reads only the existing client
      model — paths, kinds, git status, diagnostics, `ignored`, and `root()`;
      **no new protocol message**, no daemon change, no new dependency. The
      change is confined to `crates/app/src/file_tree.rs` rendering plus its
      layout constants.

## Scope

### In scope

The visual contract, plus **client-side rendering only** in
`crates/app/src/file_tree.rs` (chiefly `render()`, `render_header()`,
`render_root_row()`, `render_row()`, and the panel's layout constants). The
binding visual reference is the Paper **"Explorer — Redesign"** artboard (file
`rift`).

- **Adopt the new artboard as the visual contract.** The "Explorer — Redesign"
  artboard clones the shipped Cockpit explorer panel verbatim (same surfaces,
  indent lanes, right-aligned git lane, selection + rollup) and adds only the
  overhaul affordances across four state columns (A Default, B Search, C Inline
  rename, D Context menu) and an ANATOMY & TOKENS legend. Phase 27 implements the
  **A Default** column; B/C/D are the contracts phases 31/30/29 realize. The
  artboard supersedes the "Cockpit — IDE" explorer panel as the reference for the
  explorer surface.
- **Row anatomy (`render_row`).** Restructure the row into the artboard's slots:
  chevron/spacer slot (kept) → **reserved icon slot** (fixed width, neutral
  placeholder; the real language-tinted file-type glyphs are Phase 28) → name
  (flexible middle) → trailing cluster (diagnostic dot + right-aligned
  fixed-width git-letter lane, both from Phase 25, re-spaced to the artboard's
  7px dot / 12px letter slot). Every slot `flex_shrink_0` so columns align.
- **Row density (`render_row` + layout constants).** Replace the shipped
  `ROW_HEIGHT` (22px), `INDENT_PER_LEVEL` (14px), and row padding with the
  artboard's values (block padding, row radius, indent lanes 8/24/40/56 → 16px
  per level over an 8px base). Density constants are plain layout pixels, not
  theme tokens.
- **Hover + selected treatment (`render_row`).** Hover = base surface tint;
  selected = the Phase-25 inset 2px accent bar + active-surface fill, on the new
  density. Theme tokens only (accent / list-active / list-hover roles).
- **Header band + action row (`render_header`).** Re-lay-out the `EXPLORER` band
  to the artboard height and label style, with the right action cluster in
  fixed-width icon-slots at the artboard gap. Keep only the two live text-glyph
  actions (Collapse/Expand toggle, Reveal active file); ship no search/filter or
  new-file control (no capability yet).
- **Workspace-root row (`render_root_row`).** Re-densify the `RIFT` row (leaf of
  `root()`, uppercased, disclosure chevron driving collapse-all/expand-all) to
  the new rhythm. Neutral while `root()` is `None`, as today.
- **Loading / empty-root placeholders (`render`).** Restyle the two Phase-25
  placeholders to the redesigned visual language; keep the `root()`-based split
  and the passive, centered, muted treatment.

### Out of scope — each its own later phase

- **File-type icons + SVG asset embedding — Phase 28.** The artboard's
  language-tinted file glyphs and folder/open-folder/chevron icons need
  `gpui-component`'s SVG icon assets embedded in the product binary (the exact
  gap `file_tree.rs` already documents — only the dev-only `gallery` binary
  enables the icon assets). Phase 27 **reserves the icon slot as structure** and
  fills it with a neutral placeholder / keeps the shipped text-glyph markers;
  Phase 28 replaces that with the real icon theme. No SVG assets are embedded
  here.
- **Context-menu action framework — Phase 29.** The artboard's **State D**
  right-click popover (client-capable actions grouped above the phase-gated write
  actions) is Phase 29's interaction shell. Phase 27 adds **no** right-click
  handler; the row already is the trigger surface (its existing click target),
  so no new structure is needed.
- **File operations (create / rename / delete / move) — Phase 30.** The
  artboard's **State C** inline rename and the write actions in State D need a
  daemon write path (new `protocol` messages, daemon `std::fs` handlers). Phase
  27 ships none of them; the artboard documents them as the FILE-OPS phase.
- **Search / filter / quick-open + multi-select — Phase 31.** The artboard's
  **State B** fuzzy filter bar, filtered match emphasis, and the discrete
  multi-select treatment are Phase 31. Phase 27 renders **no** filter input and
  **no** multi-selected state (both would be unreachable dead UI); it only keeps
  the `render()` shell's seam clean so the filter band inserts cleanly later.
- **Decoration, roll-up computation, reveal, keyboard navigation, the row
  cache, empty-state split, and the `#309` ignored-files behavior** — all shipped
  by Phase 11 / Phase 25 and unchanged. `compute_rollup`, `reveal`,
  `refresh_row_cache`, the action set, and the `FileTreeEvent` wire stay as-is;
  Phase 27 only changes how their results are **arranged, sized, and styled**.
- **Protocol / daemon / explorer-crate changes.** Purely client rendering; the
  daemon, `crates/protocol`, and `crates/explorer` are untouched.
- **Source-control panel, status bar, editor chrome, settings** — their own
  specs.

## Human prerequisites

None. Client-side rendering only: no new dependency, no protocol addition, no
daemon change, no secrets or provisioning. The "Explorer — Redesign" artboard is
authored and is the visual reference; the Catppuccin-Mocha theme tokens it pulls
from are already vendored via `gpui-component`.

## Constraints

- **Reads the existing model, no new protocol** (constitution: `protocol` is a
  deliberate API surface). Every value the redesign renders — paths, kinds, git
  status, diagnostics, `ignored`, `root()` — is already on `WorktreeModel`. No
  wire message is added; no daemon or `explorer`-crate code changes.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`), never hardcoded
  hex. The artboard quotes exact hex for review legibility, but every color maps
  to an existing theme role the shipped tree already uses (base/surface for
  hover, list-active + accent for selection, danger/warning/success/info for the
  git lane and diagnostic dot, muted-foreground for the label and placeholders).
  Layout **dimensions** (row height, padding, radius, indent, slot widths, gaps)
  are plain layout pixels — permitted, and the shipped tree already uses `const`
  pixels the same way.
- **No dead controls / no dead visuals** (constitution, Phase-25 precedent). A
  control or a visual state with no reachable capability is **absent**, not
  dimmed-and-inert: the search/filter toggle, the new-file glyph, the inline
  rename, the context menu, and the multi-select treatment are documented in the
  artboard but **not rendered** by Phase 27. Reserved **structural slots** (the
  fixed-width icon column; the header-to-tree seam) are layout, not controls, and
  are allowed — they render nothing interactive.
- **Agent-agnostic** (constitution): chrome and decoration derive only from
  filesystem / git / LSP / model signals; no agent detection, no output parsing.
  The header actions touch only view-local state (`collapsed`) and the existing
  reveal path.
- **The redesign is a re-arrangement, not a data change.** `compute_rollup`,
  `refresh_row_cache`, `reveal`, the keyboard-nav actions, and the empty-state
  split are unchanged; the cache-dirty discipline (`model_mut` / `toggle_dir`
  mark dirty) is untouched, so density and slotting changes cannot drift the
  cache. This mirrors Phase 25's "changes how results are arranged, not what they
  are" ethos.
- **The in-panel header band is the design's panel header**, distinct from the
  dock-tab identity `Panel::title` returns ("Explorer"); the redesign restyles
  the band, it does not remove or duplicate the dock chrome.
- **Icon assets stay unembedded here.** The product binary still lacks
  `gpui-component`'s SVG icon assets (documented in `file_tree.rs`), so header
  actions and the reserved icon slot stay text-glyph / neutral in Phase 27; the
  SVG embedding + real file-type icons are Phase 28. A `IconName` icon would
  render blank in the product binary today.
- **No `.unwrap()` in library code**; no `todo!()` in merged code; a decoration
  or state the model does not carry is not rendered (no placeholder glyph),
  matching the tree's existing "render only what the model carries" discipline.
- **Reuse `gpui-component` widgets, never fork them** (constitution): the header
  actions stay `Button` (ghost, xsmall) as today; the tree stays
  `v_virtual_list`; the redesign only changes layout/tokens around them.
- **Headless-testable seams.** The row-slot layout (icon slot reserved, trailing
  cluster order), the density constants, the header action set (still exactly the
  two live actions), and the loading-vs-empty split stay pure model reads /
  layout the existing Phase-11/Phase-25 unit tests already cover; the visual
  arrangement (lane alignment, accent bar, band density) is validated at the
  milestone QA gate against the artboard.

## Prior art

Consulted the "Explorer overhaul — prior-art index (Phases 27–31)" in
`prior-art.md`, the shipped Phase-11 tree (`spec-explorer-panel.md`), and the
shipped Phase-25 parity pass (`spec-explorer-parity.md`).

- **Paper "Explorer — Redesign" artboard (file `rift`)** — the binding visual
  contract. It clones the shipped Cockpit explorer panel verbatim (surfaces,
  256px width, header band, `RIFT` root row, indent lanes, right-aligned git
  lane, inset-accent selection, collapsed-chevron rollup) and adds the overhaul
  affordances across four state columns (A Default, B Search, C Inline rename,
  D Context menu) plus a three-group ANATOMY & TOKENS legend (file-type/folder
  icon hexes, header-action live-vs-phase tags, row-states & git-lane swatches).
  Phase 27 implements **column A**; B/C/D are the contracts phases 31/30/29
  realize. Every value is drawn 1:1 from the Styleguide Catppuccin-Mocha token
  set; the only palette extension is the language icon tints (peach `.rs`, teal
  `.toml`, info `.md`), documented in the legend and landing with Phase 28.
- **`zed` `crates/project_panel`** (GPL-3.0, study-only): standard IDE-explorer
  row anatomy, density, and the trailing decoration column — the same reference
  Phases 11/25 used, patterns not code.
- **rift-local grounding**: `crates/app/src/file_tree.rs` (the shipped tree —
  `HEADER_HEIGHT` 28px, `ROW_HEIGHT` 22px, `INDENT_PER_LEVEL` 14px, the two live
  header actions, the trailing dot + git-letter lane, collapsed-only rollup,
  loading/empty split, text-glyph markers) and `crates/app/src/worktree.rs`
  (`root()` distinguishes not-yet-loaded from empty; `entries()` enumerates the
  dirs collapse-all folds).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **The "Explorer — Redesign" artboard supersedes the "Cockpit — IDE" explorer panel as the visual contract — a design artifact only, no constitution / architecture / protocol change** | The redesign is the visual foundation phases 28–31 build on; recording it as a design-doc supersession (not a foundation-doc edit) keeps the contract in the design layer where it belongs. The roadmap's phase-27 foundation-impact note already states this. | 2026-07-08 |
| **Phase 27 ships the artboard's A Default column only; B (search), C (rename), D (context menu) are contracts for phases 31/30/29** | Those states map to capabilities that do not exist yet (fuzzy filter, daemon write path, context-menu framework). Shipping them as UI with no capability is a dead control / dead visual (constitution; Phase-25 precedent). The artboard documents them; the later phases realize them. | 2026-07-08 |
| **The icon slot is reserved as structure now; real file-type SVG icons land in Phase 28** | The product binary does not embed `gpui-component`'s SVG icon assets (documented in `file_tree.rs`), so the real language-tinted glyphs cannot render until Phase 28 embeds them. Reserving a fixed-width icon slot (neutral placeholder) now means Phase 28 drops icons in with **no** re-layout. A reserved layout slot is not a dead control. | 2026-07-08 |
| **Header ships exactly Collapse-all/Expand-all + Reveal-active; search/filter and new-file are absent, not dimmed** | The two live actions map to shipped client capability (Phase 25). The artboard renders search/filter (Phase 31) and new-file (Phase 30) dimmed for review legibility, but Phase 27 must not ship a control with no capability. It restructures the action-row **layout** (fixed-width slots, artboard gap) so those actions slot in later without re-layout. | 2026-07-08 |
| **Header actions and the reserved icon slot stay text-glyph / neutral in Phase 27** | Same asset gap: an `IconName` icon renders blank in the product binary. Text-glyph `Button` labels render reliably (as the shipped header already does). Phase 28 swaps glyphs for SVG icons across the header and the icon slot together. | 2026-07-08 |
| **Multi-select treatment is documented in the artboard but not implemented** | Multi-select is a Phase-31 capability; rendering a multi-selected row state that no interaction can reach is dead code. Phase 27 keeps single-selection (the shipped accent-bar treatment, re-densified); the artboard's discrete multi-select fill is Phase 31's to wire. | 2026-07-08 |
| **The redesign re-arranges and re-densifies; it does not touch decoration data, rollup, reveal, keyboard nav, the row cache, or the empty-state split** | Those all shipped in Phases 11/25 and are unchanged. Confining Phase 27 to `render_*` layout + layout constants keeps the change reviewable and cannot drift the derived row cache. | 2026-07-08 |
| **Layout dimensions are plain layout pixels; only colors go through theme tokens** | The shipped tree already uses `const` pixel values for row height / indent / slot widths and theme tokens for every color. The redesign follows the same split: artboard pixel values become layout constants; artboard hexes map to existing theme roles. No hardcoded color literal is introduced. | 2026-07-08 |
| **Client-only, no daemon / protocol / dependency change** | Every needed value is already streamed and folded onto the client model; this is pure rendering. Matches Phase 11/25's "reads the existing model" ethos and the roadmap's phase-27 no-foundation-impact note. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Phase 27 — Explorer redesign (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] The explorer renders the redesigned row anatomy: a fixed-width **icon
      slot** (neutral placeholder — no SVG) sits between the chevron/spacer slot
      and the name; the name occupies the flexible middle; the trailing cluster
      (diagnostic dot + right-aligned git-letter lane) keeps the Phase-25 order
      and column-aligns across rows and depths. Asserted at the QA gate against
      the artboard's A Default column; the slot-order / fixed-width invariants are
      exercised headlessly where the existing render tests reach them.
- [ ] The redesigned **density** (row height, block padding, radius, indent
      lanes) replaces the shipped constants and reads at the artboard rhythm; a
      `grep` confirms the density values live as layout constants, not inline
      literals scattered per row.
- [ ] **Hover** and **selected** rows match the artboard: hover = base tint,
      selected = inset accent bar + active-surface fill, all via theme tokens; a
      `grep` confirms no hardcoded color literal was introduced.
- [ ] The redesigned **header band** renders at the artboard height with the
      `EXPLORER` label and a right action cluster of fixed-width slots; it ships
      **exactly** Collapse-all/Expand-all + Reveal-active and **no** search/filter
      or new-file control. Collapse-all still folds every `EntryKind::Dir` and
      toggles to Expand-all (asserted headlessly over `collapsed` / `visible_rows`
      as in Phase 25); Reveal-active still fires the workspace reveal path; the
      `RIFT` root row re-densifies and its chevron still mirrors the collapse
      state.
- [ ] The **loading** (`root()` `None`) and **empty-root** (`root()` `Some` +
      `is_empty()`) placeholders are restyled to the redesign and stay distinct,
      passive, centered, and muted; a populated root shows the tree. Asserted
      headlessly via the model accessors (the Phase-25 tests carry over).
- [ ] The `render()` shell still stacks header / root-row / scrollable tree so a
      Phase-31 filter band inserts between header and tree without a re-layout,
      and Phase 27 adds **no** empty filter element.
- [ ] `grep` confirms no agent detection introduced, no new protocol message
      added, and no change under `crates/protocol`, `crates/daemon`, or
      `crates/explorer` — the change is confined to `crates/app/src/file_tree.rs`.
- [ ] Milestone QA (dev channel): the explorer reads like the "Explorer —
      Redesign" artboard's **A Default** column — redesigned header band + live
      actions, the reserved icon slot and re-densified rows, the trailing
      dot + git-letter cluster aligned in its lane, hover/selection legible, and
      the loading/empty placeholders restyled — while search, icons, context
      menu, and file ops remain visibly absent (deferred to 28–31).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The reserved icon slot leaves a visible empty gap that reads as broken until Phase 28 | The slot carries a neutral placeholder (or keeps the shipped text-glyph marker) rather than empty whitespace, so the row reads as intentional; the fixed width matches the Phase-28 icon so no re-layout follows. QA confirms the row reads clean without real icons. |
| The redesign accidentally ships a dimmed-but-dead control (search / new-file) by cloning the artboard too literally | Prior decision + Outcome make these **absent**, not dimmed; the header ships exactly the two live actions. QA and a `grep` for the omitted actions confirm nothing dead was added. |
| Density-constant changes drift the row cache or break the virtual-list size vector | Density is layout only; `ROW_HEIGHT` stays a single `const` the size vector reads, so changing it keeps the vector correct. `compute_rollup` / `refresh_row_cache` are untouched, so the derived cache cannot drift. Existing Phase-11 cache tests carry over. |
| Two issues both edit `file_tree.rs` → rebase churn | Split by disjoint seam: `render_row` (row anatomy + density + hover/selected) is issue 1; the `render()` chrome (`render_header`, `render_root_row`, the loading/empty branch) is issue 2, sequenced after issue 1 so the shared layout constants land once. |
| Restyling introduces a hardcoded hex where a theme token exists | Constraint + a `grep` gate: every color goes through an existing theme role; only layout pixels are literals. Reviewed per PR. |
| The header band is mistaken for the dock-tab title and duplicated | The band is the panel's own header (design); `Panel::title` stays the dock identity — unchanged from Phase 25. Noted as a constraint; QA confirms no redundancy. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 27 — Explorer
  redesign, the visual foundation of the explorer overhaul 27–31). Visual
  contract is the new Paper **"Explorer — Redesign"** artboard (file `rift`),
  which clones the shipped Cockpit explorer panel verbatim and adds the overhaul
  affordances across four state columns (A Default, B Search, C Inline rename,
  D Context menu) plus an ANATOMY & TOKENS legend; it supersedes the "Cockpit —
  IDE" explorer panel as the explorer's reference. Scope held to client-side
  re-layout / re-density of `file_tree.rs` implementing the artboard's **A
  Default** column and reserving the structural slots (fixed-width icon column;
  the header-to-tree seam) that phases 28–31 fill — with **no dead control**:
  file-type icons (Phase 28), context menu (Phase 29), file operations
  (Phase 30), and search/filter + multi-select (Phase 31) are documented in the
  artboard but not rendered. No protocol / daemon / dependency change; a design
  artifact plus client rendering only.
