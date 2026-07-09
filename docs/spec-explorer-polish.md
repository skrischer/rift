# Spec: Explorer polish — v1.1 QA refinements

> Status: READY
> Created: 2026-07-09
> Completed: —

Five cohesive refinements to the shipped explorer (v1.1.0, phases 27–31),
surfaced by live QA against Zed's file explorer as the reference: indent guide
lines, full-row hover/selection highlight, a Zed-style chevron-less disclosure
model (fully-left workspace-root row driving whole-tree collapse; the folder
icon itself as the per-folder disclosure), legible folder-icon tints, and an
industry-standard file-type icon mapping. Some of these deliberately **revise**
prior decisions (Phase 28's disclosure chevrons are removed; Phase 27's
chevron-driven root row is replaced; Phase 28's `overlay` folder tint and its
curated icon subset are corrected/broadened).

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. Step progress lives in the issues, never here.

- [ ] **Indent guide lines.** Every row renders one thin vertical guide line per
      nesting level, aligned to the existing indent-lane geometry (Phase 27's
      `INDENT_BASE` 8px + `INDENT_PER_LEVEL` 16px), so a run of nested/expanded
      levels between two siblings is legible at a glance (as in Zed). The line is
      drawn via a subtle theme token (a border/muted role), never a hardcoded
      hex, and re-tints on a theme switch.
- [ ] **Full-row highlight.** The hover and selected/active backgrounds span the
      **full panel width** (the entire row, edge to edge), not just the
      text/icon extent. The row's content keeps its per-depth indent; only the
      highlight surface becomes full-width. The Phase-27 inset accent bar +
      active-surface fill for the cursor row, and the `secondary` fill for a
      multi-selected non-cursor row, are preserved on the now-full-width surface.
- [ ] **Chevron-less disclosure model (Zed-style).** The per-folder disclosure
      chevron/twisty is **removed entirely**; the **folder icon** (open vs closed
      glyph) is the disclosure affordance, and clicking a folder row still
      toggles its expansion. The reclaimed chevron-slot width (the 12px slot plus
      its slot gap) is removed so every row shifts left accordingly. Files no
      longer reserve a blank chevron spacer.
- [ ] **Fully-left workspace-root row + whole-tree collapse.** The workspace-root
      row (leaf of `model.root()`, the `RIFT` row) renders **fully left-aligned**
      with no chevron and no reserved icon/chevron slot, as the project root.
      Clicking it collapses/expands the **entire tree**: collapsed hides all
      root-level files and folders (an empty tree body); expanded restores the
      prior tree. This is distinct from — and does not disturb — the header's
      collapse-all/expand-all action, which continues to fold every directory
      while leaving root-level entries visible.
- [ ] **Preserved navigation & header behavior.** Keyboard navigation
      (`SelectUp`/`SelectDown`, `OpenSelected`, `CollapseOrSelectParent` /
      `ExpandOrSelectChild` collapse/expand on the selected row, `SelectFirst`/
      `SelectLast`, and the `Shift`-extend multi-select) and the header's
      collapse-all/expand-all toggle behave exactly as before; only the
      per-folder disclosure *affordance* (chevron → folder glyph) and the
      root-row *semantics* (collapse-all → whole-tree collapse) change.
- [ ] **Legible folder icons.** Both the open and closed folder glyphs are
      clearly legible against the panel surface. The collapsed-folder tint no
      longer resolves to the `overlay` token (which in the shipped Catppuccin
      Mocha theme is a near-black scrim, `#11111bcc`, and reads as invisible on
      the dark sidebar); both folder tints resolve to theme roles with adequate
      contrast against the sidebar/background surface, via theme tokens with no
      hardcoded hex.
