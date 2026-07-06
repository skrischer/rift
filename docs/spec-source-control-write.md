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
      exactly those lines (index blob rewritten from the base blob + selected
      hunks), leaving the rest unstaged.
- [ ] All write operations are explicit user actions over the protocol —
      the daemon never writes git state on its own.

## Scope

### In scope

- `protocol` (deliberate API change, version-bumped): request/response pairs
  `StageFile { path }`, `UnstageFile { path }`, `StageHunk { path, hunk_id }`,
  `Commit { message }` → one `GitOpResult { op, ok, error }` reply each, plus
  `DiscardFile { path }` if the gate accepts discard. Replies are
  per-connection (the #482 routing discipline); the resulting state change
  arrives through the existing push-only git recompute (no echo of status in
  the reply).
- `daemon`/`explorer` (gix, already the git engine): stage = write the
  worktree blob into the index at `path` (add for untracked); unstage =
  restore the index entry from HEAD (remove for newly added); commit = tree
  from index + commit with parents=[HEAD], author/committer from config,
  reject empty message or empty index; hunk staging = construct the new index
  blob from the CURRENT INDEX blob + the selected hunk's lines (hunks come
  from rift's own diff engine, so application is deterministic; `hunk_id` =
  the stable hunk header from the last pushed `FileDiff` — a stale id is
  rejected, never fuzzily applied). A git op triggers an immediate recompute
  tick so the UI converges fast.
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
  worktree-vs-HEAD diff per file; hunk staging operates on index-blob
  application as specced. Revisit when dogfooding demands split views.
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
  §0; mono for all code/numbers.
- Constitution: no `.unwrap()` in libs; protocol documented + tested valid/
  malformed; crate boundaries (gix usage stays in explorer/daemon).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Staged/unstaged sections read the EXISTING `GitStatusEntry.index`/`.worktree` codes | The data model has carried the split since the git-status phase — only the UI grouped by change type; no protocol change needed for the read side | 2026-07-06 |
| Hunk staging = deterministic application of rift's own hunks onto the index blob, keyed by stable hunk ids; stale ids rejected | rift computed the hunks it displays — applying exactly them is deterministic; fuzzy patch application is where hunk staging goes wrong (prior art: Zed git_ui stages its own computed hunks the same way) | 2026-07-06 |
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
      tests on real fixture repos: stage/unstage tracked+untracked+deleted,
      commit (empty-message and empty-index rejected), hunk staging on a
      multi-hunk file (exact lines staged; stale hunk id rejected)
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
