# Spec: Implementation workflow automation

> Status: READY
> Created: 2026-06-07
> Completed: —

Automate the implementation cycle (issue → merged) via `just` recipes, a CI
correctness gate, an interactive tmux reviewer pane, and a `/implement` skill —
with GitHub board status transitions baked into every phase.

## Outcome

- [ ] `just pr-merge <n>` takes a green PR to merged, removes the worktree, and
      ff-syncs `develop` — entirely against the remote, never touching the local
      checkout, with no manual git cleanup afterward.
- [ ] `rift-app` is compiled in CI: a deliberate `main.rs`/app-affecting compile
      break turns the PR check red *before* merge instead of at visual review.
- [ ] Starting work via `just agent-worktree <branch> <issue#>` moves the issue to
      `In Progress` on the board; `just pr-merge` leaves it `Done`. The status is
      never forgotten because the recipes own the transition.
- [ ] The reviewer runs in its own tmux pane with a fresh context, emits a verdict
      the orchestrator can read without parsing the live pane, and the pane stays
      interactive for the human afterward.
- [ ] `/implement <issue#>` drives one real READY-spec issue from start to merged,
      applying the status transitions and the hard review gate along the way.

## Scope

### In scope

- `just pr-merge <n>` + `just pr-wait <n>` — unified poll → rebase-on-BEHIND →
  remote-only squash-merge → worktree-rm → develop ff-sync, with the gh-2.45
  workarounds.
- `scripts/set-issue-status.sh <issue#> <Todo|In Progress|Done>` — ProjectV2
  status flip.
- `just agent-worktree <branch> [issue#]` — optional issue arg flips to
  `In Progress` on creation.
- A CI job compiling `rift-app` (`cargo check -p rift-app`, cached), run always.
- `just review-pane <branch>` — interactive `claude` reviewer in a tmux pane,
  verdict via file, plus a `just review-pane-rm <branch>` teardown (kill pane +
  drop the verdict file) invoked best-effort from `pr-merge` cleanup so a merge
  also closes its review pane.
- `.claude/skills/implement/SKILL.md` — the `/implement` orchestration skill.

### Out of scope

- The planning side (`just plan-issues`, milestone/issue generation from a spec) —
  ships as its own planning skill in a separate spec.
- The bundled-channel boilerplate refactor (6-site pattern) — its own `refactor:`
  issue, not smuggled in here.
- Agent-teams `TaskCompleted`/`TeammateIdle` hooks as a *required* gate — noted as
  optional local fast-feedback only; not a deliverable (fires only in team mode,
  so not durable enforcement).

## Constraints

- `claude -p` / headless `--print` is forbidden anywhere — the subscription billing
  change on 2026-06-15 makes it cost extra credits. Reviewer is interactive only.
- `gh` 2.45.0 quirks: no `--json` on `gh pr checks`; no `gh pr update-branch`; repo
  auto-merge disabled (no `--auto`). Poll via `gh pr view --json statusCheckRollup`.
- Runs inside a real tmux session (`$TMUX` set) on a Windows host + WSL2. The
  reviewer pane depends on this; agent-teams split-pane mode is NOT used.
- Board: project `PVT_kwHOBLauTs4BZeLy`, Status field `PVTSSF_…maE`, options
  `Todo / In Progress / Done`.
- The push-guard hook fires only on Bash commands matching the `Bash(git push*)`
  prefix; it then blocks when the current branch (in the hook's cwd) is
  `main`/`develop` or the command targets them. A push via `git -C <worktree> push`
  or inside a `just` recipe does not start with `git push`, so it bypasses the hook
  entirely — `pr-merge`'s internal pushes are unaffected. (Never push a protected
  branch directly regardless.)
- `just agent-worktree`'s new issue argument must default to empty (`issue=""`) so
  existing single-arg callers (`just agent-worktree feat/x`) keep working and skip
  the board flip.
