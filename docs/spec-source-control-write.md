# Spec: source-control write path

> Status: READY
> Created: 2026-07-06
> Completed: —

Give rift its first git write capability per the Paper "Git — Diff Review"
artboard: stage/unstage (file and hunk level), commit with message, the
STAGED/CHANGES panel anatomy, the diff header with Split|Unified toggle and
hunk squares, split-diff rendering with word-level emphasis — all through the
daemon (gix), agent-agnostically.

## Outcome

- [ ] The source-control panel matches §4: commit textarea (mono, 2-3 rows) +
      primary `✓ Commit` button with a live `N staged` suffix; `STAGED
      CHANGES` and `CHANGES` sections (driven by the EXISTING index/worktree
      split on `GitStatusEntry`) with count pills, section-level stage-all/
      unstage-all icons, and per-row hover actions (stage/unstage; discard per
      gate decision).
- [ ] Committing the staged set from the panel creates a real commit (author/
      committer from the repo's git config); the panel, explorer decoration,
      status-line totals, and ahead counter converge on the daemon's next
      recompute without any refresh.
- [ ] The diff header matches §4: file name + dir, `+n −m` for the open file,
      hunk mini-squares, a Split|Unified segmented toggle (persisted
      preference), and `+ Stage hunk` on each hunk header.
- [ ] Split view renders two aligned columns with per-side gutters, tinted
      add/delete rows, hatched filler rows, and word-level emphasis inside
      changed lines; Unified stays available via the toggle.
- [ ] Hunk-level staging works: staging one hunk of a multi-hunk file stages
      exactly those lines and leaves the others stageable afterwards (the
      decompose-and-reapply algorithm below); a divergent index (external
      `git add`, staged-then-edited) yields a clean error suggesting
      file-level staging — never a mis-application.
- [ ] All write operations are explicit user actions over the protocol —
      the daemon never writes git state on its own.

## Scope

### In scope

- `protocol` (deliberate API change, version-bumped): request/response pairs
  `StageFile { path }`, `UnstageFile { path }`, `StageHunk { path, hunk_id }`,
  `Commit { message }` → one `GitOpResult { op, ok, error }` reply each, plus
  `DiscardFile { path }` if the gate accepts discard. `hunk_id` is defined
  normatively: the FNV-1a hash (the deploy.rs fingerprint pattern) over the
  hunk's header numbers AND all its lines — a same-shape content change
  yields a different id, so the daemon verifies content identity before any
  application ("a stale id is rejected, never fuzzily applied" holds
  literally). Replies are per-connection (the shipped buffer/diff
  request-reply path, daemon lib.rs:769-787); the resulting state change
  arrives through the existing push-only git recompute — the watcher's
  `.git/index` whitelist already triggers it, no new tick mechanism.
- `daemon`/`explorer` (gix, already the git engine — the index→tree half
  uses gix's `tree-editor` feature, zero new dependencies): stage = write the
  worktree blob into the index at `path` (add for untracked, filters per
  gix's autocrlf pipeline); unstage = restore the index entry from HEAD
  (remove for newly added); commit = build the tree from the index
  (tree-editor), commit with parents=[HEAD], author/committer from config,
  reject an empty message or a NOTHING-STAGED state (index tree == HEAD
  tree — an index is never literally empty in a non-empty repo); a transient
  `index.lock` (live agent) gets one bounded retry, then a clean
  `GitOpResult` error.
- Hunk staging — the decompose-and-reapply algorithm (normative; the
  displayed hunks are worktree-vs-HEAD while application targets the index,
  so bases MUST be reconciled): at op time (1) recompute the file's
  worktree-vs-HEAD hunks fresh and verify the requested `hunk_id` matches
  one of them; (2) recover the already-staged subset S by diffing
  index-vs-HEAD and requiring S to decompose into exact matches of current
  hunks; (3) new index blob = apply(HEAD blob, S ∪ {selected hunk}); (4) if
  S does not decompose (index modified externally / staged-then-edited),
  reject with a clean error that names file-level staging as the fallback.
  This keeps every hunk stageable after the first one and never
  mis-addresses a divergent index.
- `app` (source_control.rs): panel anatomy per §4 — commit box (gpui
  textarea, mono), Commit button + staged count, STAGED/CHANGES sections from
  `GitStatusEntry.index`/`.worktree` (replacing the by-change-type grouping),
  count pills, section icons, row hover actions, path column, letter lane.
- `app` (diff_view.rs): header per §4 (`+n −m` aggregated from the loaded
  hunks, mini hunk squares, Split|Unified segmented control persisted in the
  window-state store), hunk-header `+ Stage hunk` ghost button, split
  renderer (two columns, aligned rows, per-side gutters, hatched fillers)
  with word-level intra-line emphasis (client-side longest-common-subsequence
  on the changed line pairs of a hunk).
- Diff base semantics stay worktree-vs-HEAD (the existing `RequestDiff`
  contract) in v1 — the staged/unstaged VIEW split of the diff itself is out
  of scope (see below).

### Out of scope

- Push/pull/fetch, branch operations, merge/rebase UI (no remote writes in
  v1; the commit-button dropdown segment ships only if the gate accepts
  Amend — see open decision — otherwise the button renders without the
  segment, a recorded deviation).
- Per-side diffs (index-vs-HEAD vs worktree-vs-index views): v1 keeps ONE
  worktree-vs-HEAD diff per file. Honest consequence: already-staged hunks
  keep rendering as changes (worktree still differs from HEAD) with a live
  "+ Stage hunk" (re-staging is a no-op by construction), there is no
  hunk-level UNstage, and `+n −m`/hunk squares aggregate staged+unstaged —
  known v1 semantics, not bugs to file at the QA gate. Revisit with per-side
  views when dogfooding demands them.
- Commit signing (gpg/ssh), hooks execution semantics beyond what gix's
  commit does natively (no hook execution in v1 — documented limitation).
- Multi-repo/submodule support (single root, as everywhere).

## Constraints

- gix only (musl-clean; git2 ruled out by constitution/tech table); no
  shelling out to a git binary.
- All writes go through the daemon protocol — the client never touches the
  repo directly (remote-first).
- Destructive ops (discard, if accepted) require an explicit confirm dialog
  (#420 pattern) and are never batched.
- Theme tokens only; diff tints derive from success/danger at low alpha per
  §0; mono for all code/numbers. The Commit button's check mark is a
  gpui-component icon, never a literal glyph in a string (no-emoji rule).
- Constitution: no `.unwrap()` in libs; protocol documented + tested valid/
  malformed; crate boundaries (gix usage stays in explorer/daemon).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Staged/unstaged sections read the EXISTING `GitStatusEntry.index`/`.worktree` codes | The data model has carried the split since the git-status phase — only the UI grouped by change type; no protocol change needed for the read side | 2026-07-06 |
| Hunk staging = decompose-and-reapply against the HEAD blob (S ∪ selected), never direct application onto the index blob; `hunk_id` = FNV-1a over header + lines | The displayed hunks are HEAD-relative while the write target is the index — direct application is only correct when index == HEAD, which the FIRST staged hunk already breaks (spec-review finding 1). Content-hashed ids catch same-shape edits (finding 2) | 2026-07-06 |
| Git ops reply only ok/error; state arrives via the existing push recompute | One source of truth for git state (the push path); echoing state in replies would create a second sync mechanism | 2026-07-06 |
| Split|Unified preference persists in the window-state store | Established local-persistence pattern (phase 9); a per-file toggle would be noise | 2026-07-06 |
| No hook execution in v1 | gix does not run hooks natively; silently skipping hooks must be documented, not accidental (constitution: no surprising behavior); revisit on demand | 2026-07-06 |
| Word-level emphasis is client-side LCS on hunk line pairs | Presentation concern; the protocol stays line-based (hunks unchanged) | 2026-07-06 |

## Prior art

- `docs/prior-art.md` → Phases 19–26 index, Phase 24 rows: `gix` staging +
  commit APIs (reuse — already the daemon's git dependency); `zed`
  `crates/git_ui` + `gitui` (hunk-staging interactions, reference);
  `smolcars/hunk` + GitComet (split-diff virtualization + intra-line
  emphasis, reference).

## Human prerequisites

None. (Commits use the repo's existing git config identity — already
configured on the host.)

## Tracking

- Milestone: created after this spec merges (phase 24) — `Depends on
  milestone: none`.
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Protocol tests for every new pair (valid + malformed); daemon git-op
      tests on real fixture repos: stage/unstage tracked+untracked+deleted
      (+ exec-bit and symlink paths), commit (empty message and
      nothing-staged rejected), hunk staging on a multi-hunk file: stage
      hunk A then hunk B (both land exactly), stale/content-changed hunk_id
      rejected, externally-divergent index rejected with the file-level hint
- [ ] Behavioral: stage → commit from the panel; `git log`/`git status` on
      the host confirm; panel/explorer/status line converge without refresh
- [ ] Behavioral: stage ONE hunk of a two-hunk file → `git diff --cached`
      shows exactly that hunk
- [ ] Split view: aligned columns, word-level emphasis, hatched fillers;
      toggle persists across restarts
- [ ] Discard (if in scope): confirm dialog, file restored to HEAD, panel
      converges
- [ ] Visual match vs the Git — Diff Review artboard at the QA gate

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gix index/commit API edge cases (filters, CRLF, symlinks, exec bits) | Fixture tests cover text+exec-bit+symlink paths; filters: v1 documents that .gitattributes filters beyond autocrlf follow gix defaults |
| Hunk application drifts when the worktree changed since the diff was pushed | Stale hunk-id rejection + the diff auto-refresh (#488) keep the UI on fresh hunks; the op errors cleanly instead of mis-staging |
| Concurrent agent writes during a git op | Ops are index-only and atomic per gix; the recompute after the op reflects whatever the agent did meanwhile |
| Split renderer perf on huge diffs | Reuse the existing virtualized row rendering; word-level LCS only for VISIBLE hunk line pairs |

## Decision log

- 2026-07-06: Spec drafted from the wave-1 SCM gap analysis (no write path,
  unified-only renderer, header gaps — all CONFIRMED) and the design
  distillation §4; read-side split confirmed already present in
  `GitStatusEntry`.
- 2026-07-06: Fresh-context review (PR #534): both blocking findings baked
  in — the hunk-staging algorithm is now decompose-and-reapply against the
  HEAD blob (direct index-blob application was incorrect the moment the
  index diverged, i.e. right after the first staged hunk), and `hunk_id` is
  a content fingerprint (FNV-1a over header + lines) so same-shape edits are
  caught. Non-blocking adoptions: gix `tree-editor` feature named for the
  index→tree half, "nothing staged" replaces the wrong "empty index"
  predicate, no new recompute tick (the `.git/index` watcher already fires),
  the staged-hunks-still-render consequence documented as v1 semantics,
  icon-not-glyph note, index.lock bounded retry, corrected per-connection
  citation. Amend's extra gix cost (manual ref transaction — commit_as
  expects ref == first parent) recorded for the gate decision.
