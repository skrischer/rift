# Spec: gpui headless renderer (Linux/WSLg)

> Status: READY
> Created: 2026-07-08
> Completed: —

Implement the one missing `PlatformHeadlessRenderer` impl for Linux/WSLg
(offscreen wgpu texture + GPU→CPU readback) via an **additive `[patch]` fork** of
`gpui` on the frozen `4bee412` base, so `HeadlessAppContext::capture_screenshot`
produces a real PNG off-macOS. This is Phase 1 of the "Visual UI harness" track —
the enabler the Phase-2 harness (drive + capture + Paper diff) builds on; it ships
no harness itself.

## Outcome

What is true when this work is done:

- [ ] `gpui_platform::current_headless_renderer()` returns `Some(...)` on Linux
      (was `None`); a `HeadlessAppContext` built with it opens a window and
      `capture_screenshot` returns a **non-blank** `RgbaImage` of the rendered
      scene.
- [ ] The renderer is an **additive** change on the frozen `gpui` base
      (`4bee412`): a `WgpuHeadlessRenderer` implementing the two
      `PlatformHeadlessRenderer` trait methods (`render_scene_to_image` +
      `sprite_atlas` — the trait has exactly these two at `4bee412`), plus the
      `current_headless_renderer()` Linux arm — no other `gpui` API changes.
- [ ] rift consumes the fork via a `[patch]` redirect; `Cargo.lock` resolves to
      **exactly one** `gpui` and one source per zed-sourced crate (the
      single-`gpui`-invariant trial is green).
- [ ] `just ci` is green (headless workspace, excl. `rift-app`); `rift-app`
      builds unchanged on the GPU station (App Check green).
- [ ] The render path is confirmed to produce a **visually correct** image of a
      known rift or gallery view — via lavapipe-in-WSL or the GPU station,
      whichever the render probe establishes.

## Scope

### In scope

- In the `skrischer/zed` fork (branch off `4bee412`): a `WgpuHeadlessRenderer`
  in `gpui_wgpu` implementing `gpui::PlatformHeadlessRenderer`'s two methods
  (`render_scene_to_image` = offscreen texture render + `copy_texture_to_buffer`
  readback + BGRA→RGBA; `sprite_atlas`), mirrored from `gpui_macos`'s
  `MetalHeadlessRenderer` / `render_scene_to_image`; and the
  `current_headless_renderer()` Linux arm returning `Some(...)`.
- In rift: the `[patch."https://github.com/zed-industries/zed"]` redirect to the
  fork, the regenerated `Cargo.lock`, and the single-`gpui`-invariant trial
  (throwaway worktree) that proves the graph still unifies.
- A minimal rift-side smoke test (hosted in `rift-terminal` — a gpui-consuming
  crate in the headless workspace, not `rift-app`; the rev-bump built/tested it
  headlessly): build a `HeadlessAppContext`, render a trivial known view, assert
  `capture_screenshot` yields a non-blank PNG.

### Out of scope

- **The Phase-2 harness** — snapshot registry, `TestAppContext` driving of real
  rift views, Paper-MCP diff, CI pixel baseline. Separate phase, gated on this one.
- **The Windows (D3D11) headless renderer** — deferred (see Prior decisions;
  gate-confirmed). Linux/WSLg is the headless-loop environment this unblocks.
