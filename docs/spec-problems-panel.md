# Spec: Phase 13 — Problems panel (project-wide diagnostics)

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

A problems panel that lists every diagnostic the daemon streams — project-wide, grouped by file, sorted by severity — and jumps to a diagnostic's location on click. Surfaces the LSP diagnostics the client model already holds but only renders inline in the editor today. Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)); a pure client-side read of the existing model.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] A **problems panel** docks into the IDE shell (bottom dock) and lists **all** diagnostics the client holds — every file in the model's diagnostics map, not just the open file — grouped by file, each entry showing severity, message, and line:col.
- [ ] Entries are **sorted by severity** (Error > Warning > Information > Hint), files with errors surfacing first; a per-file and a total count (e.g. "N errors, M warnings") is visible.
- [ ] Clicking a diagnostic **jumps to its location**: the file opens in the editor and scrolls to the diagnostic's range — reusing the existing editor open + go-to-position machinery (Phases 4/5).
- [ ] The panel is **live**: as the daemon streams `Diagnostics` updates (an agent introduces or fixes an error), the list updates without manual refresh; a file whose diagnostics clear drops out.
- [ ] The panel is **agent-agnostic and read-only**: it renders diagnostics from the model only; no agent detection, no diagnostic authoring.

## Scope

### In scope

- **Problems panel** (`crates/app`): a dockable panel (Phase 10 bottom dock) that reads `WorktreeModel::all_diagnostics()` (the project-wide `path -> server -> Vec<Diagnostic>` map already streamed and folded), flattens per file, groups by file, sorts by severity then location, and renders each entry (severity icon/color, message, `line:col`, optional source/code). Virtualized if the list is long.
- **Counts**: a per-file and an aggregate error/warning count from the model (the `total` accessor already exists).
- **Jump-to-location**: selecting an entry emits an open-file-at-position signal the workspace routes to the editor (open the file, scroll/select the diagnostic range) — reusing the same path the tree's open and LSP go-to-definition already use.
- **Live updates**: the panel repaints on every `Diagnostics` fold (the workspace already folds them onto the model and notifies); no new stream.

### Out of scope

