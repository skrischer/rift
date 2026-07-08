# Spec: Explorer file-type icons + SVG asset embedding

> Status: READY
> Created: 2026-07-08
> Completed: —

Fill the icon slot Phase 27 reserved: render the "Explorer — Redesign"
artboard's **A Default** file-type and folder icons — language-tinted file
glyphs, folder / open-folder, and a disclosure chevron — replacing today's
neutral placeholder and text-glyph markers, backed by a curated,
license-clean file-type SVG set embedded in the shipping `rift` binary and
mapped to theme-token tints so the icons follow the active theme.

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. This is the **icon** phase of the explorer overhaul (27–31): it
realizes the file-type / folder icons of the artboard's **A Default** column
and its ANATOMY & TOKENS **icon legend**, dropping real glyphs into the
fixed-width icon slot Phase 27 reserved without any re-layout. It does **not**
add a context menu (Phase 29), file operations (Phase 30), or search /
multi-select (Phase 31).

- [ ] The explorer renders a **file-type icon** in the reserved fixed-width
      icon slot of every file row: a distinctive per-extension glyph
      (`.rs`, `.toml`, `.md`, and the curated common repo types) with a
      **generic-file fallback** for any unmapped extension — never a blank
      slot. The glyph is tinted through a **theme token**, so it follows the
      active theme (Catppuccin Mocha today), matching the artboard's icon
      legend (`.rs` peach, `.toml` teal, `.md` info/sky, generic subtext) with
      **no hardcoded hex**.
- [ ] Directory rows render a **folder** glyph in the icon slot: an
      **open-folder** glyph when the directory is expanded and a **closed
      folder** glyph when collapsed, tinted through theme tokens (expanded →
      the artboard's primary role, collapsed → the overlay role). The
      **disclosure chevron** in the chevron/spacer slot is a real chevron glyph
      (right when collapsed, down when expanded), replacing the Phase-27 text
      twisty; a file keeps the same-width blank chevron spacer so names stay
      column-aligned.
- [ ] The two **live header actions** (Collapse-all / Expand-all toggle and
      Reveal-active) and the **workspace-root (`RIFT`) row** render **icon
      glyphs** instead of the Phase-27 Unicode text glyphs, using
      `gpui-component`'s `IconName` set. No new header action is added
      (search/filter and new-file stay absent — Phases 31/30).
- [ ] The shipping `rift` binary **embeds** the file-type SVG assets: they
      render in the cross-compiled release build (windowed, no console), not
      only under the dev-only `gallery` feature. A headless guard test loads a
      bundled file-type SVG through the app's asset source and asserts it is
      present and non-empty (mirroring the existing gpui-component-asset guard).
- [ ] The extension → icon mapping is a **pure, headless-testable** table:
      extension → (glyph, tint role), plus a `default_file` fallback and the
      `default_folder` / `default_folder_open` folder glyphs — the Zed
      icon-theme JSON **shape** as a Rust static (not a JSON file; a single
      bundled set, not user-swappable — see Prior decisions). Unit tests cover
      the mapped extensions, the fallback, and the folder open/closed states.
- [ ] The explorer stays **agent-agnostic** and derives the icon purely from
      the entry's path extension and kind (`EntryKind::File` / `Dir`) plus its
      collapse state — model reads only; **no new protocol message**, no daemon
      change, no agent detection or output parsing. The change is confined to
      `crates/app` (the icon mapping + `file_tree.rs` rendering + the app asset
      source in `main.rs`); `crates/protocol`, `crates/daemon`, and
      `crates/explorer` are untouched.
- [ ] `cargo deny check licenses` continues to pass with **no new crate
      dependency** required (the assets are vendored SVGs served by the
      already-present `rust-embed`); the vendored set's upstream license (MIT)
      ships alongside it for provenance.

## Scope

### In scope

Client-side only, in `crates/app`: the app asset source (`main.rs`), a new
extension → icon mapping module, and the icon rendering in `file_tree.rs`
(`render_row`, `render_root_row`, `render_header`). The binding visual
reference is the Paper **"Explorer — Redesign"** artboard (file `rift`) — its
**A Default** column plus the ANATOMY & TOKENS **icon legend**.

- **Embed the file-type SVG set (app asset source, `main.rs` + vendored
  assets).** Vendor a curated, license-clean set of monochrome file-type SVGs
  under `crates/app/assets/` and register them through a rift-owned
  `AssetSource` that serves rift's own asset paths and **delegates every other
  path** (the `icons/*.svg` gpui-component asks for) to
  `gpui_component_assets::Assets`. Swap `Application::with_assets(Assets)` for
  `with_assets(RiftAssets)` (a single asset source is all GPUI accepts). The
  existing activity-rail / window-control / connection-screen `IconName` glyphs
  keep resolving through the delegate; the existing gpui-component-asset guard
  test stays valid, and a new guard test asserts a rift file-type SVG loads.
- **Extension → icon mapping (new module in `crates/app`, e.g.
  `file_icons.rs`).** A pure function from a row's leaf name / extension and
  kind to (a) the glyph to render (a vendored file-type SVG path, or an
  `IconName` for folder / chevron / generic-file), and (b) the **theme-token
  tint role**. Shape mirrors Zed's icon-theme JSON — `default_file`,
  `default_folder`, `default_folder_open`, and a `file_types` extension map —
  as a Rust static, with a `default_file` fallback so an unmapped extension is
  never blank. Case-insensitive on the extension; a dotfile with no extension
  (e.g. `.gitignore`) may map on its full leaf name.
- **File / folder icons in `render_row` (`file_tree.rs`).** Replace the
  reserved icon slot's Phase-27 neutral placeholder with a `gpui-component`
  `Icon` (`Icon::empty().path(<svg>)` for a vendored file-type glyph, or
  `Icon::new(IconName::…)` for folder / open-folder / generic-file), tinted via
  the mapped theme token. Replace the text twisty in the chevron slot with a
  chevron `Icon` (right/down by collapse state); a file keeps a same-width
  blank chevron spacer. Slot widths stay exactly Phase 27's fixed widths so no
  row re-layout follows and the trailing git-letter lane stays column-aligned.
- **Chrome icons in `render_root_row` + `render_header` (`file_tree.rs`).** The
  `RIFT` root row's disclosure chevron becomes a chevron `Icon` (mirroring
  collapse-all state); the two live header-action `Button`s carry `IconName`
  icons instead of Unicode text labels (keeping their tooltips and `ghost`
  `xsmall` styling). No action is added or removed.