- [ ] **Industry-standard file-type icon mapping.** The extension → icon mapping
      is broadened to the de-facto standard (Zed's default icon theme, which is
      Seti-derived) and made semantically correct: the current markdown glyph
      (which renders as a down-arrow — the Seti *default* glyph mis-named
      `markdown.svg`) is replaced with a correct markdown glyph, and the common
      language/config types (`rs`, `ts`, `tsx`/`jsx`, `js`, `py`, `go`, `rb`,
      `java`, `c`/`h`, `cpp`, `json`, `yaml`/`yml`, `toml`, `md`, `html`,
      `css`, `scss`/`sass`, `sh`, `lock`, `.gitignore`, `LICENSE`, `Dockerfile`)
      each map to a recognizable, semantically-correct Seti/Zed glyph tinted
      through a theme token, with the `default_file` fallback for the long tail —
      never a blank slot.
- [ ] **License-clean, agent-agnostic, no new dependency.** The newly vendored
      SVGs are Seti UI (MIT, already the vendored provenance under
      `crates/app/assets/file_icons/LICENSE`); `cargo deny check licenses` still
      passes with **no new crate dependency** (served by the existing
      `RiftAssets`/`rust-embed` source). The icon still derives only from a row's
      leaf name/extension and `EntryKind` + collapse state; no new protocol
      message, no daemon change, no agent detection. The change is confined to
      `crates/app` (`file_tree.rs`, `file_icons.rs`, vendored `assets/`).

## Scope

### In scope

Client-side only, in `crates/app`. The two source seams are
`crates/app/src/file_tree.rs` (row/root rendering, disclosure model, indent
guides, full-row highlight) and `crates/app/src/file_icons.rs` (the mapping
table + tint roles), plus the vendored SVGs under
`crates/app/assets/file_icons/`. The visual reference is **Zed's file
explorer** (the QA screenshot: fully-left project root, folder rows with folder
icons and no chevrons, faint vertical indent guides, full-width row highlight).

- **Indent guide lines (`render_row`).** Render `depth` thin vertical lines in
  the row's leading indent region, one per level, positioned on the existing
  indent lanes (`INDENT_BASE + level * INDENT_PER_LEVEL`), each spanning the row
  height, colored with a subtle theme token (a `border`/muted role). Must align
  1:1 with Phase 27's indent geometry so the guide sits under the icon/name
  indent already in place. Applies to the plain, rename, and create row
  renderings alike (they share the row layout).
- **Full-row highlight (`render_row`).** Make the row's hover/selected background
  surface full panel width (e.g. the row container becomes `w_full`), while the
  content (icon + name + trailing cluster) keeps its per-depth indent. Preserve
  the Phase-27 cursor treatment (inset accent bar + `list_active` fill) and the
  Phase-31 `secondary` multi-select fill on the full-width surface. The
  edge-to-edge look is the Zed reference; the row radius may be dropped for a
  true full-bleed highlight (a QA-gate visual decision).
- **Remove per-folder chevrons; folder icon as disclosure (`render_row`,
  `render_rename_row`, `render_create_row`).** Delete the `twisty` chevron
  element and its fixed slot; drop the blank chevron spacer on files. The folder
  icon slot (already `folder_icon_for(is_expanded)` → open/closed glyph) becomes
  the sole disclosure affordance; the existing directory `on_click` →
  `click_dir` → `toggle_dir` toggle is unchanged. Rows shift left by the
  reclaimed chevron width + gap.
