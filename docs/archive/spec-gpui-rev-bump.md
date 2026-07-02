# Spec: gpui rev bump investigation

> Status: COMPLETED
> Created: 2026-06-13
> Completed: 2026-07-02

Analyse and trial-run a bump of the git-pinned `gpui` / `gpui_platform`
(+ the lockstep `gpui-component`) revision: document why a bump is needed, why it
cannot just be done, and what a real bump must watch for — backed by one trial
bump in an isolated worktree and a go/no-go recommendation. No production bump
lands here.

## Outcome

What is true when this work is done:

- [x] A findings document answers all three questions: **why** a bump is needed
      (the motivating consumers, starting with the #127 WebView child-window
      compositing), **why it cannot just be done** (the coupling and risks), and
      **what must be watched** (a checklist a real bump would follow).
- [x] **One** trial bump was performed in a throwaway worktree and its concrete
      result is recorded: which lockstep set was moved and to which rev; whether
      `Cargo.lock` still resolves to exactly one `gpui`; whether
      `cargo build --workspace` / `cargo test --workspace` / clippy pass; and the
      catalogue of what breaks (API churn, the termy fork's pinned `gpui` rev,
      any second `gpui`).
- [x] The trial reaches — or documents the blocker that prevents reaching — the
      payoff check: does the #127 WebView demo render a live page on the bumped
      rev (the native child window is no longer overdrawn)?
- [x] A **go/no-go recommendation** for a production bump, with either the
      concrete ordered steps it would take or the blockers that defer it.

## Scope

### In scope

- Desk analysis of the bump: motivation, the dependency coupling
  (`gpui` + `gpui_platform` + `gpui-component` + the `termy_terminal_ui` fork),
  and the breaking-change / dogfooding risks.