- **macOS** — already has `MetalHeadlessRenderer`.
- **A rift PR for the fork commit** — the wgpu-renderer commit lands in the
  `skrischer/zed` repo (the milestone's first issue creates the fork + writes +
  pushes it), not as a rift PR; rift's PRs cover only the `[patch]` +
  `Cargo.lock` + smoke test that consume it. (The gate chose in-loop autonomous
  creation over the rev-bump's human-prerequisite model.)
- Any `gpui` **rev bump** — the base stays `4bee412` (the rev bump is NO-GO,
  `archive/spec-gpui-rev-bump.md`); this is purely additive on it.

## Constraints

- **Single-`gpui` invariant (constitution).** `Cargo.lock` must hold exactly one
  `gpui`. Because the fork is additive on the *same* commit every consumer
  already pins (`4bee412`), the `gpui` API is byte-for-byte compatible for
  `gpui-component` and `termy_terminal_ui`. Note `[patch]` is **per crate name**,
  not per URL: the redirect needs one entry each for `gpui`, `gpui_platform`, and
  every other zed-sourced crate the graph pulls (`http_client`, `util`, … — the
  rev-bump enumerated the grown set); miss one and it resolves bare from upstream
  zed and re-splits the graph. The trial greps one source per zed-sourced crate
  to verify the invariant holds.
- **`[patch]` is cross-source, not same-source.** The existing Cargo.toml comment
  notes a *same-source* `[patch]` is rejected by Cargo; redirecting
  `zed-industries/zed` → `skrischer/zed` is the standard fork mechanism and is
  cross-source. Still trial-verified before landing — an unexpected rejection is
  a decision-relevant finding, not a workaround.
- **`gpui` is git-pinned and pre-1.0.** The fork branch must be pinned to an
  exact commit (no floating to a branch HEAD) so the lock is reproducible.
- **`rift-app` is the only heavy build (~20 GB skia/wgpu).** The trial and
  headless verification run excl. `rift-app`; the actual on-screen-fidelity and
  `rift-app` compile checks happen on the GPU station (App Check + `just dev`).
- **The render needs a wgpu adapter.** `lavapipe` (software Vulkan) is installed
  in the WSL dev env and `/dev/dri` + WSLg D3D12 exist; whether the headless
  renderer resolves an adapter there is the empirical render-probe unknown —
  the renderer code is identical regardless of where it runs.
- Fork code lives in `skrischer/zed` and follows gpui's own conventions; rift's
  own additions (the `[patch]`, the smoke test) follow the constitution
  (`thiserror` in libs, no `.unwrap()` in library code).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| wgpu offscreen texture + `copy_texture_to_buffer` readback, mirrored from `gpui_macos`'s `MetalHeadlessRenderer::render_scene_to_image` | Linux already renders through `gpui_wgpu`; the Metal impl is the proven blueprint (offscreen target, wait, readback, BGRA→RGBA); wgpu has native offscreen + readback | 2026-07-08 |
| **Additive `[patch]` fork on `4bee412`, not a commit bump** | Same base commit ⇒ identical `gpui` API ⇒ `gpui-component` and `termy_terminal_ui` compile unchanged ⇒ single-`gpui` holds. Avoids the API churn + termy float that made the rev bump NO-GO (`archive/spec-gpui-rev-bump.md`) | 2026-07-08 |
| Fork lives at `skrischer/zed`; rift consumes it via `[patch]` + a pinned `Cargo.lock` | The renderer reuses `gpui_wgpu`'s private draw internals (shaders, `draw_primitives`, atlas) — an external crate cannot reach them, so the change must be in `gpui` itself ⇒ a fork; `[patch]` redirects every consumer to it | 2026-07-08 |
| Single-`gpui`-invariant trial mandatory before landing, in a throwaway worktree | The invariant can break silently (rev-bump precedent: same commit, two `SourceId`s); the trial greps one source per zed-sourced crate after the `[patch]` | 2026-07-08 |
| Reuse `gpui_wgpu`'s existing `CosmicTextSystem` + sprite atlas for the headless context | Accurate glyph shaping needs a real text system (the `HeadlessAppContext` doc-comment); the wgpu renderer already owns both — the headless renderer shares them, as Metal does (`sprite_atlas().clone()`) | 2026-07-08 |
| **Scope: Linux/WSLg only; Windows (D3D11) deferred** | Linux renders via `gpui_wgpu` and is the headless-loop environment; Windows uses a different backend (`gpui_windows` D3D11) needing HWND/swapchain decoupling — a separate later phase. Accepted at the gate | 2026-07-08 |
| **The loop creates the `skrischer/zed` fork autonomously** (not a human hand-off) | The renderer is well-specified (Metal blueprint) and `repo` scope is present; the milestone's first issue forks zed off `4bee412`, writes the wgpu renderer, and pushes a pinned branch. Accepted at the gate | 2026-07-08 |
| Fork renderer authored at **upstream-PR quality** (a clean `zed-industries/zed` contribution) | Zed shares the non-macOS headless gap; writing to gpui's own conventions from the start (mirroring the `MetalHeadlessRenderer` surface, no rift-specific coupling) means an eventual upstream PR dissolves the fork rather than leaving a permanent rift-only patch. Authored against zed's current default branch shape so the PR rebases cleanly | 2026-07-08 |

## Prior art

From `docs/prior-art.md`, "Visual UI harness — prior-art index (tooling track)":

- **Headless offscreen render → PNG (the macOS blueprint to port)** —
  `gpui_macos` `MetalHeadlessRenderer` / `render_scene_to_image`;
  `gpui` `PlatformHeadlessRenderer` trait. Port the offscreen-texture + readback
  pattern to `gpui_wgpu`.
- **gpui fork / pin mechanics** — `archive/spec-gpui-rev-bump.md`: the
  single-`gpui`-invariant, the termy-fork precedent, and the exact failure mode
  (same commit → two `SourceId`s) this spec's `[patch]` + trial must avoid.

## Human prerequisites

**None.** The gate authorized the loop to create the `skrischer/zed` fork
autonomously (`gh repo fork` + branch off `4bee412` + the additive wgpu-renderer
commit; `repo` scope is present). The fork's existence is an ordering constraint
*inside* the milestone — the first issue creates it, and the `[patch]` /
`Cargo.lock` issue depends on that — but nothing is delivered by a human.

## Tracking

The decomposition into steps lives as GitHub issues, not in this file.

- Milestone: created from this spec once accepted (one per implementable step)
- Issues: reference this spec path; a PR merges only by closing one (planning gate)

## Verification

How does the developer know the spec is complete?

- [ ] **Single-`gpui` trial green:** after the `[patch]`, `Cargo.lock` holds
      exactly one `gpui` and one source per zed-sourced crate (grep verified) —
      the invariant the rev-bump broke is preserved here.
- [ ] `cargo clippy --workspace --exclude rift-app -- -D warnings` and
      `cargo test --workspace --exclude rift-app` pass (`just ci`).
- [ ] A rift smoke test builds a `HeadlessAppContext` with
      `current_headless_renderer()`, renders a trivial view, and asserts
      `capture_screenshot` returns a non-blank PNG (non-zero, non-uniform pixels).
- [ ] The render probe is recorded: whether `lavapipe`-in-WSL resolves a wgpu
      adapter and renders headlessly, or the verification runs on the GPU
      station — with a saved PNG of a known view judged visually correct.
- [ ] On the GPU station: `rift-app` builds (App Check, with and without
      `--features gallery`) unchanged by the `[patch]`.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Cross-source `[patch]` is rejected by Cargo or fails to unify the graph | Trial in a throwaway worktree **first** (rev-bump method); a rejection is a recorded finding that reshapes the approach, not a worked-around blocker |
| `lavapipe` does not resolve a headless wgpu adapter in WSL | The renderer code is unaffected; the render *verification* falls back to the GPU station. The render probe settles this before the smoke test is wired to CI |
| wgpu offscreen output drifts from the on-screen renderer (font hinting, subpixel) | Acceptable for agent-assisted visual review + Paper diff; flagged for the Phase-2 decision on whether pixel-exact CI regression is in scope |
| A future `gpui` bump forces a fork rebase | rift is frozen on `4bee412` (rev bump NO-GO), so the fork is a static additive patch until then; an upstream PR to Zed (they share the non-macOS gap) dissolves it long-term |
| The fork drags extra zed-sourced crates into the `[patch]` and one fails to unify | The trial greps **every** zed-sourced package name (not just `gpui`), per the rev-bump checklist item 4 |

## Decision log

- 2026-07-08: User directive — author the fork renderer at **upstream-PR
  quality** so it can later be proposed to `zed-industries/zed` (dissolving the
  fork). Written as a clean, general gpui contribution (gpui conventions,
  mirroring the `MetalHeadlessRenderer` surface), not a rift-specific hack. The
  rift base commit is `4bee412` (what `[patch]` pins); the upstream PR itself
  targets zed's default branch, so the renderer avoids depending on anything that
  changed after `4bee412`.
- 2026-07-08: Spec-acceptance gate (user). Two open decisions resolved:
  (1) **scope is Linux/WSLg only**, Windows (D3D11) deferred to a later phase;
  (2) **the loop creates the `skrischer/zed` fork autonomously** (not a human
  hand-off) — the milestone's first issue forks zed + writes the wgpu renderer,
  and the `[patch]` issue depends on it. Human prerequisites: none. Spec accepted
  → `READY`, merging.
- 2026-07-08: All load-bearing gpui API claims re-verified against rift's
  **actual pinned commit `4bee412`** (via `git show` in the cargo bare repo) —
  not the `1d217ee` checkout that also sits in the local cargo cache: the
  `PlatformHeadlessRenderer` trait has exactly two methods
  (`render_scene_to_image` + `sprite_atlas`) at `4bee412`,
  `current_headless_renderer()` returns `None` off-macOS, and
  `HeadlessAppContext` / `capture_screenshot` / the trait all exist at `4bee412`.
  Spec-review (PR #651, in-session Agent) verdict APPROVE; its three wording
  fixes (two-method trait, per-crate `[patch]`, named smoke-test host crate) are
  applied here.
- 2026-07-08: Spec created from the "Visual UI harness" track seed (#650). The
  track's Phase 1 is this renderer; Phase 2 (the harness) is a separate spec
  gated on it. Approach set during roadmap sparring: additive `[patch]` fork on
  the frozen `4bee412` base (not a rev bump), mirroring `gpui_macos`'s
  `MetalHeadlessRenderer` onto `gpui_wgpu`, with a mandatory single-`gpui` trial.
  Two decisions carried to the acceptance gate: Windows scope (defer vs. include)
  and how the `skrischer/zed` fork commit lands (human-prerequisite vs. in-loop).
