# Planning workflow runbook

> Operational companion to [handover-conventions.md](handover-conventions.md).
> For loop behavior, [workflow.md](workflow.md) (the loopkit contract) supersedes
> this runbook where they differ.

`handover-conventions.md` is the **rulebook** — the design-doc → issue → PR chain, status
markers, roles, and CI enforcement. This file is the **runbook**: the concrete step
sequence of one full planning cycle, the exact commands, and the pitfalls that actually
came up. Read the rulebook for *why*; read this for *how*.

A planning cycle takes one phase of work from "can we start?" to "spec is `READY`,
milestone and issues exist, roadmap reflects it."

## Phase 0 — Check readiness

Before starting new phase work, survey the roadmap, existing specs, open issues, and
milestones.

```bash
gh api repos/:owner/:repo/milestones --jq '.[]|"\(.number) \(.title) [\(.state)] open=\(.open_issues)"'
gh issue list --state open
```

Decision: is there already a `READY` spec, a milestone, issues? If not, this is a
**planning** session, not implementation. The gate before any daemon/feature code is a
`READY` spec on the default branch plus a milestone and issues — none of which a `DRAFT`
satisfies.

## Phase 1 — Resolve decisions before writing

Sort every open design question into three buckets. This is where spike effort is saved —
most "open" questions are not actually open.

1. **Precedent-decided** — backed by 2+ reference implementations in
   [prior-art.md](prior-art.md). Adopt and document in the spec (e.g. file-sync = Zed
   worktree `Snapshot` + incremental updates).
2. **Constraint-determined** — derivable from this codebase or `CLAUDE.md`. Decide it and
   record the rationale (e.g. daemon form is Lapce-flat dispatch because the daemon is a
   headless tokio/musl service with no GPUI, and `CLAUDE.md` mandates a single `State` +
   channels).
3. **Genuinely open** — neither precedent nor constraint settles it. Resolve via a spike
   or `AskUserQuestion`. Only these block `READY`.

> Lesson: check bucket 2 against the codebase before declaring anything "open". In the
> daemon cycle, three of four "open" decisions turned out to be already determined.

## Phase 2 — Draft the spec

From [spec-template.md](spec-template.md). Bound the scope tightly. Put decisions already
made in **Prior decisions** with rationale; mark genuinely-open points explicitly as
out-of-scope or spike. No step list inside the spec — steps live as issues.

## Phase 3 — Isolate in a worktree and open the PR

```bash
just agent-worktree docs/<scope>            # branch off develop, own target/
# write the spec into the worktree, then operate ONLY via git -C:
git -C ../rift-worktrees/<dir> add docs/<file> && git -C ../rift-worktrees/<dir> commit -m "docs(spec): ..."
git -C ../rift-worktrees/<dir> push -u origin docs/<scope>
gh pr create --base develop --head docs/<scope> --title "docs(spec): ..." --body "..."
```

> Pitfall: **always `git -C <worktree>`, never `cd`.** The protected-branch guard blocks
> pushes when the shell's cwd resolves to the main checkout (which sits on `develop` or is
> detached). `git -C` targets the worktree's branch directly and sidesteps the guard.

## Phase 4 — Review gate

Run a reviewer against the PR diff with the decision docs as context; get a verdict of
`READY` or `NEEDS CHANGES`. Address findings. Resolve the one genuinely-open decision via
`AskUserQuestion` — do not guess. Then flip the header `DRAFT` → `READY` **in the same
PR**: the PR is the review checkpoint, so the spec is reviewed before it is blessed.

> Pitfall (if the reviewer is a sub-`claude` in a tmux pane): the `claude` alias starts
> with `-r` (resume) — use `command claude` for a fresh autonomous session. Reporting back
> via `send-keys` is fragile (quoting, and raw newlines submit the prompt early). See
> Optimization 1 for a sturdier approach.

## Phase 5 — Merge