- **One** trial bump in an isolated git worktree (its own `target/`, never the
  station's), moving the lockstep set to one candidate rev and recording the
  result end-to-end: build, test, clippy, single-`gpui` invariant, and — best
  effort — the #127 WebView render check, resurrecting the reverted #127 webview
  code (PR #243 / commits `50c7840`, `0378877`, `c6cce10`, `06d0e75`) in the
  throwaway worktree only.
- A written findings + go/no-go recommendation, recorded in this spec's decision
  log — which is preserved when the spec is later set `COMPLETED` and moved to
  `docs/archive/`, so the artefact survives close-out — and mirrored on the issue.

### Out of scope

- **Landing the bump on `develop`.** That is a separate follow-up, planned only
  if and as the findings recommend; this spec deliberately ships no production
  dependency change.
- **Fixing all breakage the trial surfaces.** The trial *catalogues* breakage; it
  does not repair the app against the new rev.
- **Re-landing the #127 WebView demo.** Its own follow-up, gated on a real bump.
- **Updating the `termy_terminal_ui` fork.** If the trial shows the fork's pinned
  `gpui` blocks the bump, that becomes a documented prerequisite — not work done
  here.
- Bumping any non-`gpui` dependency.

## Constraints

- **Single-`gpui` rule (constitution).** `Cargo.lock` must hold exactly one
  `gpui`. `gpui`, `gpui_platform` and `gpui-component` therefore have to move in
  lockstep; the trial must verify the invariant holds (or record that it broke).
- **GPUI is pre-1.0 and git-pinned.** It ships from git, not crates.io, and
  every consumer pins or floats a commit; breaking-change churn between revs is
  expected (`docs/prior-art.md`: "expect breaking-change churn"; upstream warns
  to pin a specific GPUI commit alongside `gpui-component`).
- **The `rift-app` build is the only heavy one** (skia/wgpu, ~20 GB). The trial
  runs in an isolated worktree with its own `target/` and must not disturb the
  station's `target/` or the dogfooding stable channel.
- **The motivating signal is a GUI render**, which no headless gate can verify;
  the WebView payoff check happens on the GPU station via `just gallery`.
- Minimal code: the only code this spec produces is the throwaway trial; the
  durable artefact is the findings document.

## Human prerequisites

None. The spike runs entirely on the existing dev setup — the GPU station, the
existing SSH / dogfooding config, and the in-loop dependency/worktree autonomy
the constitution already grants. No new secrets, accounts, or provisioning.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Investigation only — analyse + one trial + document, do **not** land the bump | The user scoped the issue to a spike; landing is a follow-up decided *by* the findings, since the blast radius (whole GPUI foundation) is unknown until measured | 2026-06-13 |
| The motivating consumer is the #127 WebView child-window compositing | On the pinned `gpui` (`4bee412`) the native WebView2 child window is overdrawn by gpui's DXGI surface and `GPUI_DISABLE_DIRECT_COMPOSITION` is ineffective; windowed-child webview compositing needs a newer gpui (see the archived `spec-component-gallery.md` decision log) | 2026-06-13 |
| Lockstep set = `gpui` + `gpui_platform` + `gpui-component`; the trial keeps one `gpui` | Constitution single-rev rule; all three resolve from git and unify in the lock today (`gpui` at `4bee412`) | 2026-06-13 |
| The `termy_terminal_ui` fork is a first-class subject of the analysis | rift's pinned termy (`49d3928`) hard-pins `gpui` `rev=83de8a2…`, yet the lock resolves to `4bee412` with no `[patch]` — an anomaly the spike must explain; the fork's pin is a likely hard blocker for any bump | 2026-06-13 |
| Trial candidate = latest `gpui-component` HEAD, letting it drag the matching `gpui`/`gpui_platform` | It is what a real bump would target and where the webview-compositing fix lives; `gpui-component@HEAD` is tested against the `gpui` it floats, so the set is coherent (empirically `gpui-component` `cda0fc7` → `gpui` `8589cbb`). The trial pins and records the exact resolved revs. (user, spec-acceptance gate) | 2026-06-13 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file.

- Milestone: [Phase 908 — gpui rev bump investigation](https://github.com/skrischer/rift/milestone/22)
- Issues: #269 (the one investigation issue: analyse + trial + document).

## Verification

How does the developer know the spec is complete?

- [x] The findings document answers why-needed / why-not-trivial / what-to-watch,
      each with concrete evidence from this codebase.
- [x] The trial bump's result is recorded: the moved lockstep set + candidate rev,
      the `Cargo.lock` single-`gpui` outcome, build/test/clippy results, and the
      breakage catalogue (including the termy-fork interaction).
- [x] The WebView render result on the bumped rev is recorded — either it renders,
      or the exact blocker that prevented reaching the check is documented.
- [x] A go/no-go recommendation with ordered next steps (or deferral blockers) is
      written into the decision log.
- [x] `develop` is unchanged by this spec (no production dependency bump merged);
      the trial lived only in a throwaway worktree.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The `termy_terminal_ui` fork's pinned `gpui` rev does not match the candidate, forking `gpui` into two | The trial measures this first; if it blocks, the finding is "update the termy fork is a prerequisite" — documented, not worked around |
| API churn between `gpui` `4bee412` and the candidate is large enough to break the terminal widget / window setup / gallery | The trial *catalogues* the breakage surface rather than fixing it; the go/no-go weighs the size of that surface |
| The trial's heavy build disturbs the station / dogfooding stable channel | Run strictly in an isolated worktree with its own `target/`; never build the candidate on the station's main `target/` |
| The WebView payoff check is unreachable because earlier breakage blocks compilation | Record the furthest point reached; a "cannot even build" result is itself a valid, decision-relevant finding |
| The candidate rev is a moving target (gpui-component floats) | Pin the exact candidate rev in the trial and record it, so the finding is reproducible |
| The current one-`gpui` resolution is anomalous (termy hard-pins `83de8a2…`, lock holds `4bee412`, no `[patch]`); it may be fragile or accidental, and a bump could silently break it | Explaining the current resolution is an explicit deliverable; the trial verifies the single-`gpui` invariant after the move rather than assuming it survives |
| The throwaway worktree's heavy `target/` (~20 GB skia/wgpu) is left behind | Remove it with `just agent-worktree-rm` once the findings are recorded |

## Decision log

- 2026-06-13: Spec created from the #127 close-out. #127 shipped a WebView notice
  because the live `gpui-wry` embed does not composite on `gpui` `4bee412`; this
  spec investigates the bump that would unblock it, without committing to land it.
- 2026-06-13: Spec-acceptance gate — candidate rev for the trial set to latest
  `gpui-component` HEAD (dragging the matching `gpui`/`gpui_platform`); human
  prerequisites confirmed `none`. Spec accepted and set `READY`.
- 2026-07-02: Investigation complete (#269). Findings below; **recommendation:
  NO-GO for now** — see ordered next steps at the end of this entry.

  **Why a bump is needed.** The sole identified motivating consumer, today, is
  still #127: on the pinned `gpui` (`4bee412118d`), the `gpui-wry` (Wry/WebView2)
  native child window is overdrawn by gpui's DXGI surface, and
  `GPUI_DISABLE_DIRECT_COMPOSITION` has no effect on this rev (see the archived
  `spec-component-gallery.md` decision log, 2026-06-13 entry, and PR #243). A
  fresh sweep of the open roadmap/issues on 2026-07-02 (`gh issue list --search
  gpui`, `docs/roadmap.md`) turned up no second issue or phase that names a
  newer-`gpui` requirement — #127 remains the only concrete driver.

  **Why it cannot just be done — confirmed, and worse than the risk table
  expected.** The June convergence strategy (archived `spec-gpui-component-
  adoption.md`, 2026-06-02 spike) rested on one premise: `gpui`, `gpui_platform`
  and `gpui-component` all **bare-track** the same `zed-industries/zed` git URL
  (no `rev=`), so Cargo unifies every consumer to whatever single commit the
  committed `Cargo.lock` pins. That premise held on 2026-06-02. **It no longer
  holds upstream.** Fetching `gpui-component`'s own `Cargo.toml` at its current
  HEAD (`a9a7341c35b62f27ff512371c62419342264710c`, fetched 2026-07-02) shows it
  now declares `gpui = { git = "…/zed", rev = "1d217ee39d381ac101b7cf49d3d22451ac1093fe" }`
  — an **explicit rev**, not bare. Upstream re-added a rev lock sometime between
  `9ad30e6` (rift's current pin) and `a9a7341c` (HEAD), reversing the exact
  behavior the archived spec's convergence architecture depended on ("gpui-
  component floats gpui by design (rev lock removed upstream 2025-12-18)").

  Concretely, bumping only `gpui-component`/`gpui-component-assets` to `a9a7341c`
  while leaving rift's own `gpui`/`gpui_platform` lines bare (as the pre-existing
  Cargo.toml comment instructs) produces **two** `gpui` package entries in
  `Cargo.lock`: one at the bare source (`zed#1d217ee…`, after `cargo update -p
  gpui --precise 1d217ee…`) and one at the query-pinned source
  (`zed?rev=1d217ee…#1d217ee…`) pulled in by `gpui-component` → `http_client`.
  Same commit, two different Cargo `SourceId`s, non-unifying — Cargo compiles
  `gpui` twice.

  Adding a matching explicit `rev = "1d217ee…"` to rift's own `gpui`/
  `gpui_platform` lines (mirroring gpui-component's new pin) **reduces but does
  not fix** this: the `termy_terminal_ui` fork (rift's pin: `2c2bd091e4a`, commit
  message "bare-track gpui so downstream pins the rev") still bare-tracks `gpui`
  in **its own** workspace `Cargo.toml` (`gpui = { git = "…/zed", package =
  "gpui" }`, no rev — confirmed by reading the fork's manifest at rift's pinned
  commit). Because termy is a separate git-dependency resolution unit, that bare
  edge does not inherit rift's `rev=` pin at all — it floats independently to
  whatever `zed-industries/zed`'s default branch HEAD happens to be at
  lockfile-generation time. In the trial this resolved to `zed#bb48a4298…`, a
  third, later, non-reproducible commit. Net result after both fixes: **still
  two `gpui` entries** in `Cargo.lock` (rift+gpui-component unify to
  `1d217ee…`; termy's bare edge floats to a different commit) — **the
  single-`gpui` invariant is broken**, and termy's own June mitigation ("bare-
  track gpui so downstream pins the rev") is now insufficient, because it
  assumed every party stayed bare. The moment any one party (here,
  gpui-component) switches to an explicit rev, bare-tracking elsewhere stops
  converging to it.

  This also resolves the "anomaly" flagged in this spec's Prior decisions
  table (termy hard-pinning `83de8a2…` while the lock held `4bee412`, no
  `[patch]`): that description was already stale by the time this spec was
  written — rift's `termy_terminal_ui` pin had moved from `49d3928` (hard `rev
  = 83de8a2…`) to `2c2bd09` (bare-tracked) on 2026-06-22, in `chore(deps): bump
  termy_terminal_ui to 2c2bd09`. The "no `[patch]`, still resolves to one
  `gpui`" behavior on `develop` today is not fragile or accidental — it is
  exactly the bare-tracking convergence working as designed, for as long as
  gpui-component itself stays bare. It just stopped staying bare.

  **Confirmed compile failure (not hypothetical).** With rift's `gpui`/
  `gpui_platform` pinned to `1d217ee…` (matching gpui-component) and termy left
  as-is, `cargo check --workspace --exclude rift-app` fails on `rift-terminal`
  — the crate that exchanges `gpui` types with `termy_terminal_ui` at its API
  boundary — with 14 errors:
  - 12 are the textbook duplicate-crate diamond (`E0308`/`E0277`), e.g.
    `expected gpui::color::Hsla, found gpui::Hsla`, `the trait bound
    TerminalGrid: gpui::IntoElement is not satisfied` — rift-terminal's own code
    compiles against the rev-pinned `gpui`, `termy_terminal_ui` compiles against
    its independently-floated one; `rustc` reports "there are multiple
    different versions of crate `gpui` in the dependency graph" on every one.
    All 12 are in `crates/terminal/src/pane_view.rs`.
  - 2 (`E0061`, `crates/terminal/src/session_view.rs:564` and
    `crates/terminal/src/pane_view.rs:975`) are genuine, dual-gpui-independent
    API churn: `Styled::flex_grow()` changed from `fn flex_grow(mut self) ->
    Self` to `fn flex_grow(mut self, grow: f32) -> Self` between `4bee412` and
    `1d217ee`. Mechanically trivial (add an `f32`), but real evidence that
    pre-1.0 churn exists independent of the termy blocker.
  - `cargo clippy --workspace --exclude rift-app -- -D warnings` fails
    identically (same 14 errors; no additional lint-only issues surfaced).
  - `cargo test --workspace --exclude rift-app` fails the same way — cargo
    aborts the whole invocation before running any suite once `rift-terminal`
    fails to compile (no `test result:` line for anything). The 8 workspace
    crates that don't touch `gpui` (`rift-daemon`, `rift-explorer`,
    `rift-logging`, `rift-lsp`, `rift-plugin-api`, `rift-protocol`, `rift-ssh`,
    `rift-tmux-core`) all compiled cleanly under `cargo check`/`clippy`
    (`.rmeta` produced for each) and their test binaries built successfully;
    running those binaries directly (bypassing cargo's workspace-wide abort)
    confirms **306 tests passed, 0 failed** across all 8 — the breakage is
    fully confined to the two `gpui`-consuming crates.
  - Also observed: the *set* of zed-workspace crates that must stay unified
    grew between revs — `http_client`, `util`, `media`, `perf`, `refineable`,
    `scheduler`, `sum_tree`, `zlog`, `ztracing` (and their macro crates) appear
    as new `zed`-sourced dependencies at `1d217ee` that were not separately
    named in the `4bee412` graph. A future bump's single-source check must
    re-verify all of these, not just `gpui` itself.

  **Trial result (Outcome/Verification record).**
  - Lockstep set moved: `gpui`, `gpui_platform` (bare → `rev =
    "1d217ee39d381ac101b7cf49d3d22451ac1093fe"`), `gpui-component`,
    `gpui-component-assets` (`rev`: `9ad30e631e1…` → `a9a7341c35b…`, HEAD of
    `longbridge/gpui-component` as observed 2026-07-02 — a later commit than
    the `cda0fc7`/`8589cbb` pair cited illustratively in this spec's Prior
    decisions table, confirming the "candidate is a moving target" risk; the
    exact rev used here is pinned above for reproducibility).
  - `Cargo.lock` single-`gpui` invariant: **broken** (2 entries — see above).
  - `cargo build`/`check --workspace --exclude rift-app`: **fails**
    (`rift-terminal`, 14 errors). `clippy`: **fails identically**. `test`:
    **fails to run anything** as a workspace invocation; the 8 gpui-free
    crates individually pass (306/306 tests) when run directly.
  - `rift-app` was never built (hard constraint, never violated). It holds the
    majority of rift's GPUI surface — `crates/app/src/{editor,file_tree,main,
    workspace,worktree}.rs` + `gallery/{demos,mod}.rs`, ~7,492 lines — versus
    `crates/terminal/src/{pane_view,session_view,…}.rs`, ~4,399 lines, the part
    this trial *could* and did verify. Roughly two-thirds of the codebase's
    GPUI-touching surface is therefore **unverified** by this headless trial;
    a real bump attempt learns nothing about `rift-app`'s own churn until it
    is built on the GPU station.
  - Throwaway worktree: `git worktree add --detach <scratch>/gpui-trial
    develop`, never `develop` itself; removed after this entry was written
    (`git worktree remove`).

  **The #127 WebView payoff check: deferred, blocker documented.** Not
  reachable in this investigation — `rift-app` (which hosts the `gallery`
  binary) is never built headlessly by hard constraint, and the trial's
  `rift-terminal` failure means even a headless `--workspace` build does not
  reach `rift-app` today regardless. The check remains exactly as scoped:
  reapply the reverted #127 commits (PR #243: `50c7840` real WebView demo via
  `gpui-wry` pinned to the workspace's `gpui-component` rev, `0378877`
  WebView2Loader.dll deploy, `c6cce10` `GPUI_DISABLE_DIRECT_COMPOSITION` env,
  `06d0e75` load-url-after-entity-install fix) on top of a **fully converged**
  single-`gpui` bump (i.e. after the termy-fork prerequisite below is closed),
  then run `just gallery` on the GPU station and look at whether the WebView2
  child window now composites over gpui's surface instead of being overdrawn.

  **What a real bump must watch (checklist).**
  1. Read the target `gpui-component` rev's own `Cargo.toml`/`Cargo.lock`
     first — do not assume it bare-tracks `gpui`; confirm exactly how it (and
     `gpui_platform`, `gpui_web`, `gpui_macros`, `reqwest_client`) is declared.
     This flipped once already; it can flip again in either direction.
  2. **Prerequisite, out of this spec's scope:** push a commit to
     `skrischer/termy` adding an explicit `rev = "<candidate>"` to its `gpui`
     (and any other `zed`-sourced) dependency, matching whatever the target
     `gpui-component` rev pins. Merge it, then bump rift's own
     `termy_terminal_ui` pin to that termy commit *first*, alone, and confirm
     `just ci` is still green (a no-op sanity check) before touching `gpui`
     itself.
  3. Only then bump the lockstep set (`gpui` + `gpui_platform` +
     `gpui-component` + `gpui-component-assets`), all pinned to the **same
     explicit rev** — leaving any of them bare is what breaks convergence now.
  4. After regenerating `Cargo.lock`, grep for every zed-sourced package name
     (not just `gpui` — see the grown crate list above) and confirm exactly
     one source string per name.
  5. `cargo build`/`test`/`clippy --workspace --exclude rift-app` green — this
     is necessary but covers only ~1/3 of the codebase's GPUI surface.
  6. On the GPU station: build `rift-app` (with and without `--features
     gallery`) and catalogue/fix its churn — expect more of the same class as
     `flex_grow` (mechanical signature changes), likely a larger volume given
     `rift-app`'s much bigger surface.
  7. `cargo deny check licenses` on the full new graph — the candidate pulls
     several hundred new/changed transitive crates (objc2 bindings, `phf`,
     `zbus`, `quick-xml`, …) not yet vetted under the current `deny.toml`.
  8. Re-verify the `gallery` feature's `gpui-component/tree-sitter-rust`
     passthrough still names a real feature on the candidate rev.
  9. Re-verify the dogfooding `[profile.stable]` / `gpui_windows`
     build-script `debug-assertions` override still applies cleanly — it is
     keyed to gpui-internal `cfg(debug_assertions)` shader-path behavior.
  10. Re-attempt the #127 WebView resurrection on the GPU station (see above)
      — the actual payoff check.
  11. Pin deliberately: commit the exact rev everywhere (no floating to
      `HEAD`), and update the Cargo.toml comment block above the `gpui` lines
      — its current premise ("left rev-less, gpui-component floats") is now
      factually wrong for any rev at or after `a9a7341c`.
  12. Land the bump as its own `chore(deps)` PR (Cargo.toml + Cargo.lock only),
      separate from re-landing the #127 WebView demo (its own follow-up, per
      the archived `spec-component-gallery.md`).

  **Recommendation: NO-GO for now.** The candidate breaks the single-`gpui`
  invariant and fails `build`/`test`/`clippy` on the one crate this trial could
  verify headlessly, for a concretely-understood but currently-unmet
  prerequisite (the termy fork needs an explicit rev pin it does not have).
  Landing today would break the terminal widget and, by extension, the active
  dogfooding stable channel. **Ordered next steps** to reach a landable state:
  checklist items 2 → 3 → 4 → 5 (all re-attemptable headlessly, in a fresh
  throwaway worktree, once the termy prerequisite lands) → 6 → 7 → 8 → 9 (GPU
  station) → 10 (payoff check) → 12 (land). Hard blockers this investigation
  could not clear from this sandbox: item 2 (requires a merged commit to the
  externally-owned-but-rift-controlled `skrischer/termy` fork, out of this
  spec's scope by design) and items 6/9/10 (require the GPU station).
