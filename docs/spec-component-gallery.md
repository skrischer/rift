# Spec: Component gallery

> Status: DRAFT
> Created: 2026-06-09
> Completed: —

A standalone, in-repo Storybook-style gallery that renders every gpui-component
component against rift's own Catppuccin Mocha theme, so UI work can be previewed
and picked from one place instead of hunting through upstream examples.

## Outcome

What is true when this work is done:

- [ ] A `gallery` binary launches a GPUI window showing a searchable sidebar of
      component entries and a content pane that renders the selected component's
      demo, all themed with rift's active Catppuccin Mocha theme.
- [ ] Coverage is comprehensive: every component the upstream gpui-component
      gallery exposes that builds on rift's targets has an entry — including the
      exotic three (chart, code editor, webview) and tables — each with a small
      self-authored demo (no upstream story files ported).
- [ ] A **Theme tokens** entry renders the active theme's `ThemeColor` swatches
      with their names, so colors can be matched during UI work.
- [ ] The gallery's heavy/optional dependencies (tree-sitter, webview, demo data)
      are gated behind a `gallery` cargo feature so the shipping `rift` binary's
      build is unaffected.
- [ ] `just gallery` builds and runs the gallery on the GPU station.
- [ ] `cargo deny check licenses` still passes.

## Scope

### In scope

- A second binary target `gallery` in `crates/app` (`[[bin]] name = "gallery"`,
  `required-features = ["gallery"]`), with its view code in a dedicated module
  (`crates/app/src/gallery/`), kept out of `main.rs`.
- A trimmed gallery shell composed from real gpui-component primitives
  (`Sidebar` + `SidebarGroup`/`SidebarMenu`, `h_resizable`, `Input` search) —
  modeled on upstream `crates/story/src/gallery.rs` but without the
  `StoryContainer`/`Story`-trait/dock-state machinery.
- A flat in-crate registry of component entries: `(name, description,
  render_fn)`. Each `render_fn` is a small, self-contained demo authored in rift,
  showing the variants worth previewing.
- Comprehensive component coverage, explicitly including chart, code editor,
  webview, and table/data-table (with inline static demo data).
- A **Theme tokens** entry that enumerates `cx.theme()` `ThemeColor` swatches.
- A `gallery` cargo feature on `crates/app` that turns on the gpui-component
  optional features the exotic components need (e.g. `tree-sitter-languages`,
  webview) and any demo-only deps; the `gallery` binary declares it via
  `required-features`.
- Reuse of the Catppuccin theme setup: the `apply_theme` logic currently inline
  in `main.rs` is extracted so both binaries register and activate the same theme.
- A `just gallery` recipe mirroring the `dev-windows` launch pattern, building and
  running the `gallery` binary with `--features gallery`.

### Out of scope

- Wiring the gallery into the running `rift` app (no in-app keybinding/tab/toggle)
  — it is a standalone dev window.
- A runtime multi-theme switcher / theme editor. The gallery renders in the single
  active app theme (Catppuccin Mocha); a Light/Dark toggle and theme switching stay
  deferred to a future theming spec (consistent with the gpui-component-adoption
  decision to defer the switcher).
- Porting upstream story files verbatim, and pulling upstream's demo-data deps
  (`fake`, `csv`, `autocorrect`, `rust-i18n`); demos use inline static data.
- Interactive controls/“knobs” to mutate component props at runtime (Storybook
  args panel). Demos are static compositions.
