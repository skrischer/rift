# Spec: Phase 12 — Source-control panel + visual diff

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

A source-control panel that lists the working tree's changed files (from the git status the daemon already streams) and, on selecting one, shows its **visual diff** against HEAD — the review surface of vision Scenario 1 ("the git panel shows a clean diff of everything that changed; you review visually, approve, and move on"). This is the one v1.0.0 cockpit phase that needs a **new daemon capability**: computing and streaming per-file diffs. Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] A **source-control panel** docks into the IDE shell and lists the working tree's changed files (added / modified / deleted / renamed / untracked), grouped and labeled by status — read from the git status the daemon already streams (`UpdateGitStatus`, `RepoState`); no re-derivation.
- [ ] Selecting a changed file shows its **diff against HEAD** in a diff view: removed and added lines are visually distinguished, with correct line context.
- [ ] The diff is **computed on the daemon** (remote-first) and streamed to the client on request — a new capability; the client never reads the repo directly.
- [ ] **Large diffs stay responsive**: the diff view is virtualized (renders only visible rows), so a multi-thousand-line diff scrolls smoothly (the GitComet/Hunk pattern).
- [ ] **Binary / too-large files degrade gracefully**: a binary or oversized change shows a "binary file" / "diff too large" placeholder, never a hang or garbage render.
- [ ] The diff **tracks the live working tree**: after the agent (or the editor) changes a file, re-selecting it — or a lightweight refresh — shows the updated diff; a committed file drops out of the changed list (the existing status stream drives the list).
- [ ] The panel is **read-only and agent-agnostic**: it visualizes git state and diffs; it performs no git write operations (stage/commit/discard) and never inspects agent output.

## Scope

### In scope

- **Daemon diff capability** (`crates/explorer` + `crates/daemon` + `crates/protocol`): compute a per-file diff of the **current on-disk worktree content vs the HEAD blob** (always worktree-vs-HEAD, regardless of staging state — never the index blob) using **gix's blob diff** (enable gix's blob-diff feature — the exact flag, `blob` or `blob-diff`, confirmed at the spike; `gix-imara-diff` is already in the dependency tree, so no new crate). New protocol messages: a `RequestDiff { path }` `ClientMessage` and a `FileDiff { path, ... }` `DaemonMessage` carrying a structured diff (hunks with old/new line ranges + line content and add/remove/context tags), plus binary/too-large sentinels. Computed on request (like `OpenFile`), not pushed.
- **Source-control panel** (`crates/app`): a dockable panel (into a Phase 10 dock zone) listing changed files from the existing client git-status model, grouped by status, each row selecting to open its diff. Reuses the git status already folded onto `WorktreeModel`.
- **Diff view** (`crates/app`): a virtualized diff renderer (`gpui-component` virtual list) showing the streamed hunks with add/remove/context styling from theme tokens; binary/too-large placeholders.
- **Refresh semantics**: re-request the diff on file re-selection and when the status stream marks the open diff's file changed; a file leaving the changed set closes/empties its diff.

### Out of scope

- **Git write operations — stage / unstage / commit / discard / stash / branch** — Phase 12 is **read-only review**. The agent runs git in the terminal (agent-first); rift surfaces the result (vision Scenario 1: "review visually, approve, and move on"). A GUI git-write surface, if ever wanted, is a separate deliberate phase.
- **Side-by-side diff layout** *(OPEN — resolved at the spec-acceptance gate; recommended: unified/inline)*: the presentation style (unified single-column vs. two-column side-by-side) is the one genuinely-open product choice.
- **Diff for the staged-vs-unstaged split as separate views** — v1 shows the working-tree change against HEAD as one review diff; the existing status codes still distinguish index vs worktree in the file list. A VS Code-style staged/unstaged two-group SCM is out (it pairs with staging ops, which are out).
- **Commit history / log / blame / graph** — post-v1.0.0; this phase is the *current* change set, not history.
- **Inline diff decoration in the editor gutter** — the editor already handles inline diagnostics; gutter change-bars are a later editor-track item, not this panel.
- **Merge/conflict resolution UI**.

## Human prerequisites

None. The diff is computed by the daemon from the repo it already watches; no secrets, no provisioning. The only dependency change is enabling gix's `blob` feature (a feature flag on the existing `gix` dependency, not a new crate) — named here, so its addition is spec-sanctioned.

## Constraints

