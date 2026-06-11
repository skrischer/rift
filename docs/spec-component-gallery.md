# Spec: Component gallery

> Status: READY
> Created: 2026-06-09
> Completed: —

A standalone, in-repo Storybook-style gallery that renders the gpui-component
components against rift's own Catppuccin Mocha theme, so UI work can be previewed
and picked from one place instead of hunting through upstream examples.

## Outcome

What is true when this work is done:

- [ ] A `gallery` binary launches a GPUI window showing a searchable sidebar of
      component entries and a content pane that renders the selected component's
      demo, all themed with rift's active Catppuccin Mocha theme.
- [ ] Coverage is comprehensive: every component the upstream gpui-component
      gallery exposes that lives **in the `gpui-component` library** has an entry —
      including chart, code editor, and tables — each with a small self-authored
      demo (no upstream story files ported). WebView (a separate crate) is
      represented by a placeholder entry and delivered by a follow-up issue.
- [ ] A **Theme tokens** entry renders the active theme's `ThemeColor` swatches
      with their names, so colors can be matched during UI work.
- [ ] The gallery binary is opt-in: it and its extra demo-only dependencies are
      gated behind a `gallery` cargo feature, so default workspace builds and the
      shipping `rift` binary are unaffected.
- [ ] `just gallery` builds and runs the gallery on the GPU station.
- [ ] `cargo deny check licenses` still passes with the `gallery` feature on.

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
- Comprehensive coverage of the gpui-component **library** components, explicitly
  including chart, code editor, and table/data-table (with inline static demo
  data).
- A **WebView placeholder** entry that renders a "delivered by follow-up issue"
  notice, so the gallery's component map is complete while the real WebView demo
  is split out (see Out of scope / Decision log).
- A **Theme tokens** entry that enumerates `cx.theme()` `ThemeColor` swatches.
- A `gallery` cargo feature on `crates/app` that (a) gates the `gallery` binary via
  `required-features` so default `cargo build --workspace` skips it, and (b) turns
  on the gpui-component features the code-editor / chart demos need — a single
  syntax grammar for the editor demo (e.g. `tree-sitter-rust`, not the full
  `tree-sitter-languages` set) and `decimal` only if a chart/table demo uses
  decimal axes.
- `gpui-component-assets` added as a direct dependency of `crates/app` so the
  gallery binary can load icon assets via `application().with_assets(Assets)`.
- Reuse of the Catppuccin theme setup: the `apply_theme` logic currently inline
  in `main.rs` is extracted so both binaries register and activate the same theme.
- A `just gallery` recipe mirroring the `dev-windows` launch pattern, building and
  running the `gallery` binary for `x86_64-pc-windows-gnu` with `--features
  gallery`.

### Out of scope

- **The real WebView demo.** WebView is not a gpui-component feature — it is a
  separate crate (`gpui-wry` / Wry) that is not in rift's `Cargo.lock`, floats its
  own `gpui` (a single-rev convergence task), and has an unproven cross-compile to
  `x86_64-pc-windows-gnu`. It is deferred to its own follow-up issue; this spec
  ships only the placeholder entry and keeps `Cargo.lock` on exactly one `gpui`.
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
- Any change to the production `rift` binary's behavior, chrome, or dependency
  graph.

## Constraints

- **Single GPU-exclude knob.** Agents build headless with
  `cargo build --workspace --exclude rift-app`; CI splits the heavy GPU build into
  the `app-check` job (`cargo check -p rift-app`). Putting the gallery as a second
  binary *inside* `crates/app` keeps `--exclude rift-app` as the one switch that
  skips all GPU builds. A separate `crates/gallery` crate would add a second
  exclude target everywhere — rejected for that reason.
- **A second binary shares the crate's manifest dependency set.** The `gallery`
  binary is therefore gated by `required-features = ["gallery"]`, and the
  extra demo-only gpui-component features (editor grammar, optional `decimal`) are
  pulled only under that feature. `cargo build/check -p rift-app` (no feature) and
  `cargo build --workspace` (feature off) do not compile the gallery binary or its
  extra deps, so the product `rift` build is unaffected.