- Persisting selected-story/panel state across launches (upstream's `StoryState`).
- Any change to the production `rift` binary's behavior or chrome.

## Constraints

- **Single GPU-exclude knob.** Agents build headless with
  `cargo build --workspace --exclude rift-app`; CI splits the heavy GPU build into
  the `app-check` job (`cargo check -p rift-app`). Putting the gallery as a second
  binary *inside* `crates/app` keeps `--exclude rift-app` as the one switch that
  skips all GPU builds. A separate `crates/gallery` crate would add a second
  exclude target everywhere — rejected for that reason.
- **A second binary shares the crate's manifest dependency set.** Cargo compiles a
  dependency once per crate regardless of which binary uses it, so naively adding
  webview/tree-sitter deps to `crates/app` would pull them into the `rift` product
  build too. They are therefore declared `optional` and pulled only under the
  `gallery` feature, with the binary gated by `required-features = ["gallery"]`.
  `cargo build/check -p rift-app` (no feature) stays clean; `cargo build
  --workspace` skips the `gallery` binary because its required feature is off.
- **`gallery` is a feature for an implemented optional dev tool**, not for an
  unimplemented feature — so it does not violate the "no feature flags for
  unimplemented features" rule. The flag exists to keep the exotic deps off the
  product build, not to hide half-built code.
- **Theme parity.** gpui-component widgets only render in rift's palette if the
  Catppuccin theme is registered and activated (the lesson from PR #34). The
  gallery must run the same `apply_theme` path as `main.rs`, so that logic is
  shared, not duplicated.
- **Assets.** Icon-bearing components need `gpui-component-assets::Assets`; the
  gallery binary loads them via `application().with_assets(Assets)` (the product
  binary currently loads none). `gpui-component-assets` is already in `Cargo.lock`
  transitively.
- **GPUI single-rev rule (unchanged).** All consumers bare-track one zed rev pinned
  by `Cargo.lock`; the gallery adds no new GPUI source. Enabling extra
  gpui-component features must not pull a second `gpui`.
- **License.** Any feature/dep enabled for the gallery must keep
  `cargo deny check licenses` green (gpui-component is Apache-2.0).
- **No `.unwrap()` in library code; binaries use `anyhow`/`.expect("reason")` for
  true invariants** (project rule).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Build rift's own trimmed gallery, not depend on upstream `gpui-component-story` | The `story` crate is `publish=false`, path-only, and pulls heavy non-UI deps (reqwest, smol, gtk, tree-sitter, fake, csv, color-lsp, i18n); depending on it violates the minimal-dependency policy. The gallery shell is ~100 lines of gpui-component composition. | 2026-06-09 |
| Reuse upstream `gallery.rs` layout (Sidebar + h_resizable + Input search), drop the `StoryContainer`/`Story`-trait/dock machinery | That machinery exists for dockable, persisted, zoomable panels rift's gallery does not need; a flat `(name, description, render_fn)` registry is enough. | 2026-06-09 |
| Comprehensive coverage including the exotic three (chart, code editor, webview) and tables | User decision: "alle core components … komplett inkl. exotisch". | 2026-06-09 |
| Demos are self-authored with inline static data | Avoids porting 41KB upstream story files and pulling `fake`/`csv`/`i18n`; keeps each demo readable and dependency-light. | 2026-06-09 |
| Gallery lives as a second `[[bin]]` in `crates/app`, launched via `just gallery` | User decision; reuses the heavy GPU `target/`, keeps `--exclude rift-app` as the single GPU-exclude knob, and stays out of the `rift` product binary. | 2026-06-09 |
| Exotic/demo deps gated behind a `gallery` cargo feature + `required-features` on the binary | A second binary shares the crate manifest's dep set; gating keeps the product `rift` build free of webview/tree-sitter. The feature gates an implemented tool, not unfinished code. | 2026-06-09 |
| Single active theme (Catppuccin Mocha); no runtime theme switcher | Consistent with the gpui-component-adoption decision deferring a multi-theme switcher to its own spec. A Theme-tokens reference page covers the "match colors for UI work" need. | 2026-06-09 |
| Extract `apply_theme` so both binaries share it | Same-theme parity is a hard requirement (PR #34 lesson); duplication would drift. | 2026-06-09 |

## Tracking

The decomposition into steps lives as GitHub issues under a milestone, one issue per
step. This spec owns the design; the issues own progress.

- Milestone: [Component gallery](<milestone-url>) — created once this spec is `READY`
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that
traces back here (planning gate).

## Verification

How the entire spec is known complete:

- [ ] `cargo clippy --workspace -- -D warnings` passes (product crates, gallery off).
- [ ] `cargo clippy -p rift-app --features gallery -- -D warnings` passes (gallery on).
- [ ] `cargo test --workspace` passes; a unit test asserts the component registry is
      non-empty and entry names are unique.
- [ ] `cargo check -p rift-app` (no `--features gallery`) does **not** pull the
      gallery's optional deps — verified by it building unchanged and the product
      `rift` binary's dep graph being unaffected.
- [ ] `cargo deny check licenses` passes with the `gallery` feature enabled.
- [ ] `just gallery` launches a window on the GPU station: sidebar lists every
      covered component grouped sensibly; typing in the search box filters entries;
      selecting an entry renders its demo in rift's Catppuccin palette.
- [ ] The exotic entries (chart, code editor, webview) and a table entry render
      without panicking, or — where a platform webview/runtime is unavailable on the
      build target — degrade to a visible "not available on this platform" notice
      while the gallery still builds and runs (see risks).
- [ ] The **Theme tokens** entry renders the active theme's color swatches with names.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Webview/code-editor pull platform runtimes (WebView2 on `x86_64-pc-windows-gnu`, webkit2gtk on Linux/X11) that don't cross-compile cleanly | Gate them behind the `gallery` feature so the product build is never affected; if an entry cannot build on a target, ship it as a sidebar entry that renders a "not available on this platform" notice and split the real demo into a follow-up issue — the gallery as a whole must still build and run. |
| Exotic deps drag a second `gpui` or a license violation | Verify exactly one `gpui` in `Cargo.lock` and `cargo deny check licenses` green with the feature on, before close. |
| Second-binary dep bleed into the `rift` product build | `optional` deps + `required-features`; verification explicitly checks `cargo check -p rift-app` (feature off) is unaffected. |
| Gallery rots as gpui-component's API drifts | The GPU station builds it via `just gallery`; optionally extend CI `app-check` with `cargo check -p rift-app --features gallery` if the runner can provide the webview/tree-sitter system libs (otherwise the station is the gate). |
| Scope creep into a theme editor / interactive knobs | Both explicitly out of scope; this spec ships a static, single-theme gallery. |

## Decision log

- 2026-06-09: Spec created. Both genuinely-open decisions resolved up front with the
  user before drafting: coverage = **complete incl. exotic** (chart, code editor,
  webview); location = **second binary in `crates/app`**. The resulting
  second-binary-shares-manifest-deps tension is resolved by a `gallery` cargo
  feature + `required-features` so the product `rift` build stays clean.