`just pr-merge <n>` runs the whole loop: wait for green (re-polling the transient
`UNKNOWN`/`BEHIND` states GitHub reports right after checks settle), refresh the branch
server-side when `BEHIND`, remote-only squash-merge, remove the worktree and both branch
refs, and fast-forward local `develop`. Run it with `run_in_background` — a `BEHIND` rebase
triggers fresh CI it then waits on, and foreground `sleep` chains are blocked by the harness.

> Pitfall: `develop` moves under you via parallel merges → a `BEHIND` race. `pr-merge`
> already loops rebase-on-`BEHIND` until `CLEAN`, so you never poll or merge by hand. The
> `gh` 2.45 workarounds (no `gh pr checks --json`, no `gh pr update-branch`, no `--auto`)
> live inside the recipe.

## Phase 6 — Post-merge cleanup

`just pr-merge` already removes the worktree, deletes both branch refs, and ff-syncs
`develop` when run from the main checkout — so there is usually nothing to do here by hand.

> Pitfall: the main checkout (the GPU station) is often **detached** (a visual review of
> someone else's branch in progress). `pr-merge` only ff-syncs `develop` when the checkout
> is actually on `develop`; otherwise sync it later with `git fetch origin develop:develop`.
> Never assume the station is on `develop`; never touch it.

## Phase 7 — Milestone and issues (only AFTER the spec is merged)

`issue-spec-check` resolves the spec path against the **default branch** (`develop`), so
the spec must already be merged there — otherwise every issue is flagged `needs-spec`.

`just plan-issues <spec> <milestone-title> <step-file>` creates the milestone (idempotent
on title, with the spec link in its description), one issue per step, and adds each to the
board as `Todo`. The `<step-file>` is a markdown file — one `## [scope] Title` heading per
step, each with a `Goal:` line and an `Acceptance:` checklist beneath (mirror the spec's
Verification). `PLAN_ISSUES_PREVIEW=1 just plan-issues …` prints what would be created with
no GitHub writes. The spec path is injected into every issue body, so each issue resolves
against the merged spec.

## Phase 8 — Roadmap

Move the phase to `READY` in [roadmap.md](roadmap.md) and link the milestone and issues —
via its own `docs:` worktree + PR (Phases 3–6 again).

## Optimizations

Identified after running the cycle end-to-end. Items 1–3 are now implemented (the
planning-automation work, `archive/spec-planning-automation.md`); 4–5 are recorded so they
are not rediscovered.

1. **Reviewer as an agent, not a tmux pane.** — IMPLEMENTED. The `/plan` skill's review
   gate runs through the in-session Agent tool (`general-purpose` / `code-reviewer`),
   returning a structured `READY`/`NEEDS CHANGES` verdict with no alias/quoting/`send-keys`
   fragility and no manual pane lifecycle.
2. **A `just pr-merge <n>` recipe.** — IMPLEMENTED (with the implementation-side automation;
   `chore(pr-merge)` #97 later made it re-poll the transient `UNKNOWN` state). Encapsulates
   poll → rebase-on-`BEHIND` → squash-merge → worktree-rm → ff-sync `develop`, with the
   gh-version workarounds.
3. **A `just plan-issues <spec> <milestone> <step-file>` recipe.** — IMPLEMENTED. Creates
   the milestone + per-step issues from a markdown step-file and adds them to the board,
   with a `PLAN_ISSUES_PREVIEW=1` dry-run, instead of N hand-written `gh issue create` calls.
4. **Fold the roadmap update into the spec PR where possible.** Bundling the `READY` flip
   and the roadmap edit in one PR removes the entire second PR cycle (Phase 8). Caveat:
   milestone/issue numbers do not exist until after the merge (the default-branch
   spec-check needs the spec merged first), so either omit concrete `#NN` links from the
   spec PR, or keep a separate roadmap PR when the links are wanted. Document the trade-off.
5. **gh version quirks, recorded once.** No `--json` on `gh pr checks`; no
   `gh pr update-branch`; repo auto-merge disabled (no `--auto`). Listed here so nobody
   rediscovers them.