- **Icon-tint → theme-token mapping.** Every tint is an existing theme role,
  never a hex literal: folder-collapsed → overlay, folder-expanded → primary,
  chevron + generic-file → muted-foreground, and the language tints map to the
  semantic/named role whose active Catppuccin-Mocha value matches the artboard
  swatch (see the Prior-decisions mapping table). The icons are monochrome, so
  the theme token is applied as the SVG fill and re-tints automatically on a
  theme switch.

### Out of scope — each its own phase / deliberately deferred

- **User-swappable icon themes (Zed-style icon-theme JSON).** Phase 28 ships a
  **single bundled set** with the mapping authored as a Rust static in the Zed
  JSON *shape*, so a later phase can externalize it to a loadable JSON theme
  without reshaping the table. No icon-theme picker, no per-theme icon
  overrides, no runtime theme file loading here (the open decision recorded in
  `prior-art.md` for Phase 28 is settled to "single bundled set for v1").
- **Context menu — Phase 29.** No right-click actions; the icon change is
  render-only.
- **File operations (create / rename / delete / move) — Phase 30.** Icons do
  not add a write path; the daemon and `protocol` are untouched.
- **Search / filter / quick-open + multi-select — Phase 31.** No filter input,
  no filtered-match icon emphasis, no multi-select icon treatment.
- **A full upstream icon set (hundreds of extensions).** Only a **curated
  subset** covering rift's own repo file types plus the artboard's three is
  vendored; the `default_file` fallback covers the long tail. Expanding the map
  is a later, mechanical follow-up, not this phase.
- **Symlink / submodule / special-kind icons.** The model carries only
  `EntryKind::File` / `Dir`; no new kind is introduced. Icons derive from those
  two kinds plus extension.
- **Decoration, rollup, reveal, keyboard nav, the row cache, density, header
  action set, loading/empty split** — all shipped by Phases 11 / 25 / 27 and
  unchanged. Phase 28 only changes what renders **inside** the already-reserved
  icon and chevron slots (and swaps the chrome text glyphs for icon glyphs); it
  does not touch `compute_rollup`, `refresh_row_cache`, `reveal`, the density
  constants, or the `render()` shell's header/root/tree stack.
- **Protocol / daemon / explorer-crate changes.** Purely client rendering plus
  the app asset source; no wire message, no daemon handler.