- **Fully-left root row + whole-tree collapse (`render_root_row`,
  `visible_rows`/`refresh_row_cache`, a new view-local `root_collapsed` state,
  `render()` wiring).** Remove the root row's chevron and its reserved slot;
  render the uppercased root leaf flush-left at a small base padding. Clicking
  the root row toggles a new `root_collapsed` flag; while set, the visible-row
  derivation yields an empty tree body (hiding all root-level entries), and the
  root row's own folder-state affordance reflects collapsed/expanded. Expanding
  restores the prior tree verbatim (the real per-folder `collapsed` set is never
  touched by the whole-tree toggle, mirroring the filter bar's discipline).
- **Legible folder tints (`file_icons.rs`).** Re-map the folder tint roles off
  `overlay`: collapsed-folder and expanded-folder each resolve to a theme role
  that clears a contrast bar against the panel surface (recommended:
  collapsed → `muted_foreground`, expanded → `primary`/blue — both legible on
  `#181825`/`#1e1e2e`; the exact token confirmed at the QA gate against the live
  sidebar surface). Remove the now-unused `Overlay` tint role.
- **Broaden + correct the file-type mapping (`file_icons.rs` + vendored SVGs).**
  Extend `FILE_TYPES`/`FULL_NAME_TYPES` to the industry-standard set below,
  vendor the additional monochrome Seti UI SVGs, and replace the incorrect
  `markdown.svg` (currently the Seti default down-arrow) with a correct markdown
  glyph. Each entry maps to a distinctive theme-token tint; add the tint roles
  the broadened palette needs (Catppuccin-mapped named color tokens). Keep
  `default_file` as the fallback (chrome `IconName::File`, `muted_foreground`).

### Out of scope — deliberately not this batch

- **User-swappable icon themes / loadable JSON icon theme.** Still a single
  bundled Rust-static set (Phase 28's deferred follow-up is unchanged); this
  batch only broadens and corrects the static table.
- **New file types beyond the industry-standard common set.** The long tail
  stays on `default_file`; expanding further is a mechanical table edit, not a
  re-spec.
- **Symlink/submodule/special-kind icons.** The model carries only
  `EntryKind::File`/`Dir`; no new kind is introduced.
- **The trailing decoration cluster, roll-up computation, reveal, the row cache,
  git/diagnostic tints, filter bar, multi-select, context menu, file ops, and
  drag-drop** — all shipped by phases 11/25/27–31 and unchanged. This batch only
  re-arranges the row's leading region (guides, disclosure, indent), the
  highlight surface width, the root-row semantics, and the icon mapping/tints.
- **Protocol / daemon / explorer-crate changes.** Purely client rendering plus
  the vendored assets; `crates/protocol`, `crates/daemon`, and `crates/explorer`
  are untouched.
- **Re-authoring the "Explorer — Redesign" Paper artboard.** The artboard should
  be updated to match this batch (chevron-less rows, fully-left root, indent
  guides, full-row highlight, corrected icons) — recorded as a **follow-up**, not
  required by this spec (see the decision log). No design-artifact re-author is a
  gate for these code changes.

## Human prerequisites

None. Client-side and self-contained:

- **No new crate dependency.** The additional SVGs are vendored assets served by
  the existing `RiftAssets`/`rust-embed` source (`main.rs`, `#[folder = "assets"]`
  / `#[include = "file_icons/**/*.svg"]`), so `cargo deny check licenses` (which
  scans the Cargo graph) is unaffected.
- **License-clean assets.** The new glyphs are Seti UI (Jesse Weed, MIT) — the
  same MIT set already vendored under `crates/app/assets/file_icons/LICENSE`
  (confirmed: the shipped `rust.svg`/`json.svg` carry the Seti provenance). MIT
  is in `deny.toml`'s allowlist. Any substitute set must be MIT/Apache-2.0/ISC/
  CC0 and monochrome/tintable.
- The Catppuccin Mocha theme tokens the tints resolve against are already
  vendored via `gpui-component`; Zed's file explorer is the authored visual
  reference (the QA screenshot).

## Constraints

- **Builds on v1.1.0 (phases 27–31).** The fixed-width icon slot, row density
  constants, trailing cluster, keyboard actions, row cache, and `render()` shell
  already exist. This batch edits how the row's leading region and highlight are
  laid out, the root-row semantics, and the icon mapping — it does not add a new
  protocol, control, or crate.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`), never hardcoded
  hex. Indent guides, folder tints, and every file-type tint resolve to an
  existing `ThemeColor` role (`border`/`muted_foreground`/`primary`/named base
  colors `blue`/`cyan`/`yellow`/`green`/`red`/`magenta` + `info`/`warning`).
  `overlay` is **forbidden** as an icon fill (it is a near-black scrim in the
  shipped theme). Layout dimensions (indent lanes, slot widths, guide width) stay
  plain layout pixels, as Phase 27 established.
- **The `overlay` folder-tint premise from Phase 28 was wrong** for this theme.
  Phase 28's icon-tint table assumed `overlay ≈ #6C7086` (Catppuccin overlay0);
  the shipped `catppuccin-mocha.json` maps the `overlay` token to `#11111bcc` (a
  crust-black scrim at ~80% alpha), so collapsed folders render invisibly. The
  tint must be re-mapped to a legible role; verified against the live sidebar
  surface (`sidebar` `#181825` / `background` `#1e1e2e`).
- **Agent-agnostic** (constitution): icons and chrome derive only from path
  extension + `EntryKind` + collapse state + view-local UI state. No agent
  detection, no output parsing, no process inspection.
- **Reuse `gpui-component` widgets, never fork them** (constitution): icons render
  through the vendored `Icon` (`Icon::new(IconName)` for chrome / folder /
  generic-file, `Icon::empty().path(<svg>)` for a vendored file-type glyph);
  header actions stay `Button` (ghost, xsmall); the tree stays `v_virtual_list`.
  Indent guides and the full-width highlight are plain `div` styling, not new
  widgets.
- **No dead controls / no dead visuals** (constitution). Removing the chevron
  removes a now-redundant affordance (the folder glyph carries disclosure); the
  root-row and header actions each retain a distinct, reachable capability. Every
  unmapped extension still resolves to `default_file` — no blank slot.
- **The whole-tree collapse must not corrupt the per-folder `collapsed` set.**
  `root_collapsed` is a separate view-local flag; the visible-row derivation
  short-circuits to empty while it is set, and clearing it restores the exact
  prior tree — the same "scoped, non-destructive" discipline the filter bar's
  force-expansion uses. The `cache_dirty` invalidation discipline
  (`model_mut`/`toggle_dir` mark dirty) extends to the new toggle.
- **No `.unwrap()` in library code**; no `todo!()` in merged code. Extension
  parsing stays total (`to_ascii_lowercase` + `rsplit_once` / full-leaf match);
  a name with no extension falls to `default_file`.
- **Headless-testable seams.** The mapping (extension → glyph + tint), the folder
  open/closed tint selection, the indent-lane geometry, and the whole-tree
  collapse's effect on the visible-row list are pure functions / model reads with
  unit coverage. The visual result (guide legibility, full-bleed highlight, tint
  contrast, glyph fidelity, left-aligned root) is validated at the milestone QA
  gate against the Zed reference.

## Prior art

Consulted the shipped v1.1.0 explorer (`spec-explorer-redesign.md`,
`spec-explorer-icons.md`, and the search/file-ops/context-menu specs), the live
codebase, and Zed's default icon theme.

- **Zed's file explorer** — the binding visual reference for this batch (the QA
  screenshot): a fully-left project-root row, folder rows with folder icons and
  **no chevron twisties**, faint vertical indent guides per level, and a
  full-width row highlight.
- **Zed default icon theme** (`crates/theme/src/icon_theme.rs`, Seti-derived) —
  the authoritative extension → glyph mapping this batch adopts. Confirmed
  against the live source: `rs → rust`, `ts/cts/mts → typescript`,
  `tsx/jsx/… → react`, `cjs/js/mjs → javascript`, `py → python`, `go → go`,
  `rb → ruby`, `java → java`, `c/h → c`, `cpp → cpp`, `json/jsonc → code`,
  `yaml/yml → yaml`, `toml → toml`, `md/markdown → book`, `html/htm → html`,
  `css → css`, `scss/sass → sass`, shell → `terminal`, `lock → lock`,
  `conf/ini → settings`, `Dockerfile/Containerfile → docker`, default → `file`.
  This batch maps to visually-equivalent Seti glyphs vendored under
  `assets/file_icons/`.
- **Seti UI** (`jesseweed/seti-ui`, MIT) — the monochrome, tintable glyph set
  behind Zed's default theme and VS Code's Seti theme; the already-vendored
  provenance (`crates/app/assets/file_icons/LICENSE`, "Copyright (c) 2014 Jesse
  Weed"). Additional glyphs are vendored from the same MIT set.
- **rift-local grounding**: `crates/app/src/file_tree.rs` (`render_row`'s
  `twisty`/`icon_slot`/indent/highlight layout, `render_root_row`'s chevron +
  `toggle_collapse_all`, `visible_rows_unfiltered`'s collapse-aware pass,
  `refresh_row_cache`, the keyboard-nav actions), `crates/app/src/file_icons.rs`
  (`TintRole` incl. the `Overlay` bug, `FILE_TYPES`/`FULL_NAME_TYPES`,
  `folder_icon_for`), `crates/app/src/main.rs` (`RiftAssets` delegating source +
  the asset guard test), and `crates/app/assets/themes/catppuccin-mocha.json`
  (the token values: `overlay #11111bcc`, `muted.foreground #a6adc8`,
  `primary/blue #89b4fa`, `border #45475a`, base named colors).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Remove the per-folder disclosure chevron entirely; the folder icon is the disclosure affordance (open/closed glyph), matching Zed** — REVISES Phase 28's decision to render a real chevron glyph in the chevron/spacer slot | Zed has no chevron twisties; the open/closed folder glyph already encodes disclosure state and `folder_icon_for(is_expanded)` already selects it. A separate chevron is redundant. Reclaiming the slot width shifts rows left to the Zed density. | 2026-07-09 |
| **The workspace-root row renders fully left-aligned (no chevron, no reserved slot) and drives whole-tree collapse/expand** — REVISES Phase 27's chevron-driven root row that mirrored/drove collapse-all | The QA reference (Zed) shows the project root flush-left as the workspace root, and clicking it hides/shows the whole tree. Collapse-all (fold every directory, root entries stay) remains a distinct capability on the header button — the two are separated rather than aliased to one chevron. | 2026-07-09 |
| **Whole-tree collapse is a new view-local `root_collapsed` flag; the visible-row derivation short-circuits to empty while set; the per-folder `collapsed` set is never touched** | Non-destructive and reversible: expanding restores the exact prior tree, mirroring the filter bar's scoped force-expansion. Keeps the model pure and the `cache_dirty` discipline intact. | 2026-07-09 |
| **Folder tints are re-mapped off `overlay`; the `Overlay` tint role is removed** — REVISES Phase 28's icon-tint table (collapsed folder → `overlay`) | Phase 28 assumed `overlay ≈ #6C7086`; the shipped theme maps `overlay` to `#11111bcc` (a near-black scrim), so collapsed folders are invisible. Recommended: collapsed → `muted_foreground`, expanded → `primary`; the exact token confirmed at the QA gate against the live sidebar surface. `overlay` is forbidden as any icon fill. | 2026-07-09 |
| **Broaden + correct the file-type mapping to Zed's default (Seti-derived) set; fix the mis-named markdown glyph** — REVISES Phase 28's "curated subset (rift's repo types + the artboard's three)" scope | The shipped `markdown.svg` is the Seti *default* down-arrow glyph (semantically wrong, "AI-ish"); the shipped set is too small. Adopting Zed's default mapping gives developers recognizable glyphs for the common languages/configs with the `default_file` fallback for the tail. | 2026-07-09 |
| **Add the tint roles the broadened palette needs from `ThemeColor`'s named base colors** (`blue`/`cyan`/`yellow`/`green`/`red`/`magenta`, plus existing `info`/`warning`/`muted_foreground`/`primary`) | Catppuccin Mocha exposes a full named palette via theme tokens (`base.red #f38ba8`, `base.green #a6e3a1`, `base.yellow #f9e2af`, `base.blue #89b4fa`, `base.magenta #cba6f7`, `base.cyan #94e2d5`), so distinctive per-language tints stay theme-token-only, never hex, and re-tint on a theme switch. | 2026-07-09 |
| **Indent guides use a subtle `border`/muted theme token, one line per level, on Phase 27's indent lanes** | The guide must align with the shipped `INDENT_BASE`/`INDENT_PER_LEVEL` geometry so the line sits under the existing icon/name indent; `border` (`#45475a`) reads as a faint lane divider on the dark sidebar and re-tints on a theme switch. | 2026-07-09 |
| **The highlight becomes full panel width; content keeps its per-depth indent** | The QA reference (Zed) highlights the whole row edge-to-edge. Making the row container `w_full` (content still indented) preserves the Phase-27 cursor accent bar + `list_active` fill and the Phase-31 `secondary` multi-select fill on a wider surface. | 2026-07-09 |
| **Client-only, no daemon / protocol / dependency change; assets stay MIT Seti served by the existing `rust-embed`** | Every needed value is already on the client model; the icons are vendored assets, not a crate. Matches the phase 27/28 "reads the existing model / no new crate" ethos and keeps `cargo deny` green. | 2026-07-09 |

### Recommended extension → glyph → tint table

Binding rules: (1) the glyph is a vendored monochrome Seti UI SVG (or the chrome
`IconName::File` fallback); (2) the tint is a `ThemeColor` role, never a hex
literal; (3) folder tints must clear a contrast bar against the sidebar surface
and must not use `overlay`. The exact tint token per row is confirmed at the QA
gate against the live theme; the table below is the recommended, grounded
starting point. Extensions are matched case-insensitively; `.gitignore` /
`LICENSE` match on the full leaf.

| Extension(s) / leaf | Vendored glyph (`file_icons/…`) | Recommended tint role |
|---|---|---|
| `rs` | `rust.svg` (kept) | `warning` (kept) |
| `ts` `cts` `mts` | `typescript.svg` (new) | `primary` (blue) |
| `tsx` `jsx` `ctsx` `mtsx` `cjsx` `mjsx` | `react.svg` (new) | `cyan` |
| `js` `cjs` `mjs` | `javascript.svg` (new) | `yellow` |
| `py` | `python.svg` (new) | `yellow` |
| `go` | `go.svg` (new) | `cyan` |
| `rb` | `ruby.svg` (new) | `red` |
| `java` | `java.svg` (new) | `red` |
| `c` `h` | `c.svg` (new) | `primary` (blue) |
| `cpp` `cc` `cxx` `hpp` `hh` | `cpp.svg` (new) | `primary` (blue) |
| `json` `jsonc` | `json.svg` (kept) | `muted_foreground` (kept) |
| `yaml` `yml` | `yaml.svg` (new) | `magenta` |
| `toml` | `toml.svg` (kept) | `cyan` (kept) |
| `md` `markdown` | `markdown.svg` (**replaced** — correct glyph) | `info` (sky, kept) |
| `html` `htm` | `html.svg` (new) | `warning` |
| `css` | `css.svg` (new) | `primary` (blue) |
| `scss` `sass` | `sass.svg` (new) | `magenta` |
| `sh` `bash` `zsh` | `shell.svg` (kept) | `green` |
| `lock` | `lock.svg` (kept) | `muted_foreground` (kept) |
| `.gitignore` `.gitattributes` | `git_ignore.svg` (kept) | `warning` |
| `LICENSE` | `license.svg` (kept) | `muted_foreground` (kept) |
| `Dockerfile` `Containerfile` | `docker.svg` (new) | `primary` (blue) |
| any other extension | `IconName::File` (chrome fallback) | `muted_foreground` |
| directory, collapsed | `IconName::Folder` (chrome) | `muted_foreground` (**was `overlay`**) |
| directory, expanded | `IconName::FolderOpen` (chrome) | `primary` (blue) |

New SVGs to vendor under `crates/app/assets/file_icons/` (all Seti UI, MIT,
monochrome `fill="currentColor"`): `typescript.svg`, `react.svg`,
`javascript.svg`, `python.svg`, `go.svg`, `ruby.svg`, `java.svg`, `c.svg`,
`cpp.svg`, `yaml.svg`, `html.svg`, `css.svg`, `sass.svg`, `docker.svg`, plus a
corrected `markdown.svg` replacing the mis-named default glyph.

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Explorer polish — v1.1 QA (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] `cargo deny check licenses` passes with no new crate dependency; the
      vendored set's MIT `LICENSE` remains under `crates/app/assets/file_icons/`,
      and a headless guard test loads at least one newly vendored file-type SVG
      through `RiftAssets` and asserts it is `Some` and non-empty (the existing
      `test_gpui_component_icon_asset_is_embedded_in_product_build` still passes).
- [ ] Indent guides: every row at depth `d` renders `d` vertical guide lines on
      the `INDENT_BASE + level * INDENT_PER_LEVEL` lanes via a theme token; a
      `grep` confirms no hardcoded hex was introduced. Legibility of a nested run
      between siblings is confirmed at the QA gate against the Zed reference.
- [ ] Full-row highlight: hover and selection span the full panel width while the
      content keeps its per-depth indent; the cursor row keeps its accent bar +
      `list_active` fill and a multi-selected non-cursor row keeps `secondary`.
      Confirmed at the QA gate (edge-to-edge highlight).
- [ ] Chevron-less disclosure: no chevron/twisty renders on any folder or file
      row; the folder icon shows the open glyph when expanded and the closed
      glyph when collapsed; clicking a folder row still toggles it; rows have
      shifted left by the reclaimed chevron width. Asserted headlessly over the
      folder-state selection; visual shift confirmed at the QA gate.
- [ ] Fully-left root row + whole-tree collapse: the root row renders flush-left
      with no chevron/slot; clicking it hides all root-level entries (empty tree
      body) and clicking again restores the exact prior tree (the per-folder
      `collapsed` set unchanged). Asserted headlessly over the visible-row list
      before/after the toggle.
- [ ] Preserved behavior: keyboard nav (`SelectUp/Down`, `OpenSelected`,
      `Collapse/ExpandOrSelect*`, `SelectFirst/Last`, `Shift`-extend) and the
      header collapse-all/expand-all toggle behave as before (existing phase
      25/27/31 tests carry over green); the header still folds every
      `EntryKind::Dir` while leaving root-level entries visible.
- [ ] Legible folder tints: neither folder glyph resolves to `overlay`; a `grep`
      confirms the `Overlay` tint role is removed and no hardcoded hex was
      introduced; both glyphs are legible at the QA gate against the live sidebar
      surface.
- [ ] Corrected + broadened mapping: `.md` renders a correct markdown glyph (not
      the down-arrow); `.rs/.ts/.tsx/.js/.py/.go/.rb/.java/.c/.cpp/.json/.yaml/
      .toml/.html/.css/.scss/.sh` and `.gitignore`/`LICENSE`/`Dockerfile` render
      their mapped glyphs; an unmapped extension shows `default_file`. Asserted
      headlessly over the pure mapping; visual fidelity confirmed at the QA gate.
- [ ] `grep` confirms no agent detection, no new protocol message, and no change
      under `crates/protocol`, `crates/daemon`, or `crates/explorer` — the change
      is confined to `crates/app` (`file_tree.rs`, `file_icons.rs`, vendored
      `assets/file_icons/`).
- [ ] Milestone QA (dev channel): the explorer reads like the Zed reference —
      fully-left project root, chevron-less folder rows with legible folder icons,
      faint per-level indent guides, full-width row highlight, and recognizable
      per-type file glyphs — while search, context menu, and file ops behave
      exactly as they did in v1.1.0.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Removing the chevron and shifting rows left desynchronizes the trailing git-letter lane alignment | The trailing cluster is unchanged and `flex_shrink_0`; only the leading region shrinks. The row stays a flex row with the name as the single flexible slot, so the lane still right-aligns. Headless slot-order invariants carry over; QA confirms lane alignment. |
| Indent guides misalign with the icon/name indent (drawn on the wrong lanes) | Guides are computed from the same `INDENT_BASE`/`INDENT_PER_LEVEL` constants the content indent uses, so they cannot drift from it. A unit test asserts the per-level lane offsets; QA confirms the guide sits under the disclosure column. |
| The full-width highlight bleeds over the header/root-row or breaks the virtual-list sizing | The highlight is on the row container inside the virtual-list item only; `ROW_HEIGHT` stays the single `const` the size vector reads, so the vector stays correct. QA confirms no bleed. |
| Whole-tree collapse corrupts or is confused with the per-folder `collapsed` set / header collapse-all | `root_collapsed` is a separate flag; the visible-row pass short-circuits to empty while set and never reads/writes `collapsed`. Header collapse-all keeps folding directories. Unit tests cover both toggles independently. |
| A collapsed root row leaves the user with an empty panel and no obvious way back | The root row itself stays visible and clickable (its folder-state affordance reflects collapsed), so re-expanding is one click — the same affordance that collapsed it. QA confirms discoverability. |
| A re-mapped folder tint is still too dark on some surface | The binding rule requires clearing a contrast bar against the live sidebar token, confirmed at the QA gate; `overlay` is forbidden outright. Recommended roles (`muted_foreground`/`primary`) are the light subtext and the bright blue — both clearly legible on `#181825`/`#1e1e2e`. |
| A vendored Seti SVG is multi-color upstream, so the theme tint does not apply | Vendor only monochrome (`fill="currentColor"`) glyphs; flatten any multi-color source at vendor time (Seti's glyphs are monochrome). QA confirms every icon takes the theme tint and re-tints on a theme switch. |
| A per-language tint reads off-palette or clashes | Every tint is a Catppuccin-mapped theme token; the recommended table is the starting point and the exact token is confirmed at the QA gate. |
| Two issues both edit `file_tree.rs` → rebase churn | Split by disjoint seam and sequence: the `render_row` slice (indent guides + full-row highlight + chevron removal) lands first; the root-row / whole-tree-collapse slice (`render_root_row` + `visible_rows` + state) is sequenced after it (`Depends on`). The `file_icons.rs` + assets slice is disjoint and parallelizable. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-09: Spec created from live v1.1.0 QA against Zed's file explorer. Five
  refinements planned as one cohesive batch: (1) per-level indent guide lines,
  (2) full-width row highlight, (3) a Zed-style chevron-less disclosure model —
  fully-left workspace-root row driving whole-tree collapse via a new
  `root_collapsed` flag, and the folder icon (open/closed glyph) as the
  per-folder disclosure with the chevron removed, (4) legible folder-icon tints
  (re-mapped off the `overlay` near-black scrim), and (5) an industry-standard
  file-type icon mapping adopted from Zed's default (Seti-derived) theme, with
  the mis-named markdown down-arrow glyph corrected and additional MIT Seti SVGs
  vendored. Revises Phase 28's disclosure-chevron and `overlay` folder-tint
  decisions and its curated-subset icon scope, and Phase 27's chevron-driven
  root row. Client-only, no protocol/daemon/dependency change; assets stay MIT
  Seti served by the existing `rust-embed`.
- 2026-07-09: Follow-up recorded (not a gate for this batch): the "Explorer —
  Redesign" Paper artboard should be updated to reflect the chevron-less rows,
  fully-left root row, indent guides, full-width highlight, and corrected icon
  mapping, so the artboard stays the accurate visual contract for future
  explorer work.
