# Spec: Phase 14 — Status bar (branch, ahead/behind, diagnostic counts)

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

A status bar along the bottom of the window showing the current git branch, ahead/behind commit counts, and aggregate error/warning counts — the at-a-glance summary of the repo/diagnostic state the client model already holds. Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)); a pure client-side read of the existing model.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] A **status bar** spans the bottom of the window (below the dock), showing the **current branch** (or a detached-HEAD indicator) and the **ahead/behind** commit counts from `RepoState`.
- [ ] The status bar shows **aggregate diagnostic counts** — total errors and warnings across the project — from the diagnostics model.
- [ ] The bar is **live**: it updates as `RepoState` and `Diagnostics` stream (the agent switches branch, commits, introduces/fixes errors) without manual refresh.
- [ ] Clicking the diagnostic counts **focuses the problems panel** (Phase 13) — a small, high-value integration (a no-op if that panel is absent).
- [ ] Degrades cleanly: no repo → no branch/ahead-behind segment (or a muted "no repo"); zero diagnostics → a quiet "0" or empty counts; never a crash.
- [ ] Agent-agnostic and read-only: renders repo + diagnostic state from the model only.

## Scope

### In scope

- **Status bar view** (`crates/app`): a themed horizontal strip added to the app-root shell composition (the Phase 10 `flex_col`: dock area `flex_1` above, status bar below), with a left group (branch + ahead/behind) and a right group (diagnostic counts). A simple two-group layout — **not** a status-item registration framework (YAGNI; `gpui-component` ships no status-bar component, so this is a small custom strip).
- **Branch + ahead/behind segment**: reads `WorktreeModel::branch()` and `ahead_behind()` (`AheadBehind { ahead, behind }`); detached/no-repo renders a muted state.
- **Diagnostic-counts segment**: total errors + warnings computed from `all_diagnostics()` by severity (a small aggregation; shared with the problems panel's counting if a helper exists — otherwise a local helper).
- **Live updates**: repaints on `RepoState` / `Diagnostics` folds (the workspace already folds and notifies).
- **Click-to-focus-problems**: clicking the counts emits a signal the workspace routes to focus/open the problems panel.

### Out of scope

- **A status-item registration framework / plugin slots** — zed's registration model is over-engineering for a fixed v1 item set; a simple two-group layout suffices.
- **Cursor position / active-file / language / encoding / indentation segments** — editor-context items are a later refinement; v1 is branch + ahead/behind + diagnostics (the roadmap's named set).
- **Interactive git actions from the bar** (push/pull/branch-switch) — read-only, agent-first (the agent runs git in the terminal); the bar reflects state.
- **A new custom titlebar / top chrome** — deferred in Phase 10; unchanged here.
- **New protocol or daemon change** — the branch, ahead/behind, and diagnostics all already stream.

## Human prerequisites

None. Pure client-side rendering of an already-streamed model; no new dependency, no protocol change, no secrets.

## Constraints

- **Reads the existing model, no new protocol**: `branch()`, `ahead_behind()`, and `all_diagnostics()` / `diagnostic_count()` already exist and stream (`RepoState`, `Diagnostics`); the bar is a new consumer.
- **Custom strip, not a rebuilt primitive**: `gpui-component` has no status-bar component (only `title_bar.rs`), so a small themed flex row is the right build — using theme tokens for colors/borders, consistent with the rest of the shell. This is not "rebuilding a primitive gpui-component provides."
- **Per-severity counts are computed** (the model's `diagnostic_count()` is a flat total): errors and warnings are aggregated from `all_diagnostics()` by mapping `DiagnosticSeverity` to an ordinal locally (the shared type derives no `Ord`) — the same computation Phase 13 needs; extract a shared helper if Phase 13 has landed one, else a local helper.
- **Attaches to the Phase 10 shell**: the bar is a row in the app-root `flex_col` below the dock area (the roadmap frames it as reading the model with "no dock dependency" — it is chrome outside the dock zones, but composes into the Phase 10 root). Milestone depends on Phase 100.
- **Agent-agnostic, read-only** (constitution): derives only from repo/diagnostic state; no agent detection, no git write path.
- **No `.unwrap()` in library code**; no `todo!()`; missing repo/diagnostics render muted, never panic.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-14 index row anchors this spec.

- **`zed` `crates/status_bar` — reference** (GPL-3.0, study-only): the left/right status-item layout. rift takes the two-group layout but **not** the registration framework (YAGNI for a fixed v1 set).
- **`zellij` status-bar plugin — reference** (MIT): discoverability-hint status bar; background for future hint items (out of scope now).
- rift-local grounding: `WorktreeModel::branch()` / `ahead_behind()` (`AheadBehind { ahead, behind }`) and `all_diagnostics()` / `diagnostic_count()` already exist (`crates/app/src/worktree.rs`); the daemon streams `RepoState` and `Diagnostics`. `gpui-component` ships no status-bar component, so the strip is custom.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Fixed two-group status bar (branch+ahead/behind \| diagnostic counts); no registration framework** | Minimal-solution + YAGNI: zed's status-item registration is over-engineering for the roadmap's named fixed set; a simple left/right layout is enough and easy to extend later. | 2026-07-02 |
| **Reads the existing model; no new protocol** | Constraint: branch/ahead-behind (`RepoState`) and diagnostics already stream and fold onto `WorktreeModel`; the bar is a new consumer. | 2026-07-02 |
| **Custom themed strip (gpui-component has no status-bar component)** | Constraint: `gpui-component` provides `title_bar` but no status bar; a small themed flex row is the correct build, not a rebuilt primitive. | 2026-07-02 |
| **Per-severity counts computed locally / via a shared helper** | The model's `diagnostic_count()` is a flat total; errors/warnings need per-severity aggregation — the same computation Phase 13 needs, extracted to a shared helper if available. | 2026-07-02 |
| **Click-the-counts focuses the problems panel; no other interactivity** | High-value, cheap integration; a read-only bar otherwise (agent-first — git actions run in the terminal). | 2026-07-02 |
| **Editor-context segments (cursor/file/language) deferred** | Minimal-solution: v1 is the roadmap's named set (branch, ahead/behind, diagnostics); editor-context items are a later refinement. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 14 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 140 — Status bar)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] The status bar shows the current branch and ahead/behind counts from `RepoState`; a detached/no-repo state renders muted
- [ ] The status bar shows total error + warning counts; a `Diagnostics` update changes them live; zero diagnostics renders quietly
- [ ] Switching branch / committing (in the terminal) updates the branch + ahead/behind live
- [ ] Clicking the diagnostic counts focuses the problems panel
- [ ] Pure-logic tests: the error/warning aggregation over a seeded diagnostics map, and the branch/ahead-behind formatting (incl. detached/no-repo), yield the expected strings
- [ ] `grep` confirms no agent detection introduced and no git write path
- [ ] Milestone QA (dev channel): the bar reads correctly as the agent works — branch/ahead-behind track commits, counts track errors

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Duplicated per-severity counting between the status bar and the problems panel | Extract a shared aggregation helper (whichever of Phase 13 / 14 lands first owns it); trivial either way. |
| The bar competes for vertical space with the terminal/editor | It is a thin single-row strip (fixed small height); the dock area takes the remaining space. |
| Attaching to the Phase 10 root shell conflicts if Phase 10 changes | Sequence after Phase 10 (milestone depends on #24); the bar is one added row in the existing `flex_col`. |
| Ahead/behind is `None` with no upstream | Render only the branch then; `None` ahead/behind is a defined state, not an error. |
| PR size | Small phase; decompose: (1) the status-bar strip + branch/ahead-behind + diagnostic counts (read + render + live); (2) click-to-focus-problems wiring. ~200-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 14). Grounded on `WorktreeModel::branch()`/`ahead_behind()` (`AheadBehind{ahead,behind}`) and `all_diagnostics()`/`diagnostic_count()` (flat total), and the absence of a `gpui-component` status-bar component (only `title_bar.rs`). All decisions constraint/precedent-determined (reads existing model, fixed two-group layout not a framework, custom themed strip, per-severity counts computed, click-to-focus-problems); no genuinely-open decisions — the gate is acceptance + human-prerequisites (none) only.
