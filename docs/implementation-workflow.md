# Implementation workflow

How a single issue goes from `READY` spec to merged code, as practiced across the
Phase 2d statusbar work (#17, #19, #20, #21). This is the mechanical companion to
`handover-conventions.md` (which owns the design-doc -> issue -> PR chain) and the
"Parallel development (worktrees)" section in `AGENTS.md`.

## Steps

1. **Orient.** `gh issue view <N> --json title,body,labels`, read the relevant code,
   read the matching `READY` `docs/spec-*.md` (never a `DRAFT`).
2. **Plan-first.** Write a plan and get approval before implementing. For genuine
   design forks (e.g. #21: no reconnect logic exists, so what does "reconnecting"
   mean?) ask the user with a decision prompt instead of picking silently.
3. **Worktree.** `git checkout develop && git pull`, then
   `just agent-worktree feat/<scope>`. Worktrees live in `../rift-worktrees/` with
   their own small `target/` (no GPU build).
4. **Implement headless.** Edits only in the worktree. Mirror existing patterns —
   a new end-to-end signal currently touches six sites in the same shape:
   `lib.rs` (type) -> `TerminalHandle` field -> `SessionView` field ->
   `cx.spawn` consumer loop -> `PtyChannels` -> `main.rs` wiring.
5. **Verify headless.** `just lint && just test` (both `--exclude rift-app`).
6. **Commit.** Conventional Commit, scope = crate, `Closes #N` in the body.
7. **Visual-review gate (before merge).** On the GPU station:
   `git checkout --detach <branch>` + `just dev-watch`, verify visually, then
   `git checkout develop`. This is the *first* time `rift-app`/`main.rs` is
   compiled — CI and `just lint` both exclude `rift-app`.
8. **PR.** Push from the worktree (`git -C <worktree> push`, works around the
   push-guard hook), then `gh pr create --base develop`.
9. **Merge at green.** Remove the worktree *first*, then squash-merge, then sync
   develop and confirm the issue closed.
10. **Decision log.** Separate `docs:` branch/PR adding the entry to the spec.

## Recurring friction (observed)

- **`gh pr merge` trips over local branch state.** Happened four times. Causes seen:
  the branch still checked out in its worktree; the main checkout left detached on
  another branch from a visual review. The merge succeeds on the remote but the
  local post-merge cleanup leaves a junk merge commit / diverged develop.
- **Stale-base conflicts.** A parallel branch (#55 font-zoom) merged into develop
  while #21 was open, producing a `session_view.rs` conflict that had to be
  resolved by merging `origin/develop` into the feature branch before merge.
- **`rift-app` is never compiled in CI or `just lint`.** `main.rs` breakage only
  surfaces at the manual visual-review step — the largest correctness gap.
- **CI polling is awkward.** `gh pr checks` exits 0 even when no checks are
  reported yet, which breaks naive wait loops.
- **Visual review serializes on the single GPU station.**

## Proposed optimizations (priority order)

1. **`just agent-merge <branch>` recipe.** Run the merge purely against the remote
   (`gh pr merge <N> -R <repo> --squash --delete-branch`) so it never touches the
   local checkout, and remove the worktree as part of the recipe. Eliminates the
   most frequent failure class outright.
2. **`cargo check -p rift-app` in CI** (cached `target/`). Catches `main.rs`
   compile errors before merge instead of at visual review. Trade-off: CI time and
   runner disk for the skia/wgpu build, amortized by caching.
3. **Refresh base before PR** as a fixed step: `git merge origin/develop` in the
   worktree (no force-push needed; squash collapses it). Optionally create the
   worktree off `origin/develop` after a fetch.
4. **Reduce per-feature boilerplate.** The six-site channel pattern now has 6+
   identical instances — past the "extract at 2+ implementations" threshold. A
   bundled channel container would cut the edit surface to one or two sites. Own
   `refactor:` issue, not smuggled into a feature.
5. **`just pr-wait <N>` helper** (or `gh pr checks --watch`) for CI waiting.
6. **Batch visual reviews** when several headless-verified branches are queued, to
   amortize GPU-station context switches.

## What works and should stay

Plan-first plus an explicit decision prompt at forks; the hard visual-review gate
before merge; squash merges; separate `docs:` PRs for the decision log; one
worktree per issue (clean isolation, small `target/`).