- **New diagnostics sources or protocol** — the panel renders exactly what the daemon already publishes (`Diagnostics { path, server, items }`); no new capability, no daemon change.
- **Quick-fixes / code actions** — acting on a diagnostic (LSP `codeAction`) is post-v1.0.0 editor-track depth, not this panel.
- **Filtering / search within problems** (by severity, by text) — a later refinement; v1 shows the full sorted list.
- **Inline editor diagnostics** — already shipped (#189); this panel is the aggregate view, not a change to inline rendering.
- **Non-LSP diagnostics** (build output, test failures) — the model holds LSP diagnostics only; other sources are out.

## Human prerequisites

None. Pure client-side rendering of an already-streamed model; no new dependency, no protocol change, no secrets.

## Constraints

- **Reads the existing model, no new protocol**: `WorktreeModel::all_diagnostics()` and the diagnostics count accessor already exist and are exercised by tests; the panel is a new consumer, not a new data path.
- **Reuses the editor open + go-to-position path** for jump-to-location: no new navigation mechanism — the same open-file/scroll-to-range the tree open and LSP definition already drive.
- **Severity order is the enum order** (`DiagnosticSeverity`: `Error, Warning, Information, Hint`) — the same ordering Phase 11's roll-up uses; no ambiguity.
- **Virtualized rendering** if the list can be long (`gpui-component` virtual list) — consistent with the tree and diff views; a project with thousands of diagnostics must not materialize thousands of elements per frame.
- **Depends on Phase 10 (dock shell)**: the panel docks into the bottom zone Phase 10 established. Milestone depends on Phase 100.
- **Agent-agnostic, read-only** (constitution): derives only from the diagnostics model; no agent detection, no authoring.
- **No `.unwrap()` in library code**; no `todo!()`; a diagnostic for a path not in the tree still lists (diagnostics are independent of the tree in the model — the panel does not require the entry to exist as a tree row).

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-13 index row anchors this spec.

- **`zed` `crates/diagnostics/src/diagnostics.rs` — reference** (GPL-3.0, study-only): the grouped-by-file diagnostics list, jump-to-location, and severity sort. rift mirrors the shape; its data already streams via `Diagnostics` and folds onto `WorktreeModel`, so no store to build.
- rift-local grounding: `WorktreeModel::all_diagnostics()` (project-wide `path -> server -> Vec<Diagnostic>`) and the diagnostics count accessor already exist (`crates/app/src/worktree.rs`); the daemon streams `Diagnostics { path, server, items }` (`spec-daemon-lsp.md`, #177); inline editor diagnostics (#189) already consume the same model. LSP servers publish project-wide (e.g. rust-analyzer checks the whole crate), so "project-wide" is real, bounded by what servers publish.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Reads `all_diagnostics()`; no new protocol or daemon change** | Constraint: the project-wide diagnostics map already streams and folds onto the model; the panel is a new consumer. The roadmap frames Phase 13 as reading the existing model. | 2026-07-02 |
| **Grouped by file, sorted by severity then location; per-file + aggregate counts** | Precedent (zed diagnostics): the standard problems-panel shape; severity order is the `DiagnosticSeverity` enum order (no ambiguity). | 2026-07-02 |
| **Jump-to-location reuses the editor open + go-to-position path** | Constraint: opening a file and scrolling to a range already exists (tree open, LSP go-to-definition); the panel emits the same signal, no new navigation mechanism. | 2026-07-02 |
| **Docks in the bottom zone** | Phase 10 reserved bottom for exactly this; problems panels are conventionally bottom (VS Code/Zed), leaving the right dock to source-control (Phase 12). | 2026-07-02 |
| **Read-only aggregate view; no quick-fixes, no filtering in v1** | Minimal-solution: acting on diagnostics (codeAction) is editor-track depth; filtering is a later refinement. v1 is the sorted project-wide list. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 13 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 130 — Problems panel)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] The panel lists diagnostics from every file in the model (not just the open file), grouped by file, sorted by severity then location, with correct per-file + aggregate counts
- [ ] A `Diagnostics` update (error introduced, then fixed) updates the list live; a file whose diagnostics clear drops out
- [ ] Clicking a diagnostic opens its file and scrolls/selects its range in the editor
- [ ] A diagnostic for a path not present as a tree entry still lists (diagnostics independent of the tree)
- [ ] Pure-logic tests: grouping + severity/location sort + counts over a seeded diagnostics map yield the expected ordered list
- [ ] `grep` confirms no agent detection introduced
- [ ] Milestone QA (dev channel): the agent introduces type errors, they appear in the panel with counts, clicking jumps to them, fixing clears them

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Diagnostics volume on a large project floods the panel | Virtualized rendering; sort brings errors first; filtering is a noted future refinement. |
| Jump-to-location races the file load (open then scroll) | Reuse the existing open+scroll path (tree open / go-to-definition already sequence this); a range past the loaded content clamps, no panic. |
| Diagnostics for a path with no tree entry (e.g. a generated or out-of-tree file the server reports) | The panel lists diagnostics independent of tree membership (the model keys them separately); opening such a path degrades gracefully if unreadable. |
| Duplicate diagnostics across servers for one file | The model keys by server; the panel flattens per file — if two servers report the same line, both show (matching inline behavior); dedup is not a v1 goal. |
| PR size | Small phase; decompose: (1) panel + grouped/sorted list + counts from the model; (2) jump-to-location wiring; (3) live-update + virtualization polish. ~200-400-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 13). Grounded on `WorktreeModel::all_diagnostics()` (project-wide map, already streamed), the `Diagnostic` type (`range/severity/message/source/code`), and the existing inline-diagnostics consumer (#189). All decisions constraint/precedent-determined (reads existing model, zed problems-panel shape, reuse editor open+goto, bottom dock, read-only); no genuinely-open decisions — the gate is acceptance + human-prerequisites (none) only.