- **tree-sitter core is already in the product graph; only the grammar is gated.**
  `gpui-component` pulls `tree-sitter` (and `tree-sitter-json`) as *non-optional*
  deps today, so the shipping `rift` binary already contains tree-sitter core. The
  `gallery` feature adds only the *extra editor grammar* (one language) — not
  tree-sitter itself. Spec prose must not claim the product build is "free of
  tree-sitter".
- **`gallery` is a feature for an implemented optional dev tool**, not for an
  unimplemented feature — so it does not violate the "no feature flags for
  unimplemented features" rule. It gates an opt-in binary and its demo deps, not
  half-built code.
- **Build/run target.** The gallery mirrors the product app's primary dev loop
  (`just dev-windows`): its sign-off target is `x86_64-pc-windows-gnu`, run on the
  GPU station (the Windows host). Running natively on Linux/X11 is a non-required
  bonus (the components are platform-agnostic GPUI) and is not part of the
  done-criteria.
- **Theme parity.** gpui-component widgets only render in rift's palette if the
  Catppuccin theme is registered and activated (the lesson from PR #34). The
  gallery must run the same `apply_theme` path as `main.rs`, so that logic is
  shared, not duplicated.
- **Assets.** `gpui-component-assets` is already a direct dependency of
  `gpui-component` (so it is in the product graph already); the gallery adds it as
  a *direct* dependency of `crates/app` only to name the `Assets` type for
  `with_assets(Assets)` — this adds no new compilation to the product build.
- **GPUI single-rev rule (unchanged).** All consumers bare-track one zed rev pinned
  by `Cargo.lock`; this spec adds **no new GPUI source** (WebView's floating-`gpui`
  crate is deferred). `Cargo.lock` must keep exactly one `gpui` entry.
- **License.** Any feature/dep enabled for the gallery must keep
  `cargo deny check licenses` green (gpui-component is Apache-2.0). The extra editor
  grammar crate is the only new dependency this spec adds.
- **No `.unwrap()` in library code; binaries use `anyhow`/`.expect("reason")` for
  true invariants** (project rule).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Build rift's own trimmed gallery, not depend on upstream `gpui-component-story` | The `story` crate is `publish=false`, path-only, and pulls heavy non-UI deps (reqwest, smol, gtk, tree-sitter languages, fake, csv, color-lsp, i18n); depending on it violates the minimal-dependency policy. The gallery shell is ~100 lines of gpui-component composition. | 2026-06-09 |
| Reuse upstream `gallery.rs` layout (Sidebar + h_resizable + Input search), drop the `StoryContainer`/`Story`-trait/dock machinery | That machinery exists for dockable, persisted, zoomable panels rift's gallery does not need; a flat `(name, description, render_fn)` registry is enough. | 2026-06-09 |
| Comprehensive coverage of gpui-component **library** components incl. chart, code editor, tables | User decision: "alle core components … komplett". | 2026-06-09 |
| WebView deferred to a follow-up issue; placeholder entry only | Review found WebView is not a gpui-component feature but a separate `gpui-wry`/Wry crate (not in lock, floats its own gpui, unproven windows-gnu cross-compile). Deferring keeps this spec at one `gpui` and low-risk. User decision at the review gate. | 2026-06-09 |
| Demos are self-authored with inline static data | Avoids porting 41KB upstream story files and pulling `fake`/`csv`/`i18n`; keeps each demo readable and dependency-light. | 2026-06-09 |
| Editor demo enables a single grammar (e.g. `tree-sitter-rust`), not the full `tree-sitter-languages` set | Minimizes the grammar blast radius (~35 crates) while still demoing syntax highlighting; rust is the natural choice for a Rust project. | 2026-06-09 |
| Gallery lives as a second `[[bin]]` in `crates/app`, launched via `just gallery` | User decision; reuses the heavy GPU `target/`, keeps `--exclude rift-app` as the single GPU-exclude knob, and stays out of the `rift` product binary. | 2026-06-09 |
| Extra demo deps + the gallery binary gated behind a `gallery` cargo feature + `required-features` | A second binary shares the crate manifest's dep set; gating keeps the product `rift` build and default workspace builds unaffected. The feature gates an implemented tool, not unfinished code. | 2026-06-09 |
| Single active theme (Catppuccin Mocha); no runtime theme switcher | Consistent with the gpui-component-adoption decision deferring a multi-theme switcher to its own spec. A Theme-tokens reference page covers the "match colors for UI work" need. | 2026-06-09 |
| Extract `apply_theme` so both binaries share it | Same-theme parity is a hard requirement (PR #34 lesson); duplication would drift. | 2026-06-09 |
| Sign-off target is `x86_64-pc-windows-gnu` (the GPU station's `dev-windows` loop) | The gallery is previewed where the product app is previewed; Linux/X11 native is a non-required bonus. | 2026-06-09 |

## Tracking

The decomposition into steps lives as GitHub issues under a milestone, one issue per
step. This spec owns the design; the issues own progress.

- Milestone: [Component gallery](<milestone-url>) — created once this spec is `READY`
- Issues: created from this spec once it is `READY` (one per implementable step),
  including a follow-up issue for the real WebView demo.

Each issue references this spec path. A PR may only merge if it closes an issue that
traces back here (planning gate).

## Verification

How the entire spec is known complete:

- [ ] `cargo clippy --workspace -- -D warnings` passes (product crates, gallery off).
- [ ] `cargo clippy -p rift-app --features gallery -- -D warnings` passes (gallery on).
- [ ] `cargo test --workspace` passes; a unit test asserts the component registry is
      non-empty and entry names are unique.
- [ ] `cargo check -p rift-app` (no `--features gallery`) builds unchanged and the
      product `rift` binary's dependency graph is unaffected (no new deps; still
      exactly one `gpui` in `Cargo.lock`).
- [ ] `cargo deny check licenses` passes with the `gallery` feature enabled.
- [ ] `just gallery` builds for `x86_64-pc-windows-gnu` and launches a window on the
      GPU station: the sidebar lists every covered component grouped sensibly;
      typing in the search box filters entries; selecting an entry renders its demo
      in rift's Catppuccin palette.
- [ ] The chart, code editor, and table entries render without panicking; the
      WebView entry renders its "delivered by follow-up issue" placeholder.
- [ ] The **Theme tokens** entry renders the active theme's color swatches with
      names.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The code-editor grammar / chart demos drag platform or heavy deps into the product build | All gated behind the `gallery` feature; verification checks `cargo check -p rift-app` (feature off) is unaffected and `Cargo.lock` keeps one `gpui`. |
| The editor demo's grammar set balloons (~35 crates) | Enable a single grammar (`tree-sitter-rust`) rather than the umbrella `tree-sitter-languages` feature. |
| Chart/table demos need `rust_decimal` (gpui-component `decimal` feature) | Enable `decimal` under the `gallery` feature only if a demo uses decimal axes; otherwise omit. |
| WebView follow-up reintroduces a second `gpui` via `gpui-wry` | Out of scope here; the follow-up issue owns proving `gpui-wry` converges on rift's pinned zed rev and the `x86_64-pc-windows-gnu` cross-compile before adding it. |
| Gallery rots as gpui-component's API drifts | CI `app-check` now builds it: the skeleton step (#123) added `cargo clippy -p rift-app --features gallery --all-targets -- -D warnings` and `cargo test -p rift-app --features gallery` to the job, so the gallery feature compiles on every push and drift fails CI; the GPU station's `just gallery` stays the visual gate. (The editor-grammar demos may still need station-side checking if the CI runner lacks their system libs.) |
| Scope creep into a theme editor / interactive knobs | Both explicitly out of scope; this spec ships a static, single-theme gallery. |

## Decision log

- 2026-06-09: Spec created. Two genuinely-open decisions resolved up front with the
  user before drafting: coverage = **complete** (chart, code editor, tables);
  location = **second binary in `crates/app`**. The second-binary-shares-manifest-
  deps tension is resolved by a `gallery` cargo feature + `required-features` so the
  product `rift` build stays clean.
- 2026-06-09: Review gate (in-session Agent review of PR #118) corrected three
  factual errors and the spec was revised before flipping to `READY`:
  - **WebView is not a gpui-component feature** — it is a separate `gpui-wry`/Wry
    crate (not in lock, floats its own `gpui`, unproven windows-gnu cross-compile).
    Resolved with the user: **defer WebView to a follow-up issue**; ship a
    placeholder entry now. This keeps `Cargo.lock` at exactly one `gpui`.
  - **tree-sitter core is already non-optional** in the product graph via
    `gpui-component`; the `gallery` feature gates only the *extra editor grammar*
    (one language), not tree-sitter itself. Overclaiming prose corrected.
  - **Build/run target pinned** to `x86_64-pc-windows-gnu` (the GPU station's
    `dev-windows` loop), making the cross-compile-dependent outcomes verifiable.
  - Non-blocking review notes folded in: `gpui-component-assets` is a direct dep of
    `gpui-component` already (named directly only for `with_assets`); single grammar
    instead of the full set; `decimal` enabled only if a chart/table demo needs it.
- 2026-06-10: Skeleton step (#123, PR #130) merged. Implementation decision: CI
  `app-check` now gates the `gallery` feature directly, rather than leaving the GPU
  station as the only build gate — it runs `cargo clippy -p rift-app --features
  gallery --all-targets -- -D warnings` and `cargo test -p rift-app --features
  gallery` next to the feature-off `cargo check -p rift-app`. This resolves the
  Risk-table "optionally extend app-check" item to **done**. The `--all-targets`
  clippy run earned its keep immediately, catching a `#[test]`-macro recursion (the
  `gpui::*` glob pulls gpui's `test` attribute macro into scope, shadowing the
  built-in `#[test]`) that a plain `cargo check` would have missed; the fix narrows
  the test module's imports to just the symbols it uses.
- 2026-06-10: Demos part 1 (#124, PR #141) merged — Theme-tokens swatches plus the
  form/input and feedback component demos. Two implementation decisions:
  - **Registry shape: a `Demo` enum, not a flat `render_fn`.** The spec sketched the
    registry as `(name, description, render_fn)` where `render_fn` is a plain
    `fn(&mut Window, &mut App) -> AnyElement`. That holds for stateless demos, but
    several gpui-component widgets (`Input`, `OtpInput`, `Select`, `Combobox`, the
    `Form`, `Slider`) require a persistent `Entity<…State>` that a bare function
    pointer cannot own across frames. Resolved by splitting the demo column into
    `Demo::Element(fn(&mut Window, &mut App) -> AnyElement)` (stateless, rebuilt each
    frame) and `Demo::View(fn(&mut Window, &mut App) -> AnyView)` (stateful, built
    once and cached in `Gallery.views`). This keeps the flat registry the spec wanted
    while honouring gpui's state-entity model; it is the registry's intended growth
    point for the exotic demos (#126) too.
  - **Icons in debug builds need rust-embed `debug-embed`.** Visual review on the GPU
    station found icons, the spinner, and the rating stars rendering blank.
    `gpui-component-assets`' `Assets` uses `#[derive(RustEmbed)]`, which in debug
    builds (without the `debug-embed` feature) reads SVGs from the *compile-time*
    filesystem path at *runtime*. Cross-compiled to `x86_64-pc-windows-gnu` and run
    on the Windows host, that Linux path does not exist, so every icon-backed widget
    is empty. Fixed by enabling rust-embed's `debug-embed` feature via cargo feature
    unification — adding `rust-embed = { …, features = ["debug-embed"] }` as an
    optional dep under the `gallery` feature (not used in code; declared only to flip
    the feature). No new crate enters the graph (`rust-embed` is already a transitive
    dep), `Cargo.lock` keeps one `gpui` and one `rust-embed`, and the product `rift`
    build is untouched. User-approved at the visual gate.
- 2026-06-11: Demos part 2 (#125, PR #146) merged — the layout, navigation, overlay
  and picker demos, completing the in-library component coverage across parts 1+2
  (the exotic chart/editor/table set and WebView stay on #126/#127). Three
  implementation decisions:
  - **Sidebar taxonomy is rift's own.** The spec sketched a flat sidebar, and
    gpui-component's own gallery offers no finer grouping (just "Getting Started"
    plus one flat "Components" list), so there was nothing upstream to adopt. With
    the registry now at 51 entries the flat list was unreadable, so a static
    `GROUPS` name-table groups them into eight categories (Theme; Forms & Input;
    Feedback & Status; Layout; Navigation; Data Display; Overlay; Pickers). Kept as
    static labelled groups rather than interactive collapsible ones — gpui-component's
    `SidebarGroup` exposes a static `collapsed` flag but no built-in click-to-toggle
    header, so collapsibility would be net-new shell work outside this issue; the
    user settled on grouping-only. Two bijection tests (`test_every_entry_is_grouped`,
    `test_group_names_exist_in_registry`) keep the table and the registry in sync.
  - **Overlay layers must be rendered by the root view.** Dialog/Sheet/Notification
    state lives on the window's `Root`, but those layers only paint if the root view
    composes `Root::render_sheet_layer` / `render_dialog_layer` /
    `render_notification_layer` into its tree. The part-1 gallery never did, so the
    dialog and sheet demos opened invisibly (state flipped, nothing drawn) and
    part-1's notification toasts never appeared. Fixed by composing the three layers
    into `Gallery::render`, which also retroactively repaired the part-1
    notifications. Mirrors gpui-component's own gallery shell. Surfaced at the visual
    gate, where the modal "did not open" on click.
  - **Offline demos avoid network and date deps.** The Image demo embeds inline
    vector tiles rather than exercising `img()` over HTTP (the build has no http
    client and the gallery must run offline); Calendar and DatePicker are created
    without a seeded date to avoid pulling a direct `chrono` dependency just to name
    one. No new crates enter the graph.
- 2026-06-11: Exotic demos (#126, PR #180) merged — the chart, code-editor, static
  table and delegate-backed data-table demos plus the WebView placeholder, closing
  the in-library coverage (only the real WebView demo, #127, remains). Four
  implementation decisions, each resolving a Risk-table item:
  - **Charts run on `f64`, so the `decimal` feature stays off.** The Risk table left
    `decimal` conditional on "if a chart/table demo uses decimal axes". None do: the
    chart value bound is `V: Copy + PartialOrd + Num + ToPrimitive + Sealed`, and
    `impl Sealed for f64` is unconditional in gpui-component's `plot/scale/sealed.rs`
    — only `rust_decimal::Decimal` is gated behind `decimal`. So plain `f64` data
    works and `decimal` is deliberately **not** enabled. Risk-table item resolved to
    "not needed".
  - **A single `tree-sitter-rust` grammar via feature passthrough.** The `gallery`
    feature adds `gpui-component/tree-sitter-rust` (a real, distinct feature), not the
    umbrella `tree-sitter-languages` set (~35 grammar crates). `Cargo.lock` gains only
    `tree-sitter-rust 0.24.2` (plus its `cc`/`tree-sitter-language` deps) and keeps
    exactly one `gpui`; the product `rift` build is untouched. Resolves the
    "grammar-set balloons" risk.
  - **Stateless vs. stateful split per the part-1 `Demo` registry.** Chart and the
    static `Table` are `Demo::Element` (purely declarative, rebuilt each frame); the
    Code Editor (`InputState`) and Data Table (`TableState<…>`) are `Demo::View` so
    their widget-state entities survive across frames — the same rule the form/input
    demos established.
  - **WebView ships as a visible placeholder, not a hidden entry.** The entry renders
    an `Alert` that names the deferral and points at follow-up #127, so the gallery's
    component map reads as complete while the real demo is split out. It is not a
    forbidden mechanism (no cargo flag for an unimplemented feature, no `todo!()`),
    and keeps `Cargo.lock` at one `gpui`. The new single-entry **Embedded** sidebar
    group holds it; Code Editor joined **Forms & Input** and Chart/Table/Data Table
    joined **Data Display**. The spec stays `READY` (not `COMPLETED`) because #127
    still traces to it.