## Human prerequisites

None. The change is client-side and self-contained:

- **No new crate dependency.** `rust-embed` (the embedding mechanism) is
  already a direct dependency of `crates/app`, and `gpui-component-assets` is
  already in the product build. The file-type SVGs are **vendored assets**, not
  a crate, so `cargo deny check licenses` (which scans the Cargo graph) is
  unaffected and continues to pass.
- **The vendored icon set is license-clean.** The chosen set (Seti UI icons,
  MIT — the glyphs behind Zed's default icon theme and VS Code's Seti theme) is
  MIT, which is in `deny.toml`'s allowlist; its upstream `LICENSE` is vendored
  alongside the SVGs for provenance. (If the implementer substitutes another
  set, it must be MIT / Apache-2.0 / ISC / CC0 — i.e. in the allowlist — and
  monochrome/tintable; `dmhendricks/file-icon-vectors` is CC-BY-4.0, which is
  **not** in the allowlist and is therefore excluded.)
- The Catppuccin-Mocha theme tokens the tints resolve against are already
  vendored via `gpui-component`; the "Explorer — Redesign" artboard is the
  authored visual reference.

## Constraints

- **Builds on Phase 27's baseline.** Phase 28 assumes Phase 27 (Explorer
  redesign, `spec-explorer-redesign.md`) has shipped: the fixed-width **icon
  slot** and **chevron/spacer slot** in `render_row`, the header action row,
  and the `RIFT` root row already exist. Phase 28 fills the reserved slot and
  swaps the text glyphs for icons — it does **not** re-lay-out the row, change
  slot widths, or alter density. The orchestrator sequences Phase 28's
  milestone after Phase 27's.