- **Remote-first**: the diff is computed on the daemon (where the repo lives) and streamed; the client never opens the git repo. Mirrors the existing status/LSP split.
- **Reuses the existing git-status stream for the file list**: `UpdateGitStatus` / `RepoState` already flow and fold onto `WorktreeModel`; the panel reads them. The *only* new protocol is the diff request/reply — the file list needs none.
- **Diff computed with gix's blob-diff feature, not a new diff crate**: `gix-imara-diff` is already transitively present; enabling gix's blob-diff feature (exact flag name — `blob` or `blob-diff` — confirmed at the spike) exposes blob diffing. `similar` (MIT/Apache) is the named fallback **only** if gix's blob-diff API proves insufficient at the pinned `gix 0.84` — flagged so either is spec-sanctioned, but gix-first (no new top-level crate) is the intent.
- **Virtualized diff rendering is mandatory** (prior-art: GitComet/Hunk OOM'd on naive 500k-line diffs): the diff view renders only visible rows; the daemon caps or sentinels oversized diffs so neither side materializes an unbounded structure.
- **Binary and oversized handling is explicit**: the daemon detects binary (non-UTF-8 / gix binary heuristic) and applies a size ceiling — a concrete initial cap of **~20 000 changed lines or ~2 MB per file** (tied to the Hunk 25k-line perf bar; the exact constant is pinned in the diff-compute issue) — returning a sentinel the client renders as a placeholder, never a partial/garbled diff.
- **Read-only, agent-agnostic** (constitution/vision): no git write path, no agent detection. The panel derives only from git status + computed diffs.
- **Depends on Phase 10 (dock shell)**: the panel docks into the **right dock** — Phase 10 reserved the right + bottom zones for exactly these signal panels, and the right dock sits opposite the left explorer so the change list and the tree are visible together (problems, Phase 13, takes the bottom). Milestone depends on Phase 100.
- **Protocol addition is deliberate** (`CLAUDE.md` rule 5): adding `RequestDiff`/`FileDiff` to `protocol` is a considered API change; the diff type is serde-serializable and tested (valid + binary + empty + large).
- **No `.unwrap()` in library code**; `thiserror` in `crates/explorer`/`crates/daemon` libs; no `todo!()`.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-12 index row and Category 6 (Git integration) anchor this spec.

- **`Auto-Explore/GitComet` + `smolcars/hunk` `crates/hunk-git` — reference** (diff virtualization on huge files; both GPUI-native): the "render only visible diff rows, cap/sentinel oversized diffs" pattern is adopted verbatim as a constraint. Hunk's perf harness (25k-line diffs) is the bar.
- **`zed` `crates/git` — reference** (GPL-3.0, study-only): the `GitStore` diff-between-local-and-remote synchronization pattern for a diff computed remotely and shown locally.
- rift-local grounding: git status lives in `crates/explorer/src/git.rs` (gix); the protocol streams `UpdateGitStatus`/`RepoState` but **no diffs** — confirming the diff capability is genuinely new. `gix 0.84` is pinned with features `status/dirwalk/revision/sha1` (no `blob` yet); `gix-imara-diff` is already in `Cargo.lock`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Read-only review; no git write operations (stage/commit/discard)** | Vision + agent-first: the agent runs git in the terminal; rift surfaces the result ("review visually, approve, and move on"). A GUI git-write surface is a separate deliberate phase, not bundled into the review panel. | 2026-07-02 |
| **Diff computed on the daemon, streamed on request** | Remote-first (constitution): the repo lives on the remote; the client never opens it. Request/reply (like `OpenFile`) not push — a diff is only needed for the file being reviewed. | 2026-07-02 |
| **File list reads the existing git-status stream; only the diff is new protocol** | Constraint: `UpdateGitStatus`/`RepoState` already fold onto `WorktreeModel`. Re-deriving the change list would duplicate a working signal. | 2026-07-02 |
| **Diff via gix `blob` feature (gix-imara-diff), not a new crate; `similar` named fallback** | Minimal-dependency: `gix-imara-diff` is already transitively present; a feature flag beats a new top-level crate. `similar` is named only as a spec-sanctioned fallback if gix's API is insufficient at `0.84`. | 2026-07-02 |
| **Working-tree-vs-HEAD as one review diff; no separate staged/unstaged views** | The staged/unstaged split pairs with staging ops (out of scope); v1 reviews "what changed since HEAD" as one diff, with the status codes still labeling index vs worktree in the list. | 2026-07-02 |
| **Virtualized diff + binary/too-large sentinels** | Prior-art (GitComet/Hunk): naive full-diff rendering OOMs on large files; both the daemon (cap/sentinel) and client (virtual list) bound the work. | 2026-07-02 |
| **Diff presentation: unified/inline vs side-by-side** | **OPEN — resolved at the spec-acceptance gate.** Recommended: unified/inline (space-efficient — the shell already splits explorer/editor/terminal; side-by-side doubles the diff's width demand). The user's product call. | OPEN |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 12 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 120 — Source-control panel)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] Diff-capability tests (`crates/explorer`/`crates/daemon`): computing a diff for a modified file yields correct add/remove/context hunks vs HEAD; an added file diffs against empty; a deleted file diffs to empty; a binary file returns the binary sentinel; an oversized diff returns the too-large sentinel; the `FileDiff` type round-trips serde (valid + binary + empty)
- [ ] The panel lists exactly the changed files from the status stream, grouped/labeled by status; committing a file (in the terminal) removes it from the list on the next status tick
- [ ] Selecting a changed file renders its diff with correct add/remove styling and context; re-selecting after an edit shows the updated diff
- [ ] A multi-thousand-line diff scrolls smoothly (virtualized) — manual QA; a binary/too-large file shows the placeholder
- [ ] `grep` confirms no git write path and no agent detection introduced
- [ ] Milestone QA (dev channel): Scenario 1 review flow — the agent edits files, the panel lists them, the diffs read cleanly, a commit clears them

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gix's blob-diff API at `0.84` is awkward or missing behind the `blob` feature | Spike enabling the `blob` feature and diffing one file before building the panel; `similar` is the named, spec-sanctioned fallback if gix falls short — no re-planning needed. |
| Large-diff performance (render or transport) | Daemon caps/sentinels oversized diffs; client virtualizes; validate on a large real diff (the perf bar is Hunk's 25k-line harness). |
| Diff staleness vs the live working tree | Request/reply model re-fetches on selection and on the status stream marking the file changed; the diff is never assumed current without a (re)request. |
| Renames: a rename may show as delete+add | The status stream already models renames; the list labels them; the diff shows the content change of the new path. Exact rename-diff fidelity is acceptable at add/delete granularity for v1. |
| Protocol churn (`RequestDiff`/`FileDiff`) ripples to both sides | One deliberate `protocol` addition, serde-tested; the daemon handler and client consumer land in the same milestone. |
| PR size: protocol + daemon + panel + diff view | Decompose into ~400-line issues: (1a) protocol diff types + gix blob-diff compute in `crates/explorer` + serde/diff tests; (1b) daemon `RequestDiff`→`FileDiff` handler + wiring; (2) source-control panel (file list) into the right dock; (3) virtualized diff view + binary/large sentinels; (4) refresh/live-tracking wiring. Issue 1 is split (1a/1b) because compute + protocol + tests alone approaches the size ceiling (cf. `git.rs` ~550 lines). |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Review gate (fresh-context Agent review) — `APPROVE`, no blocking findings. Non-blocking folded in: diff-baseline wording tightened (always current on-disk worktree vs HEAD, never the index blob); the gix feature-flag name qualified (`blob`/`blob-diff` confirmed at the spike); a concrete oversized-diff ceiling added (~20k changed lines / ~2 MB, pinned in the compute issue); dock placement decided (right dock, opposite the explorer; problems takes bottom); issue 1 split into 1a (protocol + compute + tests) / 1b (daemon handler) since compute alone approaches the size ceiling. Reviewer independently verified: `git.rs` computes status only (no diffs); the protocol has no diff message and `OpenFile→FileContent` is the mirrored request/reply precedent; `gix 0.84` has no blob feature enabled and `gix-imara-diff` is already transitively present (no new crate); the spec-named-dependency mechanism is correctly applied.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 12). Grounded on `crates/explorer/src/git.rs` (gix status, no diffs), the protocol git types (`UpdateGitStatus`/`RepoState`, no diff), and `gix 0.84` (no `blob` feature yet, `gix-imara-diff` already in `Cargo.lock`). Constraint/precedent-determined: read-only review (agent-first/vision); daemon-computed diff streamed on request; file list from the existing status stream; gix `blob` diff (fallback `similar`); working-tree-vs-HEAD as one review diff; virtualized rendering + binary/too-large sentinels; depends on Phase 10 for a dock zone. One genuinely-open item carried to the gate: unified/inline vs side-by-side diff presentation.
