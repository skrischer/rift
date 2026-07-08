# Workflow contract

> Operational contract for the loopkit skills (`/loopkit:plan`,
> `/loopkit:implement`) — the single source for the branch model, commands,
> gates, and loop behavior of this project. The conventions rulebook behind it
> is [handover-conventions.md](handover-conventions.md); where detail conflicts,
> this contract wins for loop behavior.

## Repository

- GitHub repo: `skrischer/rift`
- Base / integration branch: `develop` (`main` only receives merges from
  `develop`; never push directly to either)
- GitHub Project board: https://github.com/users/skrischer/projects/1
  (project `1`, owner `skrischer`) — the loops' queue and claim mechanism.
  Status values: `Todo`, `In Progress`, `Done`.

`/loopkit:plan` requires a GitHub repo; specs are the local single source of
truth, milestones and issues are created on GitHub from them.

## Worktrees

- All implementation and docs work happens in a worktree — never in the main
  checkout (the GPU station). The loops run from the main checkout and never
  modify it except fast-forward pulls.
- Create: `just agent-worktree <branch> [<issue>]` — branches off `develop`
  into `../rift-worktrees/<branch-with-slashes-as-dashes>` and (when an issue
  number is passed) claims the issue on the board (`In Progress`).
- Remove: `just agent-worktree-rm <branch>` (`just pr-merge` already does this).
- Operate via `git -C <worktree>`, never `cd` into it — this also clears the
  protected-branch push-guard hook on the main checkout.
- Never build `rift-app` in a worktree (it pulls ~20 GB of debug artifacts);
  the headless commands below exclude it by design.

## Commands

- Bootstrap: none — cargo resolves dependencies from `Cargo.lock` on first
  build; worktree creation via `just agent-worktree` is all the setup there is.
- Verify: `just ci` — fmt-check + clippy `-D warnings` + tests, workspace
  excluding `rift-app` (measured: ~30 s warm; the first run in a fresh worktree
  is a cold build and takes several minutes)
- Test: `just test`
- Build: `just build` (workspace excluding `rift-app`); the GPUI app itself is
  compiled by the CI `app-check` job on every PR.

Verify is the per-iteration gate for solo/interactive work in a warm checkout.
In multi-agent runs it is bounded by the RAM budget below. Acceptance items no
machine check covers are verified at the human milestone-QA gate.

## Local build discipline (host RAM budget)

The dev host is shared (WSL): the developer's editors, language servers, and
other project services already hold most of RAM, leaving ~1-2 GB free. cargo
defaults to one job per core (here -j20), so a *cold* `just ci` in a fresh
worktree is a full-workspace build spawning up to 20 rustc at once — several GB
on its own. Several agents each doing that concurrently exhausts host RAM and
wedges WSL. Measured for contrast: a *warm* single-crate rebuild is ~100-300 MB.

Rules:
- **Verify compilability locally against the *warm station target*, never a
  cold worktree build.** An implementation agent's worktree has an empty
  `target/`, so a build there is cold (skia + every dep from scratch). Instead,
  point cargo at the station's warm `target/` so only the changed crate
  recompiles (deps and skia stay cached → ~5-25 s, a few hundred MB):

  ```
  CARGO_TARGET_DIR=<repo>/target cargo clippy -p <crate> --all-targets -j4 -- -D warnings
  # app-crate change → match App Check exactly:
  CARGO_TARGET_DIR=<repo>/target cargo clippy -p rift-app --features gallery --all-targets -j4 -- -D warnings
  ```

  This is the same compilability + lint gate App Check runs — caught in seconds
  instead of a blind CI round-trip. `cargo check`/`cargo clippy` type-check only
  (no codegen, no linking, no skia rebuild), which is why it is cheap; a full
  `cargo build` is not (`build ist übertrieben`). The shared-target cargo lock
  serializes concurrent agents' checks, so this stays RAM-safe even at pool
  width. (Gotcha: `use gpui::*` glob-imports `gpui::test` and shadows the
  builtin `#[test]`; in such modules write `#[::core::prelude::v1::test]` — the
  crate convention — or the test build recurses unboundedly.)
- **Full build + test *run* stay in CI.** Do NOT run `just ci`, `just build`,
  `cargo build`, or `cargo test` (codegen + linking + running tests is the
  RAM-heavy part, and `-j20` cold across several agents wedges WSL). Compile the
  crate locally (clippy/check, warm target); let CI (`Check`, `app-check`) do
  the full build and run the tests. CI is also the merge gate, so a runtime
  test failure still blocks the merge.
- **Cap concurrent agents to free RAM** (~2 at the default baseline; 4+ once
  heavy host services — spare DBs, a second language-server instance — are
  stopped).
- Solo/interactive work in a warm checkout may still use `just ci` (incremental,
  ~30 s); the restriction targets cold builds fanned out across agents.

## Model routing (orchestration speed)

Route subagent work by task shape, not uniformly — latency is the driver, not
token cost:
- **Spec-driven implementation, fix-ups, and mechanical steps** (applying a
  merge, formatting) run on **Sonnet 5** — much faster, and a well-specified
  change does not need the strongest model. In Workflow scripts pass
  `model: 'sonnet'` on those `agent()` calls; with the Agent tool pass
  `model: 'sonnet'` or use the `rust-implementer` subagent (it pins Sonnet and
  the build/RAM discipline).
