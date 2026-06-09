# Spec: Trial — /plan skill smoke test (DELETE ME)

> Status: DRAFT
> Created: 2026-06-09
> Completed: —

Throwaway spec used to exercise the `/plan` skill end-to-end before marking
`spec-planning-automation` COMPLETED. Not a real feature — torn down after the run.

## Outcome

- [ ] The `/plan` skill drives a spec from readiness to a reviewed `READY` state and
      then generates a milestone and issues via `just plan-issues`.

## Scope

### In scope

- A single trivial planning step, used only to verify the cycle mechanics.

### Out of scope

- Any real rift behavior — this spec is deleted immediately after the trial.

## Constraints

- Must not be merged into `develop`; the trial tears it down instead.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Trial spec is never merged | It exists only to drive the skill once; merging it would pollute develop history | 2026-06-09 |
| **OPEN — resolved at the review gate:** the trial step's scope label | Contrived open point to exercise the genuinely-open-decision gate | 2026-06-09 |

## Tracking

- Milestone: Trial smoke (created during the run, deleted after)
- Issues: created from this spec by `just plan-issues`, then deleted.

## Verification

- [ ] `just plan-issues` creates the milestone and one issue with the spec path
      injected, added to the board as `Todo`.
- [ ] The Agent-tool review gate returns a structured verdict.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Leftover trial artifacts | Full teardown: delete issues + milestone, close PR unmerged, remove branch/worktree |

## Decision log

- (none yet)
