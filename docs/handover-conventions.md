# Handover conventions

Rules for the bidirectional exchange between Cowork (planner) and Claude Code (implementor) through `docs/`.

## Roles

**Cowork** writes specs, maintains the roadmap, prepares architectural decisions, and tracks status. It does not write code.

**Claude Code** implements from specs, updates status after completion, and logs decisions made during implementation. It does not create or restructure specs.

## File conventions

### Naming

- Active specs: `spec-<scope>.md` (e.g. `spec-phase2d-tabbar.md`, `spec-daemon-filetree.md`)
- Completed specs: move to `archive/` with the same name
- Foundation docs: lowercase, descriptive (`vision.md`, `architecture.md`, `roadmap.md`)
- Reference docs: lowercase, descriptive (`patterns.md`, `protocol.md`)

### Spec format

All implementation specs follow the SDD template in `spec-template.md`. The six required sections are: Outcome, Scope, Constraints, Prior Decisions, Task Breakdown, Verification.

## Status markers

Use these markers in spec headers and in `roadmap.md`:

- `DRAFT` — spec is being written, not ready for implementation
- `READY` — spec is complete and reviewed, Claude Code can start
- `IN PROGRESS` — implementation underway
- `COMPLETED` — all verification criteria met, with date
- `BLOCKED` — cannot proceed, reason documented
- `DEFERRED` — consciously postponed, reason documented

## Cowork -> Claude Code

When Cowork finishes a spec:
1. Set status to `READY`
2. Update `roadmap.md` to reflect the next planned work
3. The spec is self-contained — Claude Code should not need to ask for clarification on scope or constraints

A good spec answers: what is done when this is done? What must NOT be touched? What decisions are already made?

## Claude Code -> Cowork

When Claude Code completes work on a spec:
1. Update the spec's status to `COMPLETED` with date
2. Mark completed steps in the task breakdown with checkboxes
3. Add entries to the decision log for any decisions made during implementation
4. If scope changed during implementation, note what changed and why

When Claude Code encounters a blocker:
1. Set status to `BLOCKED` with reason in the spec header
2. Add a note in the task breakdown at the blocked step

## Roadmap updates

`roadmap.md` is the single overview of project progress. Both sides keep it current:
- Cowork updates it when planning new work or reprioritizing
- Claude Code updates the current phase status after completing a spec

## Language

Documentation in `docs/` is written in English (the project and codebase are English).
