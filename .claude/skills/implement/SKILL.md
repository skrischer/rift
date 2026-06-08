---
name: implement
description: Drive a single rift issue through the full implementation cycle end-to-end — orient on the issue and its READY spec, create an isolated worktree (flipping the issue to In Progress), implement and verify headless, commit/push/open a PR, run the interactive reviewer pane, then merge via pr-merge and mark the issue Done. Use when the user runs /implement <issue#> or asks to implement/work a specific rift issue from start to merged.
---

# /implement — drive one issue from start to merged

Orchestrates the rift issue -> merged cycle using the project's own recipes
(`just agent-worktree`, `just pr-merge`, `just review-pane`) and
`scripts/set-issue-status.sh`. The argument is the issue number, e.g.
`/implement 76`. (`just pr-merge` waits on the checks via `pr-wait` internally.)

**Git autonomy:** implement, verify, commit, push and open the PR without
pausing. **Stop and ask only at the merge gate** — and on any error or blocker.
Never merge until the human confirms after seeing the reviewer's verdict.

## Preconditions

- Run from the main checkout, on `develop`, with a clean tree. Check
  `git rev-parse --abbrev-ref HEAD` and `git status -sb`; if not on a clean
  `develop`, stop and ask.
- Inside a tmux session — the reviewer pane needs it. Check `$TMUX`; if unset,
  warn that the review gate will be unavailable.

## 1. Orient

- `gh issue view <n>` — read the issue and its acceptance checklist.
- Read the referenced `docs/spec-*.md`. It must be `READY` (never act on a
  `DRAFT`). The spec owns the design; the issue owns the step.
- A `feat:`/`fix:` PR must close an issue tracing to a spec (the `planning-gate`
  check). `chore:/docs:/refactor:/test:/ci:/build:/perf:` are exempt.

## 2. Plan first

- For non-trivial work, lay out a short plan and confirm the approach before
  implementing. Prefer reusing existing patterns; build the minimum the issue
  needs. Use AskUserQuestion only at a genuine fork — a design decision the spec
  and code do not settle — not for choices with an obvious default.

## 3. Branch and start

- Pick a branch: `feat/<scope>`, `fix/<scope>` or `chore/<scope>`; `<scope>` is
  the crate name where one applies.
- `just agent-worktree <branch> <n>` — creates `../rift-worktrees/<branch-dashes>`
  off develop and flips the issue to **In Progress** in one step.

## 4. Implement (headless, in the worktree)

- Work in `../rift-worktrees/<branch-dashes>`. Read existing code first, reuse
  utilities, keep the change minimal. Follow CLAUDE.md: agent-agnostic core, no
  `.unwrap()` in library code, respect crate boundaries, no `clone()` to satisfy
  the borrow checker, no `todo!()`/`unimplemented!()` in merged code, no emojis.

## 5. Verify

- In the worktree: `just lint && just test`, fix until green. The GPU app is not
  built headless — app-affecting changes are caught by the CI `app-check` job.

## 6. Commit, push, open the PR (no pause)

- Commit with Conventional Commits (scope = crate). The body references the spec
  and ends with `Closes #<n>`. Stage specific files; never blind `git add -A`.
- Push the feature branch with `git -C <wt> push -u origin <branch>` — phrased
  this way it does not start with `git push`, so it bypasses the push-guard's
  matcher (a plain `git push` from the main checkout is blocked even for a feature
  branch). Never push to `main`/`develop`.
- `gh pr create --base develop` with a body that restates the change, the
  verification done, and `Closes #<n>`.

## 7. Review gate (hard)

- `just review-pane <branch>` — opens the interactive reviewer in its own pane
  with a fresh context. It writes its verdict to
  `.claude/review-<branch-dashes>.md`.
- Read that file once the first line appears. Relay the verdict. On
  `VERDICT: REQUEST_CHANGES`, address the findings (back to step 4) and push the
  fix before proceeding. Only an `APPROVE` (or an explicit human override) clears
  the gate. If `$TMUX` is unset, ask the human to review manually instead.

## 8. Merge gate (STOP)

- Ask the human to confirm the merge. On confirmation: `just pr-merge <n>` — it
  waits for green (refreshing the branch server-side when behind), squash-merges,
  closes the review pane, removes the worktree and both branch refs, and ff-syncs
  local develop.

## 9. Close out

- `scripts/set-issue-status.sh <n> Done` — the merge auto-closes the issue; this
  flips the board column.
- Add any decisions made during implementation to the spec's Decision log.
- If the spec's verification is now fully met, set it `COMPLETED` with the date,
  move it to `docs/archive/`, and update `docs/roadmap.md`.

## If blocked

- Stop immediately and ask — do not invent workarounds. Set the spec status to
  `BLOCKED` with the reason and comment the blocker on the issue.
