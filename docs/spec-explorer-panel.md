# Spec: Phase 11 — Explorer panel (decoration, reveal, keyboard nav)

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

Turn the navigate-and-open file tree into a real IDE explorer: git-status + diagnostic decoration rolled up onto ancestor directories, ignored files shown (dimmed) instead of hidden (#309), reveal-the-active-file, and full keyboard navigation — all built on a precomputed decorated-row cache that also fixes the interaction freeze (rows are recomputed twice per frame today). Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)); this is where the daemon's already-streamed git/diagnostic signals first become **visible on the tree**.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] Each tree row shows its **git status** (e.g. modified / added / untracked / deleted) via color/badge, and its **diagnostic severity** (error / warning) via an indicator — read from the client worktree model the daemon already streams; no new protocol.
- [ ] Directory rows **roll up** their descendants' status: a collapsed folder containing a modified file or an error shows the strongest git status / highest severity among everything beneath it.
- [ ] **Ignored files appear in the tree, dimmed** — not hidden (#309). The daemon streams ignored entries with `ignored = true`, excluding only an explicit performance set (`target/`, `.git/`, `node_modules/`), and no longer honors ripgrep `.ignore` files for the tree (so this repo's `docs/`, `*.md`, configs are openable again).
- [ ] The **active editor file can be revealed** in the tree: its ancestor directories expand, the row scrolls into view and becomes selected (a "reveal active file" action).
- [ ] **Keyboard navigation** works with the tree focused: up/down move the selection, left/right collapse/expand (and step to parent/first-child at the edges), Enter opens the selected file (or toggles a directory), Home/End jump to first/last visible row.
- [ ] The tree **no longer recomputes the visible-row list twice per render**; a precomputed decorated-row cache, rebuilt on model change (not per frame), backs both the virtual list's sizing and its row rendering — the interaction that froze the app (open a file, then hover the next) is smooth.
- [ ] The explorer stays **agent-agnostic**: it reads only paths, kinds, git status, diagnostics, and ignored flags from the model — never agent output, never file contents beyond what the editor already opens.

## Scope

### In scope

- **Precomputed decorated-row cache** (`crates/app`): a `Vec` of rows built once per model change (the zed `EntryDetails` pattern), each carrying path / kind / depth / display name / git status / rolled-up severity / ignored flag. `render()` and the `v_virtual_list` closure read the cache; neither recomputes `visible_rows()` per frame. This is the freeze fix and the decoration foundation in one.
- **Decoration rendering**: git-status color/badge + diagnostic-severity indicator per row, using the existing `gpui-component` theme tokens; ancestor roll-up computed while building the cache.
- **Ignored-files display + daemon scan fix (#309)**: change the worktree scanner (`crates/explorer/src/snapshot.rs`, `ignore::WalkBuilder`) to **include** ignored entries marked `ignored = true`, excluding only a small explicit perf set (`target/`, `.git/`, `node_modules/`), and to stop honoring ripgrep `.ignore`. The client dims `ignored` rows. No protocol change — the `ignored` field already exists.
- **Reveal active file**: expand ancestors, select, and scroll the row into view via the existing `VirtualListScrollHandle`; triggered for the currently open editor file.
- **Keyboard navigation + focus**: a focusable tree with the arrow/Enter/Home/End action set above; selection state already exists, this adds movement and expand/collapse by key.

### Out of scope

- **File operations — create / rename / delete / move** *(OPEN — resolved at the spec-acceptance gate; recommended: defer to a dedicated daemon-file-ops concern.)* These are a **write capability**: the protocol has only `OpenFile` (read) and `SaveFile` (write-content) — create/rename/delete/move need new `ClientMessage`/`DaemonMessage` variants + daemon handlers, qualitatively different from this phase's read-only decoration/navigation. The roadmap prose frames Phase 11 as the low-risk client-side quick win reading the existing model; bundling a protocol change would break that and make the phase unreviewable.
- **Drag-and-drop** move/reorder in the tree.
- **Multi-select / marked-entries** and batch operations.
- **Fuzzy filter / search within the tree** — post-v1.0.0 (search) / Phase 16 (command palette) territory.
- **Configurable exclusion sets or an ignore/settings UI** — the perf-exclusion set is hardcoded for v1; configurability waits for Phase 17 (theme & settings). *(#309 asks for "configurable" eventually; v1 hardcodes the minimal set — minimal-solution.)*
- **The dock shell itself** (Phase 10) — this phase decorates/navigates the explorer panel; it does not build the dock.

## Human prerequisites

None. Client-side rendering + a self-contained daemon scan-behavior change; no new dependency, no secrets, no provisioning, no protocol addition.

## Constraints

- **Reads the existing model, no new protocol**: git status, diagnostics (per-server), `ignored`, and mtime are already on `WorktreeModel` (`crates/app/src/worktree.rs`); the decoration renders them. The only daemon-side change is the #309 scan behavior in `crates/explorer` — the `ignored` field is already in the protocol.
- **Cache correctness over the snapshot-as-source-of-truth invariant**: the decorated-row cache is derived state, rebuilt on every model fold (snapshot / update / git / diagnostics), never mutated independently — so it can never drift from the snapshot (the same discipline the current `visible_rows()` keeps, now memoized instead of per-frame).
- **Roll-up is bounded**: computed in one pass while building the cache (ancestors accumulate the max severity / strongest git status of descendants); no per-frame tree walk, no recursion per row.
- **Agent-agnostic** (constitution): decoration derives only from filesystem/git/LSP signals in the model; no agent detection, no output parsing.
- **`ignore` crate stays the walker** (#309): the fix reconfigures `WalkBuilder` (include ignored, mark them, drop `.ignore` honoring, keep an explicit perf-exclusion set) — it does not replace the crate or hand-roll a walker. `target/` must never be walked/watched (~20 GB) — the perf-exclusion set is load-bearing, not cosmetic.
- **Keyboard focus coexists with the dock** (Phase 10): the tree is (or becomes) a focusable panel; its key handling must not steal terminal keystrokes when the terminal panel is active (agent-first).
- **No `.unwrap()` in library code**; `thiserror` in `crates/explorer`, no `todo!()` in merged code; decoration for a status the model does not carry is simply not rendered (no placeholder).
- **Headless-testable**: cache construction + roll-up + the scanner's ignored/excluded classification are pure functions with unit coverage (valid + malformed/edge inputs); interactive reveal/keyboard/decoration visuals are validated at the milestone QA gate.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-11 index row and Category 5 anchor this spec.

- **`zed` `crates/project_panel/src/project_panel.rs` — reference** (GPL-3.0, study-only): the precomputed **`EntryDetails`** cache (one pass per data update, not per frame — directly the freeze fix), the keyboard **action set** (select up/down, expand/collapse, open, reveal), the **git/diagnostic severity roll-up** onto ancestor directories, and `reveal_entry`. rift mirrors the patterns, not the code (tightly coupled to Zed's `Project`/`Worktree`).
- **`sxyazi/yazi`, `Augani/nexus-explorer`, `noh-rs/nohrs`, `broot`** (Category 5) — async-never-block file-manager UX, GPUI + virtual-list tree skeleton, and incremental-filter patterns; background for reveal/scroll and future filtering (out of scope here).
- rift-local grounding: the current tree (`crates/app/src/file_tree.rs`) recomputes `visible_rows()` in both `render()` (for `item_sizes`) and the `v_virtual_list` closure (per invocation) — the amplification this phase removes. Issue **#309** owns the ignored-files finding and its proposed direction (show dimmed, explicit perf-exclusion set, drop `.ignore`), adopted here.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Precomputed decorated-row cache (zed `EntryDetails` pattern); no per-frame `visible_rows()`** | Precedent + constraint: the current double-recompute per render is the interaction-freeze cause; decoration would only worsen a per-frame walk. One cache rebuilt on model change fixes the freeze and is the decoration substrate. | 2026-07-02 |
| **Ancestor roll-up of git status + diagnostic severity** | Precedent (zed project_panel): a collapsed folder must surface a modified/errored descendant, else decoration is useless when collapsed. Computed in the single cache-building pass. | 2026-07-02 |
| **Ignored files shown dimmed; daemon scan includes them (except an explicit perf set); `.ignore` no longer honored (#309)** | Constraint-determined by #309's analysis: standard IDEs show ignored files (dimmed); the current walk over-applies `.gitignore` + ripgrep `.ignore` and hides `docs/`/`*.md`/configs. The `ignored` field already exists; only the walker and the client dimming change. `target/` stays excluded for perf. | 2026-07-02 |
| **Perf-exclusion set hardcoded (`target/`, `.git/`, `node_modules/`); no configurability in v1** | Minimal-solution: #309 asks for configurability "eventually"; v1 hardcodes the minimal set. A settings surface is Phase 17. | 2026-07-02 |
| **Reads the existing client model; the only daemon change is the #309 scan behavior — no new protocol** | Constraint: git status / diagnostics / ignored / mtime are already streamed and folded onto `WorktreeModel`. Decoration is pure client rendering; #309 is a `WalkBuilder` reconfig in `crates/explorer`, not a protocol addition. | 2026-07-02 |
| **File operations (create/rename/delete/move)** | **OPEN — resolved at the spec-acceptance gate.** Recommended: defer to a dedicated daemon-file-ops concern (needs new protocol write variants + daemon handlers). In-scope only if the user wants write operations in this phase, which adds a protocol change to an otherwise read-only rendering/navigation phase. | OPEN |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 11 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 110 — Explorer panel)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] Rows render git-status color/badge and diagnostic-severity indicators from the model; collapsing a folder over a modified/errored descendant shows the rolled-up status
- [ ] Cache tests: building the decorated-row cache from a seeded model yields correct depth / name / git / severity / ignored per row and correct ancestor roll-up; a model fold rebuilds it (no drift); `render()` calls the builder once, not per frame (asserted structurally)
- [ ] Scanner tests (`crates/explorer`): a `.gitignore`d path (e.g. `docs/`, `*.md` via `.ignore`) now appears with `ignored = true` and is openable; `target/`, `.git/`, `node_modules/` are still excluded; a ripgrep `.ignore` no longer hides tree entries
- [ ] Reveal: opening a file deep in the tree expands its ancestors, selects the row, and scrolls it into view
- [ ] Keyboard: up/down/left/right/Enter/Home/End navigate and expand/collapse as specified with the tree focused; with the terminal active, tree keys do not intercept terminal keystrokes
- [ ] Interaction that froze the app (open a file, hover the next) is smooth (manual QA on the dev channel)
- [ ] `grep` confirms no agent detection introduced
- [ ] Milestone QA (dev channel): the explorer reads like an IDE explorer — decoration is legible, ignored files are visibly dimmed and openable, reveal and keyboard nav feel right

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Removing `.gitignore`/`.ignore` hiding re-introduces a `target/` walk (~20 GB) and tanks scan/watch performance | The explicit perf-exclusion set (`target/`, `.git/`, `node_modules/`) is mandatory and tested; `target/` is never walked or watched. Validate scan time on this repo before/after. |
| The decorated-row cache drifts from the snapshot after an incremental update | The cache is rebuilt on every model fold, never mutated in place; a test folds an update and asserts the cache matches a fresh build. |
| Roll-up cost on a 100k-entry tree | Single-pass accumulation while building the cache (O(n), no per-row recursion); the cache is built on data change, not per frame — the same budget the virtual list already assumes. |
| Keyboard focus fights the terminal (agent-first) | Tree key handling is scoped to the focused tree panel; terminal keystroke delivery is unchanged (a Phase 10 constraint carried here). |
| Shares `file_tree.rs` with Phase 10 (panel adapter + render seam) → rebase churn | Sequence after Phase 10 (milestone depends-on #24); the render-cache seam this phase introduces is compatible with the Panel adapter. |
| Scope pressure to add file-ops (the roadmap name says "file ops") | Gate decision; if deferred, file-ops become a dedicated daemon-write concern — the spec's out-of-scope line and the OPEN row make the boundary explicit. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 11). Grounded on the current `file_tree.rs` (double per-frame `visible_rows()` = the freeze), `worktree.rs` (model already carries git/diagnostics/ignored/mtime), the scanner in `crates/explorer/src/snapshot.rs` (`ignore::WalkBuilder`), and issue #309. Constraint/precedent-determined: precomputed decorated-row cache (zed `EntryDetails`) as freeze-fix + decoration substrate; ancestor roll-up; #309 ignored-files fix (show dimmed, explicit perf-exclusion set, drop `.ignore`) with the perf set hardcoded for v1; reveal + keyboard nav; reads the existing model with no new protocol. One genuinely-open item carried to the gate: whether file operations (create/rename/delete/move — a new daemon write capability) belong in this phase or a dedicated deferred concern.
