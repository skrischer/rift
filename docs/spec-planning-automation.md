# Spec: Planning workflow automation

> Status: READY
> Created: 2026-06-08
> Completed: —

Automate the planning cycle (readiness → `READY` spec + milestone + issues) via a
`just plan-issues` recipe and a `/plan` skill — the planning-side sibling to the
completed implementation-workflow automation, filling the slot it reserved.

## Outcome

- [ ] `just plan-issues <spec> <step-list>` creates the milestone (spec link in its
      description) and one issue per step — each carrying the spec path and an
      acceptance checklist — and adds every issue to the board as `Todo`, replacing
      the hand-rolled `gh issue create` / `gh project item-add` loop.
- [ ] `/plan <scope>` drives one planning cycle from the readiness survey to a
      merged `READY` spec with milestone, issues and roadmap updated — stopping only
      at the genuinely-open-decision gate and the merge gate.
- [ ] Open design questions are sorted into precedent-decided, constraint-determined
      and genuinely-open; only the genuinely-open ones surface via `AskUserQuestion`,
      and they are never guessed.
- [ ] A spec reaches `READY` only after a fresh-context review verdict, and the
      `DRAFT` → `READY` flip lands in the same PR as the review.
- [ ] Milestone and issues are created only after the spec merges to `develop`, so
      `issue-spec-check` (resolving against the default branch) never flags them
      `needs-spec`.

## Scope

### In scope

- `just plan-issues <spec> <milestone-title> <step-file>` — milestone create
  (idempotent on title) + one issue per step (Spec / Goal / Acceptance, matching the
  issue form fields) + board add as `Todo`, reusing `set-issue-status.sh` and the
  gh-2.45 workarounds. The `<step-file>` is a markdown file: one `## [scope] Title`
  heading per step, with a `Goal:` line and an `Acceptance:` checklist beneath; the
  recipe splits on the headings and injects the spec path into each issue body.
- `.claude/skills/plan/SKILL.md` — the `/plan` orchestration skill, mirroring
  `/implement`'s shape: preconditions, numbered phases, hard gates, if-blocked. It
  inlines the happy-path commands and references `docs/planning-workflow.md` for the
  deep pitfalls (gh quirks, `git -C` push-guard) rather than duplicating them.
- The review gate runs through the in-session **Agent tool**
  (`code-reviewer` / general-purpose), distinct from `/implement`'s tmux
  review-pane.

### Out of scope

- Changes to the rulebook (`handover-conventions.md`) or the CI chain
  (`issue-spec-check`, `planning-gate`) — the conventions are settled; this
  automates them, it does not redesign them.
- A spec scaffolder (`just plan-spec`) that templates a new `spec-*.md` — the
  planner writes the spec by hand from `spec-template.md`; templating design prose
  buys nothing.
- Auto-resolving genuinely-open decisions — those stay a human gate by design.
- The implementation side — already shipped in
  `archive/spec-workflow-automation.md`.

## Constraints

- `claude -p` / headless `--print` is forbidden anywhere (billing change
  2026-06-15) — the review gate uses the in-session Agent tool, never a headless
  `claude`.
- `gh` 2.45.0 quirks (no `--json` on `gh pr checks`, no `gh pr update-branch`,
  auto-merge disabled) are already encapsulated by `pr-wait` / `pr-merge`; the
  planning side reuses those recipes unchanged for its own spec PR.
- `issue-spec-check` resolves the spec path against the **default branch**, so the
  milestone and issues can only be created after the spec has merged to `develop`.
- `planning-gate` exempts `docs:` and `chore:` PRs from the closes-an-issue
  requirement, so the spec PR (`docs:`) and the tooling PRs (`chore:`) need no
  closing issue — but the planning tooling still gets a milestone and issues for
  tracking parity with `spec-workflow-automation`.
- Board ids, the push-guard, and the `git -C <worktree>` discipline are identical
  to `spec-workflow-automation` and are reused, not re-derived.
- No new dependencies. The recipe uses only `just`, `gh`, `git`, and the existing
  `set-issue-status.sh`.
