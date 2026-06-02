# Planning conventions

Rules for the exchange between **planning** and **implementation**. Both happen in Claude Code — the split is by *session intent*, not by tool. A planning session writes specs and decomposes work into issues; an implementation session writes code against a `READY` spec. The discipline below is what keeps the two honest.

## The chain: design-doc -> issue -> PR

Every piece of implementation progress traces through three layers, each owning exactly one thing. No layer duplicates another.

| Layer | Owns | Source of truth for |
|---|---|---|
| `docs/spec-*.md` | The design: why, what, done-criteria | Outcome, scope, constraints, decisions, verification |
| GitHub milestone | The phase | Progress overview, grouping |
| GitHub issues | The steps | What is open / in progress / done, history |

The step decomposition lives **only** as issues — never as a task list inside the spec. The spec stops at design; issues carry the steps. A PR closes an issue (`Closes #N`); the issue references the spec.

This chain is **mechanically enforced**, not just documented here:
- `blank_issues_enabled: false` + a required Spec field on the issue form — every issue must name a spec.
- `issue-spec-check.yml` flags any issue whose spec reference does not resolve to an existing file (`needs-spec` label).
- `planning-gate.yml` (required status check on `develop`) blocks any `feat:`/`fix:` PR that does not close an issue tracing to an existing `docs/spec-*.md`. `chore:/docs:/refactor:/test:/ci:/build:/perf:` PRs are exempt.

## Project board

The [`rift` project board](https://github.com/users/skrischer/projects/1) visualizes all open issues across milestones. It is a **view, not a source of truth** — it adds no information the chain above does not already own; it only surfaces it.

- One project for the whole repo, never one per milestone. Phases are separated by the built-in **Milestone** field, not by separate boards.
- Every new issue is added to the board and its **Status** set (`Todo` on creation). Built-in fields only — no custom fields unless a real grouping need appears (a `Crate` field was rejected because issue crates are ~80% `terminal` and multi-crate titles can't map to a single select).
- Views are configured in the GitHub web UI (the API cannot create them):
  - **Board** grouped by **Status** — the execution kanban (Todo / In Progress / Done)
  - **By Milestone** grouped by the **Milestone** field — the phase/roadmap split
- The board README links the roadmap and each active spec — links, never copies (specs stay the single source of truth).

When a milestone's issues all close, the board reflects it automatically; no manual cleanup beyond the spec archival in "When work completes".

## Roles by session, not by tool

**Planning session** writes specs, maintains the roadmap, prepares architectural decisions, creates the milestone and issues. It does not write feature code.

**Implementation session** implements from a `READY` spec, updates status after completion, logs decisions made during implementation. It does not restructure specs or invent scope.

Keeping these as separate sessions preserves the review checkpoint: a spec reaches `READY` (a deliberate gate) before any code is written against it. Same tool, same discipline a two-tool split would impose — enforced by the `READY` gate and the CI chain above, not by which app is open.

## File conventions

### Naming

- Active specs: `spec-<scope>.md` (e.g. `spec-phase2d-tabbar.md`, `spec-daemon-filetree.md`)
- Completed specs: move to `archive/` with the same name
- Foundation docs: lowercase, descriptive (`vision.md`, `architecture.md`, `roadmap.md`)
- Reference docs: lowercase, descriptive (`patterns.md`, `protocol.md`)

### Spec format

All specs follow `spec-template.md`. Design sections only: Outcome, Scope, Constraints, Prior Decisions, Tracking, Verification. The Tracking section links the milestone and lists the issues — it does not restate the steps in prose.

## Status markers

Used in spec headers and `roadmap.md`:

- `DRAFT` — being written, not ready for implementation
- `READY` — complete and reviewed; implementation may start; milestone and issues created
- `IN PROGRESS` — implementation underway
- `COMPLETED` — all verification met, with date; spec moved to `archive/`
- `BLOCKED` — cannot proceed, reason documented
- `DEFERRED` — consciously postponed, reason documented

## When a spec is ready for implementation

1. Set status to `READY`
2. Create the milestone and one issue per implementable step (each referencing the spec path). The milestone description ends with a clickable spec link: `Design: [spec-<scope>.md](https://github.com/skrischer/rift/blob/develop/docs/spec-<scope>.md)`
3. Add every new issue to the `rift` project board (see "Project board" below)
4. Update `roadmap.md` to reflect the next planned work
5. The spec is self-contained — an implementation session should not need to ask about scope or constraints

A good spec answers: what is true when this is done? What must NOT be touched? What decisions are already made?

## When work completes

1. PRs close their issues automatically (`Closes #N`); the milestone closes when its issues do
2. Set the spec status to `COMPLETED` with date and move it to `archive/`
3. Add entries to the spec's decision log for any decisions made during implementation
4. If scope changed, note what changed and why
5. Update `roadmap.md`

When blocked: set status to `BLOCKED` with the reason in the spec header, and comment on the affected issue.

## Language

Everything in `docs/` and on GitHub (issues, PRs, commits) is written in English. The codebase is English.
