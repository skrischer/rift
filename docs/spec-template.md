# Spec: [Feature/Phase Name]

> Status: DRAFT | READY | IN PROGRESS | COMPLETED | BLOCKED | DEFERRED
> Created: YYYY-MM-DD
> Completed: —

One-sentence summary of what this spec delivers.

## Outcome

What is true when this work is done? Write observable, verifiable outcomes — not activities.

- [ ] Outcome 1
- [ ] Outcome 2

## Scope

### In scope

- What this spec covers

### Out of scope

- What this spec explicitly does NOT cover (and why, if not obvious)

## Constraints

Technical constraints, existing decisions, and assumptions that affect implementation.

- Constraint 1
- Constraint 2

## Prior decisions

Decisions already made that the implementor must respect. Include rationale so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| Example: Use termy's PaneTerminal | MIT licensed, 14k LOC production-grade, upstream maintained | 2026-05-06 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file — one issue per step, grouped under a milestone. This spec owns the design; the issues own progress. Do not duplicate the step list here.

- Milestone: [Phase/Feature name](<milestone-url>)
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path in its body. A PR may only merge if it closes an issue that traces back here (enforced by the planning gate).

## Verification

How does Claude Code (or the developer) know the entire spec is complete?

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] [Specific behavioral test]
- [ ] [Specific edge case handled]

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Risk 1 | How to handle it |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work progresses.

- YYYY-MM-DD: [Decision and rationale]