- No new dependencies. Recipes use only `just`, `gh`, `git`, `tmux`, `cargo`.
- The hard visual-review gate before merge stays — never blind-merge GPU/main.rs.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| No `claude -p` / headless mode anywhere | Billing change 2026-06-15 charges extra credits; reviewer is interactive `claude` instead | 2026-06-07 |
| Reviewer = manual interactive `claude` in a tmux pane, not an agent-teams teammate | Must stay interactive for the human after the review; verified teammates go idle post-task and manual tmux is more robust on WSL | 2026-06-07 |
| Reviewer verdict via `.claude/review-<branch>.md` file; `send-keys` only to drive the pane | Avoids the send-keys newline-submit/quoting fragility recorded in the runbook when reading results back | 2026-06-07 |
| Merge is remote-only (`gh pr merge`, never the local checkout) | Eliminates the 4×-observed local-state failure class and preserves the visual-review gate | 2026-06-07 |
| `rift-app` CI job runs always, not path-filtered on `crates/app/**` | Breakage often originates in non-app crates (the 6-site channel pattern), so a path filter would be leaky | 2026-06-07 |
| `rift-app` is checked in a separate `app-check` CI job with its own `Swatinem/rust-cache` key | Keeps the fast `check` job lean, isolates the heavy skia/wgpu build, gives a distinct app-compiles signal | 2026-06-07 |
| Mechanics in `just`/`scripts`, orchestration in the skill | Recipes stay testable and reusable outside the skill; matches the existing justfile + push-guard-script precedent | 2026-06-07 |
| `pr-wait` reports green only when `statusCheckRollup` is non-empty AND every entry is `SUCCESS`/`NEUTRAL`; an empty rollup keeps waiting under a bounded timeout | Avoids the `gh pr checks` exit-0 trap (recorded in `implementation-workflow.md`) that merges before CI registers | 2026-06-07 |
| `set-issue-status.sh` resolves issue# → board item id via `gh api graphql` over the project's paged `items` (match on `content.number`); field/option ids queried at runtime, not hardcoded | Issue numbers are not board item ids; runtime lookup avoids brittle hardcoded `PVTI_…`/option ids | 2026-06-07 |
| Reviewer pane launches via `command claude`, not the `claude` alias | The alias starts with `-r` (resume) and would reuse a prior session instead of a fresh review context | 2026-06-07 |

## Tracking

- Milestone: Workflow automation (created once this spec is `READY`)
- Issues: created from this spec after it merges to `develop` (one per implementable
  step). The step decomposition lives only as issues, not here.

## Verification

- [ ] `cargo clippy --workspace --exclude rift-app -- -D warnings` and
      `cargo test --workspace --exclude rift-app` still pass.
- [ ] A PR with an app-affecting compile break shows the `app-check` CI job red;
      a correct one is green.
- [ ] A test issue driven through the cycle shows `Todo → In Progress → Done` on the
      board, driven only by the recipes.
- [ ] `just agent-worktree feat/x` with no issue arg still succeeds and skips the
      board flip (existing callers unbroken).
- [ ] `just pr-merge <n>` performs the full loop on a real PR, including a `BEHIND`
      rebase, leaving no junk merge commit or diverged `develop`.
- [ ] `pr-wait` keeps waiting while `statusCheckRollup` is empty (checks not yet
      registered) and only reports green once every entry is `SUCCESS`/`NEUTRAL`.
- [ ] `just review-pane <branch>` opens a pane, the reviewer writes a verdict file,
      and the pane remains interactive afterward.
- [ ] `/implement <issue#>` completes one real issue end-to-end.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `rift-app` CI build pulls skia/wgpu — long build, runner disk | `cargo check` (not full build) + `Swatinem/rust-cache`; accept the cost as the price of closing the only correctness gap |
| `send-keys` fragility for reading results back | File-based verdict; `capture-pane -p` only as a liveness fallback |
| tmux/WSL brittleness | Reviewer uses plain `tmux` + interactive `claude`, no agent-teams dependency; `$TMUX` presence asserted by the recipe |
| `gh pr merge` race when `develop` moves under an open PR | `pr-merge` loops rebase-on-`BEHIND` until `CLEAN`, then merges |

## Decision log

- `set-issue-status.sh` resolves the board item + project id from the issue's own
  `projectItems` (one GraphQL hop) instead of paging the project's `items` and
  matching `content.number`. Same runtime-resolution outcome with fewer calls, and
  it drops the project-id hardcode entirely — portable to any repo/project,
  overridable with `RIFT_PROJECT_NUMBER` when an issue sits on several boards. —
  2026-06-08
- `review-pane` seeds the reviewer with a single-line prompt via `send-keys -l`
  (literal) after polling `#{pane_current_command}` until claude has replaced the
  launching shell, rather than a blind sleep or a fragile multi-line send. The
  pane id is stored in `.claude/review-<branch>.pane` so `review-pane-rm` (called
  best-effort from `pr-merge`) can close it. Caveat: a fresh worktree may trigger
  claude's one-time folder-trust prompt, which intercepts the first send — accept
  it and re-run `review-pane`. — 2026-06-08
