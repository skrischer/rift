# Spec: gpui rev bump investigation

> Status: DRAFT
> Created: 2026-06-13
> Completed: —

Analyse and trial-run a bump of the git-pinned `gpui` / `gpui_platform`
(+ the lockstep `gpui-component`) revision: document why a bump is needed, why it
cannot just be done, and what a real bump must watch for — backed by one trial
bump in an isolated worktree and a go/no-go recommendation. No production bump
lands here.

## Outcome

What is true when this work is done:

- [ ] A findings document answers all three questions: **why** a bump is needed
      (the motivating consumers, starting with the #127 WebView child-window
      compositing), **why it cannot just be done** (the coupling and risks), and
      **what must be watched** (a checklist a real bump would follow).
- [ ] **One** trial bump was performed in a throwaway worktree and its concrete
      result is recorded: which lockstep set was moved and to which rev; whether
      `Cargo.lock` still resolves to exactly one `gpui`; whether
      `cargo build --workspace` / `cargo test --workspace` / clippy pass; and the
      catalogue of what breaks (API churn, the termy fork's pinned `gpui` rev,
      any second `gpui`).
- [ ] The trial reaches — or documents the blocker that prevents reaching — the
      payoff check: does the #127 WebView demo render a live page on the bumped
      rev (the native child window is no longer overdrawn)?
- [ ] A **go/no-go recommendation** for a production bump, with either the
      concrete ordered steps it would take or the blockers that defer it.

## Scope

### In scope

- Desk analysis of the bump: motivation, the dependency coupling
  (`gpui` + `gpui_platform` + `gpui-component` + the `termy_terminal_ui` fork),
  and the breaking-change / dogfooding risks.
- **One** trial bump in an isolated git worktree (its own `target/`, never the
  station's), moving the lockstep set to one candidate rev and recording the
  result end-to-end: build, test, clippy, single-`gpui` invariant, and — best
  effort — the #127 WebView render check (re-applying the reverted #127 webview
  code in the throwaway worktree only).
- A written findings + go/no-go recommendation, recorded in this spec's decision
  log (and the issue).

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

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Investigation only — analyse + one trial + document, do **not** land the bump | The user scoped the issue to a spike; landing is a follow-up decided *by* the findings, since the blast radius (whole GPUI foundation) is unknown until measured | 2026-06-13 |
| The motivating consumer is the #127 WebView child-window compositing | On the pinned `gpui` (`4bee412`) the native WebView2 child window is overdrawn by gpui's DXGI surface and `GPUI_DISABLE_DIRECT_COMPOSITION` is ineffective; windowed-child webview compositing needs a newer gpui (see the archived `spec-component-gallery.md` decision log) | 2026-06-13 |
| Lockstep set = `gpui` + `gpui_platform` + `gpui-component`; the trial keeps one `gpui` | Constitution single-rev rule; all three resolve from git and unify in the lock today (`gpui` at `4bee412`) | 2026-06-13 |
| The `termy_terminal_ui` fork is a first-class subject of the analysis | rift's pinned termy (`49d3928`) hard-pins `gpui` `rev=83de8a2…`, yet the lock resolves to `4bee412` with no `[patch]` — an anomaly the spike must explain; the fork's pin is a likely hard blocker for any bump | 2026-06-13 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file.

- Milestone: [Phase 908 — gpui rev bump investigation](<milestone-url>)
- Issues: created from this spec once it is `READY` (one investigation issue:
  analyse + trial + document).

## Verification

How does the developer know the spec is complete?

- [ ] The findings document answers why-needed / why-not-trivial / what-to-watch,
      each with concrete evidence from this codebase.
- [ ] The trial bump's result is recorded: the moved lockstep set + candidate rev,
      the `Cargo.lock` single-`gpui` outcome, build/test/clippy results, and the
      breakage catalogue (including the termy-fork interaction).
- [ ] The WebView render result on the bumped rev is recorded — either it renders,
      or the exact blocker that prevented reaching the check is documented.
- [ ] A go/no-go recommendation with ordered next steps (or deferral blockers) is
      written into the decision log.
- [ ] `develop` is unchanged by this spec (no production dependency bump merged);
      the trial lived only in a throwaway worktree.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The `termy_terminal_ui` fork's pinned `gpui` rev does not match the candidate, forking `gpui` into two | The trial measures this first; if it blocks, the finding is "update the termy fork is a prerequisite" — documented, not worked around |
| API churn between `gpui` `4bee412` and the candidate is large enough to break the terminal widget / window setup / gallery | The trial *catalogues* the breakage surface rather than fixing it; the go/no-go weighs the size of that surface |
| The trial's heavy build disturbs the station / dogfooding stable channel | Run strictly in an isolated worktree with its own `target/`; never build the candidate on the station's main `target/` |
| The WebView payoff check is unreachable because earlier breakage blocks compilation | Record the furthest point reached; a "cannot even build" result is itself a valid, decision-relevant finding |
| The candidate rev is a moving target (gpui-component floats) | Pin the exact candidate rev in the trial and record it, so the finding is reproducible |

## Decision log

- 2026-06-13: Spec created from the #127 close-out. #127 shipped a WebView notice
  because the live `gpui-wry` embed does not composite on `gpui` `4bee412`; this
  spec investigates the bump that would unblock it, without committing to land it.