- **The icon-asset gap Phase 27 documented is closed** (verified in the
  codebase, 2026-07-08). `crates/app` now takes `gpui-component-assets` as a
  **direct product dependency** and `main.rs` registers it via
  `with_assets(Assets)` for the shipping `rift` binary, guarded by
  `test_gpui_component_icon_asset_is_embedded_in_product_build` (issue #597).
  So gpui-component's Lucide `IconName` glyphs — `Folder`, `FolderOpen`
  (`icons/folder-open.svg`), `File`, `ChevronRight`, `ChevronDown` — already
  render in the release binary and back the **chrome tier** (folder / chevron /
  generic-file) with **no new embedding**. The genuinely-new asset-embedding
  work is confined to the **file-type tier** (distinctive per-language glyphs
  Lucide does not carry). Phase 27's `file_tree.rs` comment that "only the
  dev-only `gallery` binary enables the icon assets" is stale and must be
  removed as part of this phase.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`), never
  hardcoded hex. The artboard quotes exact hex for review legibility; every
  tint maps to an existing theme role whose active value matches the swatch.
  Icons are monochrome so the token applies as the fill and re-tints on a theme
  switch. Layout dimensions (slot widths, icon size) are plain layout pixels,
  as Phase 27 established.
- **No dead controls / no dead visuals** (constitution, Phase-25/27
  precedent). Phase 28 adds no new control — it only re-renders slots that
  already exist. Every unmapped extension resolves to the `default_file`
  fallback, so no row ever renders a blank icon slot.
- **Agent-agnostic** (constitution): the icon is a function of path extension +
  `EntryKind` + collapse state only. No agent detection, no output parsing, no
  process inspection.
- **Reuse `gpui-component` widgets, never fork them** (constitution): icons
  render through the vendored `Icon` element (`Icon::new(IconName)` or
  `Icon::empty().path(<svg>)`); header actions stay `Button` (ghost, xsmall).
  The rift `AssetSource` mirrors `gpui_component_assets::Assets`' own
  rust-embed pattern (`#[folder]` / `#[include]`) and delegates to it — it does
  not replace or fork it.
- **The asset source must delegate, not shadow.** rift's file-type SVGs live
  under a distinct path prefix (e.g. `file_icons/…`), disjoint from
  gpui-component's `icons/*.svg`, so delegation is by prefix with no collision.
  Note `gpui_component_assets::Assets::load` returns `Err` (not `Ok(None)`) on
  a missing path — the delegating source must route by prefix and hand
  gpui-component paths straight through, so the existing `IconName` glyphs keep
  resolving unchanged.
- **No `.unwrap()` in library code**; no `todo!()` in merged code. Extension
  parsing uses total operations (`rsplit('.')` / `Path::extension`) that never
  panic; a name with no extension falls to the fallback.
- **Headless-testable seams.** The extension → icon/tint mapping and the
  folder open/closed selection are pure functions with unit coverage; the app
  asset source is covered by a load-guard test. The visual result (glyph
  legibility, tint fidelity, slot alignment) is validated at the milestone QA
  gate against the artboard's A Default column and icon legend.

## Prior art

Consulted the "Explorer overhaul — prior-art index (Phases 27–31)" in
`prior-art.md`, the shipped Phase-11 / Phase-25 tree, and the Phase-27 redesign
spec.

- **Paper "Explorer — Redesign" artboard (file `rift`)** — the binding visual
  contract. Phase 28 realizes the file-type / folder icons of its **A Default**
  column and the ANATOMY & TOKENS **icon legend** (hex-accurate: `.rs` peach
  `#FAB387`, `.toml` teal `#94E2D5`, `.md` info/sky `#89DCEB`, generic subtext
  `#A6ADC8`, folder expanded primary `#89B4FA` / collapsed overlay `#6C7086`),
  mapped to theme-token roles rather than the literal hex.
- **Zed icon themes** ([docs](https://zed.dev/docs/extensions/icon-themes)) —
  the mapping **shape** reference: `default_file` / `default_folder` /
  `default_folder_open` + a `file_types` extension map. Phase 28 mirrors this
  shape as a Rust static (single bundled set; JSON externalization deferred).
- **`gpui-component` `Icon` + `gpui-component-assets`** (Apache-2.0, vendored)
  — the render widget and the already-embedded Lucide (ISC) chrome glyphs
  (`folder`, `folder-open`, `file`, `chevron-right`, `chevron-down`). Reused
  for the chrome tier; `Icon::empty().path(<svg>)` renders the vendored
  file-type tier.
- **Seti UI icons** (`jesseweed/seti-ui`, MIT) — the bundled file-type glyph
  set (the icons behind Zed's default icon theme and VS Code's Seti theme):
  monochrome, tintable, MIT (allowlisted). A curated subset is vendored.
- **rift-local grounding**: `crates/app/Cargo.toml` (`gpui-component-assets`
  direct dep + `rust-embed` with `debug-embed`), `crates/app/src/main.rs`
  (`with_assets(Assets)` + the asset guard test), `crates/app/src/file_tree.rs`
  (the reserved icon slot + text twisty + text-glyph header actions this phase
  replaces), and `crates/app/src/{activity_rail,connection_screen,settings,
  source_control,diff_view}.rs` (existing `Icon::new(IconName::…)` usage in the
  product build — proof the chrome tier renders without new embedding).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **The chrome tier (chevron, folder, folder-open, generic-file) reuses gpui-component's already-embedded `IconName`; only the file-type tier is newly embedded** | Verified in-codebase: `gpui-component-assets` is a direct product dependency and `main.rs` registers it with `with_assets`, guarded by `test_gpui_component_icon_asset_is_embedded_in_product_build` (#597). The Phase-27 spec's "assets stay unembedded" premise is stale. Lucide carries folder/chevron/file glyphs but **no** per-language file-type glyphs, so the new embedding is narrowed to the file-type set only. | 2026-07-08 |
| **Bundle Seti UI icons (MIT); vendor a curated subset, not the whole set** | Seti is the icon set behind Zed's default icon theme and VS Code's Seti theme — monochrome, tintable, MIT (allowlisted in `deny.toml`). A curated subset covering rift's own repo types plus the artboard's three keeps the binary small and the map readable; the `default_file` fallback covers the long tail. `dmhendricks/file-icon-vectors` is excluded because its CC-BY-4.0 is not allowlisted. | 2026-07-08 |
| **Vendor the SVGs as assets served by the existing `rust-embed`; add no new crate dependency** | `rust-embed` is already a direct `crates/app` dependency. A rift `AssetSource` mirroring `gpui_component_assets::Assets`' rust-embed pattern embeds the vendored SVGs with zero new crates, so `cargo deny check licenses` is unaffected. The upstream MIT `LICENSE` is vendored alongside for provenance. | 2026-07-08 |
| **Single rift `AssetSource` that delegates non-rift paths to `gpui_component_assets::Assets`** | GPUI's `with_assets` accepts exactly one source. A delegating `RiftAssets` serves `file_icons/*.svg` from rust-embed and hands every other path (gpui-component's `icons/*.svg`) straight through, so the activity rail / window controls / connection screen keep resolving and the existing asset guard test stays valid. Delegation is by disjoint path prefix; `Assets::load` returns `Err` on miss, so routing must be by prefix, not by trial-and-error fallthrough. | 2026-07-08 |
| **Single bundled set for v1; user-swappable icon themes deferred** | The `prior-art.md` open decision for Phase 28 (single set vs Zed-style swappable) is settled to a single bundled set — the minimal solution for the current workflow (constitution: no premature abstraction). The mapping is authored in Zed's icon-theme JSON *shape* (`default_file` / `default_folder` / `default_folder_open` / `file_types`) as a Rust static, so a future phase can externalize it to a loadable JSON theme without reshaping. | 2026-07-08 |
| **Icon tints map to theme-token roles, never hex** (mapping table below) | The artboard's icon legend quotes Catppuccin-Mocha hex for review, but every tint must resolve to an existing theme role so icons follow a theme switch (constitution: theme tokens only). Peach/teal/sky have no dedicated named role, so they map to the semantic/named role whose active value matches the swatch — confirmed against the Styleguide at implementation. | 2026-07-08 |
| **Icons derive only from path extension + `EntryKind` + collapse state** | Keeps the explorer agent-agnostic and reads only the existing model — no new protocol, no daemon change, no new `EntryKind`. Symlink/submodule icons are out of scope because the model carries no such kind. | 2026-07-08 |
| **Fill the reserved slot at Phase-27's fixed widths; no re-layout** | Phase 27 reserved the icon and chevron slots at fixed widths precisely so Phase 28 drops glyphs in with no row re-layout. Keeping the widths identical preserves the trailing git-letter lane's column alignment. | 2026-07-08 |

### Icon-tint → theme-token mapping

Binding rule: the tint is the theme token whose active Catppuccin-Mocha value
matches the artboard swatch — never the hex literal. Recommended tokens (all
exist on `gpui-component`'s `ThemeColor`); the file-type language tints are
confirmed against the Styleguide at implementation.

| Slot / type | Artboard legend | Theme-token role |
|---|---|---|
| Folder, collapsed | overlay `#6C7086` | `overlay` |
| Folder, expanded (open) | primary `#89B4FA` | `primary` |
| Disclosure chevron | (neutral) | `muted_foreground` |
| Generic file (`default_file`) | subtext `#A6ADC8` | `muted_foreground` |
| `.rs` | peach `#FAB387` | `warning` (Catppuccin peach) — or named `yellow` if closer to the swatch |
| `.toml` | teal `#94E2D5` | `cyan` (named teal) — confirm against the Styleguide |
| `.md` | info/sky `#89DCEB` | `info` (Catppuccin sky) |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Phase 28 — Explorer file-type icons + SVG asset embedding (created
  at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app (`--features gallery` per the App
      Check job).
- [ ] `cargo deny check licenses` passes with no new crate dependency; the
      vendored icon set's MIT `LICENSE` is present under `crates/app/assets/`.
- [ ] The shipping `rift` binary embeds the file-type SVGs: a headless guard
      test loads a bundled file-type SVG through the app's `AssetSource` and
      asserts it is `Some` and non-empty; the existing
      `test_gpui_component_icon_asset_is_embedded_in_product_build` still passes
      (gpui-component `IconName` glyphs keep resolving through the delegate).
- [ ] File rows render the mapped file-type glyph in the reserved icon slot:
      `.rs`, `.toml`, `.md` show their distinct glyphs; an unmapped extension
      shows the `default_file` fallback (never blank). Asserted headlessly over
      the pure mapping (`ext → glyph`), with the QA gate confirming the visual
      match to the artboard's A Default column + icon legend.
- [ ] Directory rows render an **open-folder** glyph when expanded and a
      **closed-folder** glyph when collapsed; the disclosure chevron is a real
      chevron glyph (right collapsed / down expanded); a file keeps a same-width
      blank chevron spacer so names column-align. Asserted headlessly over the
      folder-state selection; alignment confirmed at the QA gate.
- [ ] Every icon tint resolves to a **theme token** (a `grep` confirms no
      hardcoded hex color literal was introduced in `file_tree.rs` or the icon
      module); switching the theme re-tints the icons.
- [ ] The two live header actions and the `RIFT` root-row chevron render
      `IconName` icons (not Unicode text glyphs); Collapse-all/Expand-all still
      folds every `EntryKind::Dir` and toggles (asserted over `collapsed` /
      `visible_rows` as in Phase 25/27); Reveal-active still fires the reveal
      path; no new header action was added and none removed (a `grep` confirms
      search/filter and new-file remain absent).
- [ ] The stale `file_tree.rs` comment claiming only the `gallery` binary
      embeds icon assets is removed / corrected.
- [ ] `grep` confirms no agent detection introduced, no new protocol message,
      and no change under `crates/protocol`, `crates/daemon`, or
      `crates/explorer` — the change is confined to `crates/app` (icon module +
      `file_tree.rs` + `main.rs` asset source + vendored assets).
- [ ] Milestone QA (dev channel): the explorer reads like the "Explorer —
      Redesign" artboard's **A Default** column — file-type glyphs tinted per
      the icon legend, folders open/closed with the chevron, header + root-row
      icons — while the context menu, file ops, and search remain visibly
      absent (Phases 29–31).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A vendored SVG is multi-color upstream, so the theme tint does not apply and it renders in its baked colors | Vendor only **monochrome** glyphs (single fill inheriting the icon's text color); flatten any multi-color source glyph at vendor time. Seti's glyph SVGs are monochrome. QA confirms every icon takes the theme tint; a theme switch re-tints them. |
| The delegating `RiftAssets` mis-routes gpui-component paths and the activity rail / window controls render blank (a #597 regression) | Route strictly by disjoint prefix: `file_icons/…` served from rust-embed, every other path handed to `gpui_component_assets::Assets`. The existing gpui-component-asset guard test (`icons/folder.svg`) stays as a regression guard; a new test asserts a `file_icons/…` path loads. |
| Icon-slot content shifts the row and breaks the trailing git-letter lane alignment Phase 27 established | Render inside Phase 27's already-reserved fixed-width slot; do not change slot widths or gaps. The icon is size-clamped to the slot. Headless slot-order/width invariants (from Phase 27) carry over; QA confirms lane alignment. |
| The curated subset misses a common repo extension, leaving many rows on the generic glyph | The `default_file` fallback guarantees a legible icon for any extension; the curated set covers rift's own repo types plus the artboard's three. Expanding the map later is mechanical (one static-table edit), not a re-spec. |
| A new pure icon module lands with no consumer and trips clippy `dead_code` (`-D warnings`) | Land the mapping table together with its first consumer (`render_row`) in the same issue, so it is live on merge — the module is not a standalone unused PR. |
| Cross-compiled Windows release reads icons from a compile-time path and renders blank (the original #597 failure mode) | `rust-embed`'s `debug-embed` feature (already enabled) embeds assets in debug builds; the release profile embeds by default. The guard test runs in CI. QA on the dev/stable Windows channel confirms real rendering. |
| Peach/teal tints have no dedicated theme role and a wrong pick reads off-palette | The mapping table records the recommended role and the binding rule ("match the swatch via a token, never hex"); the exact token is confirmed against the Styleguide at implementation and at the QA gate. |
| Two issues both edit `file_tree.rs` → rebase churn | Split by disjoint seam: the row icons (`render_row` + the mapping module) is one issue; the chrome icons (`render_header` + `render_root_row`) is a later issue sequenced after it, so the shared file lands its edits in order. The asset source (`main.rs`) is a disjoint first issue. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 28 — file-type
  icons + SVG asset embedding, the third phase of the explorer overhaul 27–31).
  Key finding: the icon-asset gap Phase 27's spec and `file_tree.rs` document is
  **stale** — `crates/app` already takes `gpui-component-assets` as a direct
  product dependency and `main.rs` registers it via `with_assets(Assets)`
  (guarded by a test, #597), so gpui-component's Lucide `IconName` glyphs render
  in the release binary. This narrows the phase to two tiers: **reuse** the
  embedded chrome glyphs (folder / open-folder / chevron / generic-file) and
  **embed** only the file-type tier (a curated MIT Seti subset, vendored and
  served by the already-present `rust-embed` through a delegating rift
  `AssetSource` — no new crate dependency). Tints map to theme-token roles per
  the icon-legend table; the mapping is authored in Zed's icon-theme JSON shape
  as a Rust static (single bundled set for v1; user-swappable themes deferred).
  Scope held to `crates/app` rendering + the app asset source; no protocol,
  daemon, or explorer-crate change. Realizes the artboard's **A Default** column
  file-type / folder icons and its ANATOMY & TOKENS icon legend.