- The spec is authored in a `docs/<scope>` worktree and operated via `git -C` (the
  push-guard blocks pushes whose cwd resolves to the protected main checkout).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Planning review gate = in-session Agent tool (`code-reviewer` / general-purpose), not the tmux review-pane | A spec review is a one-shot `READY`/`NEEDS CHANGES` verdict with no need for a lingering interactive pane; the Agent tool returns structured output, drops the `send-keys`/quoting fragility the runbook flagged for the pane, and is not `claude -p` (no billing concern). Runbook optimization #1. | 2026-06-08 |
| `plan-issues` is a `just` recipe (mechanics); the cycle is the skill (orchestration) | Same split as `spec-workflow-automation`: recipes stay standalone-testable and reusable outside the skill. | 2026-06-08 |
| Milestone + issues are created after the spec merges, never in the spec PR | `issue-spec-check` resolves the spec path against the default branch; an unmerged spec would flag every issue `needs-spec`. | 2026-06-08 |
| The roadmap update is its own `docs:` PR after the milestone/issues exist | Folding it into the spec PR would force omitting the concrete `#NN` milestone/issue links (they do not exist pre-merge); a separate PR keeps the links live. Runbook optimization #4 trade-off. | 2026-06-08 |
| `/plan` runs readiness → spec → PR → review autonomously and stops only at the genuinely-open-decision gate and the merge gate | Mirrors `/implement`'s autonomy split: the routine steps flow, the two judgment/irreversible points (an unsettled design decision; the merge) stay human. | 2026-06-08 |
| `plan-issues` step-list = a markdown file, one `## [scope] Title` per step with a `Goal:` line and an `Acceptance:` checklist | Resolved at the review gate (`AskUserQuestion`) over a TSV and a heredoc DSL: markdown is human-readable, diff-friendly, carries multi-line acceptance checklists natively, and needs no parser dependency (section split + field read). | 2026-06-08 |

## Tracking

- Milestone: Planning automation (created once this spec is `READY`)
- Issues: created from this spec after it merges to `develop` — one per step. Note
  the bootstrap: `plan-issues` cannot create its own issues, so this spec's
  milestone and issues are hand-rolled once (the last hand-rolled run); every spec
  after this one uses `plan-issues`. The step decomposition lives only as issues,
  not here.

## Verification

- [ ] `just plan-issues` on a throwaway spec creates the milestone and N issues,
      each carrying the spec path and an acceptance checklist, all added to the board
      as `Todo`; a re-run does not duplicate the milestone.
- [ ] Every generated issue passes `issue-spec-check` (its spec ref resolves) — no
      `needs-spec` label.
- [ ] `/plan <scope>` drives the readiness → merged-`READY`-spec → milestone+issues →
      roadmap cycle, pausing only at the two gates (observed on a trial spec, since
      this spec's own issues are hand-rolled per the bootstrap).
- [ ] The review gate yields a structured `READY` / `NEEDS CHANGES` verdict from a
      fresh context; `NEEDS CHANGES` blocks the `READY` flip.
- [ ] A genuinely-open decision in the trial spec is surfaced via `AskUserQuestion`,
      not guessed.
- [ ] The spec-authoring half of the cycle (readiness → spec → PR → review) was
      dogfooded to produce this spec; `plan-issues` is first exercised on the next
      spec, and the friction observed feeds the skill.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `plan-issues` bootstrap: it cannot create its own issues | This spec's milestone and issues are hand-rolled once; `plan-issues` is exercised on the next real spec |
| `gh issue create` cannot set the ProjectV2 `Status` in one call | `plan-issues` adds the issue to the board, then leans on the board's built-in `Todo` default (with `set-issue-status.sh` as the explicit fallback), same as the runbook |
| Milestone duplicated on a re-run | `plan-issues` looks up an existing milestone by title before creating |
| The Agent-tool reviewer lacks the repo-wide context a fresh `claude` session would build | Seed it with the PR diff plus the decision docs (`handover-conventions.md`, `prior-art.md`) — the same inputs the runbook's reviewer received |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work
progresses.

- The `plan-issues` step-list input format was the spec's one genuinely-open
  decision, resolved at the review gate via `AskUserQuestion`: a markdown file (one
  `## [scope] Title` per step, `Goal:` + `Acceptance:` beneath) over a TSV
  (multi-line acceptance does not fit one line) and a heredoc DSL (needs a field
  parser). Markdown is diff-friendly and dependency-free. — 2026-06-08
