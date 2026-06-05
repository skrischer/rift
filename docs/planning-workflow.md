# Planning workflow runbook

> Operational companion to [handover-conventions.md](handover-conventions.md).

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

```bash
# poll checks — this gh version has NO `gh pr checks --json`:
gh pr view <n> --json mergeable,mergeStateStatus,statusCheckRollup \
  --jq '{mergeable,state:.mergeStateStatus,checks:[.statusCheckRollup[]?|{name,conclusion}]}'
```

- Branch protection requires the branch to be up to date. On `BEHIND`:
  `git -C <wt> merge origin/develop --no-edit && git -C <wt> push`, then re-wait.
  (`gh pr update-branch` does not exist in this gh version; repo auto-merge is disabled, so
  `--auto` is rejected.)
- On `CLEAN`: `gh pr merge <n> --squash --delete-branch`.

> Pitfall: `develop` moves under you via parallel merges → a `BEHIND` race. Loop:
> rebase-on-develop + push until `CLEAN`, then merge. Run the wait loop with
> `run_in_background` — foreground `sleep` chains are blocked by the harness.

## Phase 6 — Post-merge cleanup

```bash
just agent-worktree-rm docs/<scope>
# update local develop — depends on the main checkout's state:
git fetch origin develop:develop      # if develop is NOT checked out anywhere
git pull --ff-only origin develop     # if develop is active in the main checkout
git branch -D docs/<scope>            # squash-merged -> only -D works; needs explicit approval
```

> Pitfall: the main checkout (the GPU station) is often **detached** (a visual review of
> someone else's branch in progress). Never assume it is on `develop`; never touch it. The
> refspec fetch `develop:develop` fails if `develop` happens to be checked out — fall back
> to `pull --ff-only` there.

## Phase 7 — Milestone and issues (only AFTER the spec is merged)

`issue-spec-check` resolves the spec path against the **default branch** (`develop`), so
the spec must already be merged there — otherwise every issue is flagged `needs-spec`.

```bash
gh api repos/:owner/:repo/milestones -X POST \
  -f title="..." \
  -f description="... Design: [spec-<scope>.md](https://github.com/skrischer/rift/blob/develop/docs/spec-<scope>.md)"
gh issue create --title "[scope] ..." --label implementation --milestone "..." --body-file step.md
gh project item-add 1 --owner skrischer --url <issue-url>   # Status auto-defaults to Todo
```

Each issue body must contain the spec path (`docs/spec-*.md`), an acceptance checklist that
mirrors the spec's Verification, and dependency refs (`#NN`). The project board's built-in
workflow sets `Status: Todo` on add — no manual GraphQL needed.

## Phase 8 — Roadmap

Move the phase to `READY` in [roadmap.md](roadmap.md) and link the milestone and issues —
via its own `docs:` worktree + PR (Phases 3–6 again).

## Optimizations

Identified after running the cycle end-to-end. Not yet implemented — captured here so they
are not rediscovered.

1. **Reviewer as an agent, not a tmux pane.** Drive Phase 4 through the `Agent` tool
   (`code-reviewer` / general-purpose) or `/code-review` instead of `command claude` in a
   split pane reporting via `send-keys`. Structured return value, no alias/quoting/newline
   fragility, no manual pane lifecycle. The tmux variant is visible and pretty but brittle.
2. **A `just pr-merge <n>` recipe.** Encapsulate the Phase 5/6 loop — poll checks →
   rebase-on-`BEHIND` → squash-merge → remove worktree → fast-forward `develop` — including
   the gh-version workarounds. Hand-rolled twice already; clear candidate.
3. **A `just plan-issues <spec>` recipe.** Create the milestone + issues from a small step
   list and add them to the board, instead of N hand-written `gh issue create` calls.
4. **Fold the roadmap update into the spec PR where possible.** Bundling the `READY` flip
   and the roadmap edit in one PR removes the entire second PR cycle (Phase 8). Caveat:
   milestone/issue numbers do not exist until after the merge (the default-branch
   spec-check needs the spec merged first), so either omit concrete `#NN` links from the
   spec PR, or keep a separate roadmap PR when the links are wanted. Document the trade-off.
5. **gh version quirks, recorded once.** No `--json` on `gh pr checks`; no
   `gh pr update-branch`; repo auto-merge disabled (no `--auto`). Listed here so nobody
   rediscovers them.