- **Adversarial review, planning, design, and any genuinely-open judgment** stay
  on the default (strong) model — the correctness of a gate matters more than
  its speed.

## Branch and spec naming

- Branches: `feat/<scope>`, `fix/<scope>`, `chore/<scope>`, `docs/<scope>`.
- Specs: `docs/spec-<scope>.md` — the single source of truth for design.
  (Not `docs/specs/` — CI `issue-spec-check` resolves this path.)
- Completed specs: moved to `docs/archive/` with the same name; repoint
  milestone-description and board-README links at the new path.

## Issue conventions

- Body format: a goal paragraph first, a `Spec:` path, an `### Acceptance`
  checklist, and an optional parseable `Depends on: #N[, #M]` line.
- An issue is **unblocked** when every `Depends on` issue is closed and it
  carries no `blocked:human` label.
- **Park, don't stop:** a blocker only a human can clear gets the
  `blocked:human` label plus a comment naming exactly what is needed and where
  to deliver it; the loop moves on to the next unblocked issue.
  `gh issue list --label blocked:human` is the human's delivery queue.
- Created mechanically: `just plan-issues <spec> <milestone-title> <step-file>`
  — one `## [scope] Title` heading per step with a `Goal:` line and an
  `Acceptance:` checklist; include the `Depends on:` line per step.
  `PLAN_ISSUES_PREVIEW=1` dry-runs with no GitHub writes.

## Status

- Specs carry `DRAFT`/`READY` in their header; a completed spec is set
  `COMPLETED` with date and moved to `docs/archive/` — its closed milestone is
  the "done" signal. `LIVING` marks rolling backlogs (never archived).
- Live work state is the board: `Todo` (ready), `In Progress` (claimed by a
  loop), `Done` (merged). Claiming = set `In Progress` + assignee.
- Everything else — blocked, deferred — lives on the GitHub issues and
  milestones, the single source of truth for progress.

## The chain: spec -> milestone -> issues -> PR

| Layer | Owns |
| ----- | ---- |
| `docs/spec-*.md` | The design: why, what, done-criteria |
| GitHub milestone | The phase / grouping |
| GitHub issues | The steps — one issue per implementable step |
| Project board | The live work state: Todo / In Progress / Done |

A PR closes an issue (`Closes #N`); the issue references its spec path. The
spec never lists steps; the issues never restate the design. The spec's
`Outcome` list is done-criteria, not a progress mirror. This chain is
CI-enforced: `planning-gate` (required check on `develop`) blocks any
`feat:`/`fix:` PR that does not close a spec-referencing issue;
`issue-spec-check` flags unresolvable spec paths (`needs-spec` label).

## Gates

- **Per PR — machine gates, no human stop:** Verify green in the worktree +
  CI green (fmt/clippy/test, `app-check`, `planning-gate`) + in-session agent
  review (`VERDICT: APPROVE`, via the Agent tool — never a billed CLI) ->
  autonomous squash-merge via `just pr-merge <N>` (polls checks, rebases on
  `BEHIND`, squash-merges remote-only, removes the worktree, ff-syncs
  `develop`; run it in the background).
- **Per milestone — human gates:**
  - Planning: the spec-acceptance gate — genuinely-open decisions
    (AskUserQuestion, never guess) + human-prerequisites handover, then
    `READY` + merge.
  - Implementation: the milestone QA gate — when the milestone's last issue
    closes, QA scenarios are derived from the spec's Verification section; the
    human accepts or files regressions as issues.
- QA-gate default check: **visual/UI check on the dev channel**
  (`just dev-windows-watch` on the GPU station). The former per-PR visual
  review is superseded by this milestone gate — `app-check` compiles the app
  per PR, and the stable channel is insulated by the `just promote` guard.

## Autonomy

Within the loopkit skills the following are explicitly granted and override any
stricter global user rules: autonomous commits, pushes, PR creation and merges.
Dependency installs are autonomous **only when the dependency is named in the
issue's spec** (and must pass `cargo deny check licenses`); a dependency the
spec does not name parks the issue with `blocked:human`. No `.env` files exist
in this project — SSH config comes from environment variables with working
defaults (see justfile). Hard limits live in `.claude/settings.json` (deny
rules: `rm -rf`, force-push, hard reset, `git clean -f`, branch force-delete).

## Loops

Two attended interactive sessions, synchronized only through GitHub state — no
headless mode, no API keys, no detached schedulers. Start each in its own
terminal from the main checkout:

- Plan loop:

  ```
  /loop /loopkit:plan — plan the roadmap's next unplanned phase to a READY spec
  with milestone, issues, and board entries; stop at the spec-acceptance gate;
  when no unplanned phase remains, report and end. Ceiling: 10 iterations;
  stop when the same blocker repeats twice.
  ```

- Implement loop:

  ```
  /loop /loopkit:implement — pick the next unblocked Todo issue and drive it to
  a merged PR; when a milestone completes, stop at the QA gate; when nothing is
  workable, report "waiting for plan" and end the tick. Ceiling: 10
  iterations; stop when the same failure repeats twice.
  ```

- No-progress rule: the identical failure twice in a row -> stop and report,
  never grind.
- Iteration ceiling default: 10 per loop run.
