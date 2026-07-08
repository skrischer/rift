# Spec: gpui headless renderer (Linux/WSLg)

> Status: DRAFT
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
      (`4bee412`): a `WgpuHeadlessRenderer` implementing `render_scene_to_image`
      / `render_scene` / `sprite_atlas`, plus the `current_headless_renderer()`
      Linux arm — no other `gpui` API changes.
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
  in `gpui_wgpu` implementing `gpui::PlatformHeadlessRenderer`
  (`render_scene_to_image` = offscreen texture render + `copy_texture_to_buffer`
  readback + BGRA→RGBA; `render_scene`; `sprite_atlas`), mirrored from
  `gpui_macos`'s `MetalHeadlessRenderer` / `render_scene_to_image`; and the
  `current_headless_renderer()` Linux arm returning `Some(...)`.
- In rift: the `[patch."https://github.com/zed-industries/zed"]` redirect to the
  fork, the regenerated `Cargo.lock`, and the single-`gpui`-invariant trial
  (throwaway worktree) that proves the graph still unifies.
- A minimal rift-side smoke test: build a `HeadlessAppContext`, render a trivial
  known view, assert `capture_screenshot` yields a non-blank PNG.

### Out of scope

- **The Phase-2 harness** — snapshot registry, `TestAppContext` driving of real
  rift views, Paper-MCP diff, CI pixel baseline. Separate phase, gated on this one.
- **The Windows (D3D11) headless renderer** — deferred (see Prior decisions;
  gate-confirmed). Linux/WSLg is the headless-loop environment this unblocks.
- **macOS** — already has `MetalHeadlessRenderer`.
- **The fork commit to `skrischer/zed` itself** — a human prerequisite (the
  `[patch]` target must exist before rift's `Cargo.lock` resolves), exactly the
  role the rev-bump spec gave the termy-fork commit ("a merged commit to the
  externally-owned-but-rift-controlled fork, out of this spec's scope").
- Any `gpui` **rev bump** — the base stays `4bee412` (the rev bump is NO-GO,
  `archive/spec-gpui-rev-bump.md`); this is purely additive on it.

## Constraints

- **Single-`gpui` invariant (constitution).** `Cargo.lock` must hold exactly one
  `gpui`. Because the fork is additive on the *same* commit every consumer
  already pins (`4bee412`), the `gpui` API is byte-for-byte compatible for
  `gpui-component` and `termy_terminal_ui`; the `[patch]` redirects the source
  URL for all of them at once. The trial verifies the invariant actually holds.
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
| **Scope: Linux/WSLg only; Windows (D3D11) deferred** | `OPEN — resolved at the spec-acceptance gate` | 2026-07-08 |
| **Fork-commit landing: human-prerequisite push to `skrischer/zed` vs. in-loop fork+commit** | `OPEN — resolved at the spec-acceptance gate` | 2026-07-08 |

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

- **The `skrischer/zed` fork with the additive headless-renderer branch, pinned
  to an exact commit** — the `[patch]` target. rift's `Cargo.lock` cannot resolve
  until it exists. Whether the human pushes it or authorizes the loop to create
  it (`gh repo fork` + branch off `4bee412` + the additive commit) is the
  gate-resolved open decision below; either way the fork's existence is the
  prerequisite the milestone's first issue depends on.

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

- 2026-07-08: Spec created from the "Visual UI harness" track seed (#650). The
  track's Phase 1 is this renderer; Phase 2 (the harness) is a separate spec
  gated on it. Approach set during roadmap sparring: additive `[patch]` fork on
  the frozen `4bee412` base (not a rev bump), mirroring `gpui_macos`'s
  `MetalHeadlessRenderer` onto `gpui_wgpu`, with a mandatory single-`gpui` trial.
  Two decisions carried to the acceptance gate: Windows scope (defer vs. include)
  and how the `skrischer/zed` fork commit lands (human-prerequisite vs. in-loop).
