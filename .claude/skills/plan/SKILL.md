---
name: plan
description: Drive a single rift planning cycle end-to-end — survey readiness, sort open decisions into precedent/constraint/genuinely-open, draft a spec from the template in a docs worktree, open and review its PR via the in-session Agent tool (not a pane), resolve the one genuinely-open decision, flip it READY in the same PR, merge via pr-merge, then generate the milestone and issues with plan-issues and update the roadmap. Use when the user runs /plan <scope> or asks to plan a phase / write a spec for a piece of rift work from readiness to a READY spec with issues.
---

# /plan — drive one planning cycle to a READY spec + issues

Orchestrates the rift readiness -> `READY` spec -> milestone + issues cycle using
the project's recipes (`just agent-worktree`, `just plan-issues`, `just pr-merge`)
and the in-session Agent tool for the review gate. The argument is the scope of the
work to plan, e.g. `/plan daemon-filetree`.

This is the planning-side sibling to `/implement`. Read `docs/planning-workflow.md`
for the deep command detail and pitfalls, and `docs/handover-conventions.md` for the
rulebook (the design-doc -> issue -> PR chain and the status markers).

**Autonomy:** survey, draft the spec, open the PR, and run the review autonomously.
**Stop and hand off at two gates** — the genuinely-open decision (resolve via
`AskUserQuestion`, never guess) and the merge gate — and on any error or blocker.
This mirrors `/implement`'s split: the routine steps flow, the two judgment points
stay human.

## Preconditions

- Run from the main checkout, on `develop`, with a clean tree. Check
  `git rev-parse --abbrev-ref HEAD` and `git status -sb`; if not on a clean
  `develop`, stop and ask.
- No tmux needed — the review gate is the in-session Agent tool, not a pane (unlike
  `/implement`).

## 1. Readiness

- Survey the roadmap, existing specs, open issues and milestones:
  ```
  gh api repos/:owner/:repo/milestones --jq '.[]|"\(.number) \(.title) [\(.state)] open=\(.open_issues)"'
  gh issue list --state open
  ```
- Decide whether this is actually a planning session. If a `READY` spec already
  covers the scope (with a milestone and issues), there is nothing to plan — point
  the user at `/implement <issue#>` instead. Never act against a `DRAFT`.

## 2. Resolve decisions before writing

Sort every open design question into three buckets — most "open" questions are not
actually open:

1. **Precedent-decided** — backed by 2+ reference implementations in
   `docs/prior-art.md`. Adopt and record in the spec.
2. **Constraint-determined** — derivable from this codebase or `CLAUDE.md`. Decide
   it and record the rationale.
3. **Genuinely open** — neither precedent nor constraint settles it. These are the
   only ones that block `READY`; they are resolved at the review gate (step 5), not
   guessed now.

> Check bucket 2 against the codebase before declaring anything "open" — most
> "open" decisions turn out to be already determined.

## 3. Draft the spec

- From `docs/spec-template.md`. Bound the scope tightly. Put settled decisions in
  **Prior decisions** with rationale; mark each genuinely-open point explicitly
  (e.g. an `OPEN — resolved at the review gate` row). **No step list inside the
  spec** — steps live as issues. The `Outcome` list is done-criteria, not a progress
  mirror. Everything in `docs/` is written in English.

## 4. Worktree and PR

- `just agent-worktree docs/<scope>` — branches off develop with its own `target/`.
- Write the spec into the worktree, then operate **only** via `git -C <wt>`, never
  `cd`: the push-guard blocks a push whose cwd resolves to the protected main
  checkout. `git -C` targets the worktree's branch directly and sidesteps it.
  ```
  wt=../rift-worktrees/docs-<scope>
  git -C "$wt" add docs/spec-<scope>.md
  git -C "$wt" commit -m "docs(spec): ..."
  git -C "$wt" push -u origin docs/<scope>
  gh pr create --base develop --head docs/<scope> --title "docs(spec): ..." --body "..."
  ```
- A `docs:` spec PR closes no issue and is exempt from the planning-gate.

## 5. Review gate + resolve the open decision

- Review the spec with a **fresh context via the Agent tool** (`general-purpose`, or
  `code-reviewer`), seeded with the PR diff and the decision docs
  (`handover-conventions.md`, `planning-workflow.md`, the sibling spec it builds on).
  Ask for a verdict whose first line is `VERDICT: READY` or
  `VERDICT: NEEDS CHANGES`, with blocking vs non-blocking findings. Never use
  `claude -p` (billing) — the Agent tool runs in-session.
- Address the findings. **STOP:** resolve each genuinely-open decision via
  `AskUserQuestion` — do not guess. Bake the answer into the spec (the Prior
  decisions row and a Decision log entry).
- Flip the header `DRAFT` -> `READY` **in the same PR** (the PR is the review
  checkpoint, so the spec is reviewed before it is blessed). Commit and push.

## 6. Merge gate (STOP)

- Ask the human to confirm the merge. On confirmation: `just pr-merge <n>` — it waits
  for green (re-polling the transient `UNKNOWN`/`BEHIND` states), squash-merges,
  removes the worktree and both branch refs, and ff-syncs local develop.

## 7. Milestone and issues (only AFTER the spec merges)

- `issue-spec-check` resolves the spec path against the **default branch**, so the
  spec must be merged to `develop` first — otherwise every issue is flagged
  `needs-spec`.
- Write a markdown step-file: one `## [scope] Title` per implementable step, each
  with a `Goal:` line and an `Acceptance:` checklist (mirror the spec's
  Verification). Then preview, then create:
  ```
  PLAN_ISSUES_PREVIEW=1 just plan-issues docs/spec-<scope>.md "<Milestone>" steps.md  # no writes
  just plan-issues docs/spec-<scope>.md "<Milestone>" steps.md
  ```
  `plan-issues` creates the milestone (idempotent on title, spec link in its
  description), one issue per step with the spec path injected, and adds each to the
  board as `Todo`.

## 8. Roadmap

- Update `docs/roadmap.md` to reflect the planned phase, linking the milestone and
  issues — via its own `docs:` worktree + PR (steps 4 and 6 again). The concrete
  `#NN` links only exist after step 7, which is why this is a separate PR from the
  spec; folding it in would force omitting the links.

## Close out

- When the spec's verification is fully met (typically after implementation lands),
  set it `COMPLETED` with the date, move it to `docs/archive/`, repoint any links to
  it, and add implementation decisions to its Decision log.

## If blocked

- Stop immediately and ask — do not invent workarounds. Set the spec status to
  `BLOCKED` with the reason in its header and comment the blocker on the affected
  issue.
