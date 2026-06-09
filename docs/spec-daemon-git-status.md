# Spec: Phase 3 — Git status

> Status: READY
> Created: 2026-06-09
> Completed: —

The daemon computes per-file git status (staged + unstaged) for the watched worktree plus repo-level branch state, recomputing on both worktree and `.git/` changes, and streams it to the client as incremental updates that decorate the worktree-snapshot entries — giving the client a live, accurate git status for every tracked or changed file. This re-introduces the per-entry status slot the file-tree spec deliberately reserved, and is the second consumer of the worktree foundation alongside the future explorer panel.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] After the initial worktree snapshot, the daemon computes git status for the worktree and the client's worktree model carries an accurate per-file status — distinguishing **staged (index)** from **unstaged (worktree)** changes — for every tracked / changed file (modified, added, deleted, renamed, untracked, conflicted).
- [ ] The client holds the repo-level state: current **branch name** and **ahead/behind** counts versus the upstream.
- [ ] A file modified, created, deleted, or staged on the remote produces an incremental git-status update the client applies — no full worktree rescan — and the per-file status converges to git's own view (`git status --porcelain` on the remote agrees, staged vs unstaged included).
- [ ] A change that lives **inside** `.git/` — a commit, a `git add`, a branch switch — is reflected: per-file statuses and the repo-level branch/ahead-behind update, even though the worktree watcher ignores `.git/`.
- [ ] Git-status computation runs off the daemon dispatch loop (on a blocking worker), and recompute is debounced/coalesced so an agent rewriting many files does not flood the channel or stall dispatch.
- [ ] Ignored paths carry no status (consistent with the worktree snapshot excluding them): a write to a `.gitignore`d path or inside `target/` produces no git-status update.
- [ ] The git state lives in the daemon's single `State` and is published to consumers via a `watch`/`broadcast` channel — no `Arc<Mutex<State>>`.

## Scope

### In scope

- **`crates/explorer/` git-status module**: given the worktree root, compute per-file git status with `gix` (honoring the same ignore rules as the scan), and expose it as a map from relative path to status. A daemon-side library — `gpui`-free, musl-clean, pure-Rust (no `libgit2`/C).
- **`.git/` change observation**: the worktree watcher excludes `.git/`, so git-status additionally observes a minimal whitelist of git control files (`HEAD`, `index`, `refs/`, `packed-refs`) to react to commits, staging, and branch switches — a **second watched set layered on the same `notify` backend the file-tree spec introduces**, not a separate watcher stack. Recompute is debounced/coalesced like the worktree updates.
- **Protocol**: re-introduce the per-entry git status the file-tree snapshot reserved — not the placeholder `Option<String>` the file-tree spec dropped, but a full-porcelain per-file status carrying an `index` (staged) and a `worktree` (unstaged) component, streamed as incremental git-status updates keyed by relative path, **plus** a repo-level state message (current branch + ahead/behind). A deliberate, additive `crates/protocol/` change.
- **Daemon wiring**: the daemon owns the git state in its single `State`, runs the `gix` status computation off the dispatch loop on a blocking worker (`spawn_blocking`), and routes git-status updates onto the client channel alongside the worktree updates.
- **Client-side**: the client applies git-status updates onto its in-memory worktree model, decorating entries. Verifiable headless via tests/logging — this is the consuming state, not yet a rendered panel (data-layer-only, inherited from the file-tree scope decision).
- **Single watched root** = the same worktree root the file-tree spec watches (the daemon's launch directory for v1).

### Out of scope

- **The rendered explorer panel and git-status badges** — its own sub-spec (the file-tree spec already cut the panel to data-layer-only). That panel consumes this client-side status model.
- **A git diff view / git panel** (staging UI, hunk-level operations, diffs, blame) — a much larger later feature (GitComet/Hunk-class). This spec is **status only**, never diff.
- **Git write operations** (stage, unstage, commit, checkout, etc.) — read-only status; the daemon observes, it does not mutate the repo.
- **Rewiring the tmux-sourced statusbar git branch (#18, Phase 2d)** — the daemon's repo-level branch state is the eventual Phase 3 successor to that tmux path, but this spec only produces and streams the data; it does **not** touch the existing statusbar wiring. The statusbar swap is a later step.
- **Submodule recursion and multi-repo / multi-root worktrees** — single root, top-level repo only for v1, mirroring the file-tree single-root cut.
- **LSP / diagnostics** — its own sub-spec.

## Constraints

- **Sequences after the file-tree milestone.** This spec can reach `READY` in parallel, but **implementation** sequences after the worktree file-tree sync lands (`spec-daemon-filetree.md`): there must be a worktree `Snapshot` model and the snapshot/update streaming to decorate before git status has anything to attach to. The status slot this spec fills is the one the file-tree snapshot reserved.
- **`gix` must cross-compile to static musl and the explorer must stay `gpui`-free.** `git2`/`libgit2-sys` is a C dependency and cannot statically link cleanly into the musl daemon build, which is why `gix` (pure-Rust) is mandated (see Prior decisions). Verify `gix` (and its status sub-crates) are musl-clean in the `daemon-musl` CI job, the same gate the scaffolding dep-trim (PR #99) established for daemon deps.
- **Snapshot/status is the source of truth**; the client never optimistically mutates git status — it only applies daemon updates. Mirrors the established worktree and tmux snapshot discipline (`spec-daemon-filetree.md`, `archive/spec-pane-window-management.md`).
- **Git status honors the same ignore rules as the scan.** Status is computed only over the entry set the snapshot exposes (tracked + non-ignored untracked); ignored paths never carry status.
- Adding to `crates/protocol/` is a deliberate API change — both sides depend on it, never on each other.
- `gix` is a new dependency that pulls a `gix-*` transitive tree. Adding it needs the dependency-rule sign-off per `CLAUDE.md`; `gix` itself is Apache-2.0/MIT, and the whole tree is gated by the `deny` CI job (`cargo deny check licenses`). It is the pure-Rust git implementation with no native-API equivalent — `git2` is ruled out by the musl constraint, and shelling out to the `git` binary is the rejected alternative (see Prior decisions).
- `thiserror` in the explorer library, `anyhow` in the daemon binary; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| Git backend is **`gix`** (pure-Rust), not `git2`/`libgit2` and not shelling out to the `git` binary | Precedent-decided: GitComet and Hunk both run their git **read/status** path on `gix` (`prior-art.md` Category 6); both also pull `git2`, but only as a narrow fallback for **write** operations, which this read-only spec excludes — so the read path precedent is cleanly `gix`. Constraint-reinforced: the daemon is a static musl binary, and `git2`/`libgit2-sys` is a C dependency that breaks the clean musl static link the `daemon-musl` gate requires. `gix` is pure-Rust + libc and stays musl-clean. | 2026-06-09 |
| Status streams as **per-entry decoration on the worktree snapshot** — incremental git-status updates keyed by relative path, not a separate full-repo status blob per change | Mirrors the file-tree `Snapshot` + incremental `UpdateWorktree` pattern (`spec-daemon-filetree.md`), which reserved exactly this status slot, and Zed's worktree which carries git status per entry (`prior-art.md` Category 5/6). | 2026-06-09 |
| Recompute runs **off the dispatch loop on `spawn_blocking`, debounced/coalesced** | `gix` status is blocking CPU/IO work; `CLAUDE.md` mandates "async for I/O, blocking for CPU" and a non-blocking dispatch loop. Mirrors the file-tree watcher's debounce discipline against event storms. | 2026-06-09 |
| Git-status must additionally **observe a minimal `.git/` whitelist** (`HEAD`, `index`, `refs/`, `packed-refs`) | The worktree watcher ignores `.git/` by design, but commits, staging, and branch switches mutate only `.git/`; without observing these, the client's statuses would silently go stale. The whitelist keeps the watched set bounded (not all of `.git/`, which churns heavily during gc/rebase). | 2026-06-09 |
| **Snapshot-as-source-of-truth**; no client-side optimistic status mutation | The established worktree/pane discipline (`CLAUDE.md` "state flows through channels") — the client re-derives from the next authoritative update, never mutates speculatively. | 2026-06-09 |
| **Read-only status**; the daemon never mutates the repo | Minimal scope; git write operations (stage/commit) are a later git-panel feature, not the explorer-decoration foundation this spec serves. | 2026-06-09 |
| **Data-layer-only**: this spec ends when the client holds an accurate, live per-file git status in its worktree model; the rendered panel and badges are a separate sub-spec | Inherited from the file-tree review-gate decision (`spec-daemon-filetree.md`): the data-layer cut is headless-verifiable, keeps the PR small, and fits the parallel-dev model. The panel sub-spec renders both the tree and this status. | 2026-06-09 |
| **Single watched root**, top-level repo only; submodules and multi-repo deferred | No premature abstraction; mirrors the file-tree single-root cut. | 2026-06-09 |
| Git-state granularity for v1 is **full porcelain**: a per-file status with an `index` (staged) and a `worktree` (unstaged) component, **plus** repo-level branch name + ahead/behind | Resolved at the review gate (`AskUserQuestion`). Neither precedent nor constraint settled it — Zed models the full porcelain `XY` + branch state, while `CLAUDE.md` "no premature abstraction" favored a minimal cut. Chosen to model git the way git models it (Zed precedent) and front-load what the later git-panel and the statusbar-branch successor need, accepting the larger protocol surface now. The repo-level branch is produced and streamed but not wired into the statusbar by this spec (see Out of scope). | 2026-06-09 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under a Phase 3 sub-milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (git-status sub-milestone under Phase 3)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo deny check licenses` passes with the full `gix` transitive tree resolved
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with `gix` linked
- [ ] Integration test against a fixture git repo: modifying a tracked file marks its `worktree` (unstaged) component modified; `git add`-ing it moves the change to the `index` (staged) component; creating an untracked file marks it untracked; deleting a tracked file marks it deleted; committing clears the status — each via the matching incremental update applied to the client model
- [ ] A branch switch / commit in the fixture (a `.git/`-only change) recomputes and updates both the per-file statuses and the repo-level branch name + ahead/behind
- [ ] A write to an ignored path (`target/foo`, a `.gitignore`d path) emits no git-status update
- [ ] A `grep` confirms no `Arc<Mutex<State>>` in the daemon crate and that `crates/explorer` pulls no `gpui`/`gpui-component` and no `git2`/`libgit2-sys` (inspect its resolved dependency tree — `gix` only)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `gix` status API surface / version maturity is unproven in this toolchain | Verify the `gix` status entry point and pin a known-good version in the first issue, the same way the file-tree spec verifies `notify`/`jwalk`; fall back to a narrower `gix` API if the high-level status helper is unstable. |
| `gix` musl cross-compile unproven here | The `daemon-musl` CI gate builds the daemon with `gix` linked; `gix` is pure-Rust + libc and expected clean. Confirm in the first issue. |
| Recompute cost when an agent rewrites many files at once | Debounce/coalesce within a short window before recomputing; run on a blocking worker so the dispatch loop stays responsive; the ignore-pruned entry set bounds the work. |
| `.git/` internal churn during rebase/gc, transient `index.lock` | Observe only the whitelisted control files; tolerate a transient lock by retrying the recompute on the next debounce tick rather than erroring; never panic on a mid-operation read. |
| Status recompute racing the worktree snapshot (status arrives for a path the client has not yet added) | Snapshot-as-source-of-truth ordering: the client tolerates a status for an unknown path by buffering or dropping it until the entry exists, since the next authoritative update reconciles. Define the exact reconciliation in the first protocol issue. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-09: Spec created from `/plan git-status`. `gix` recorded as precedent-decided (GitComet + Hunk read path) and constraint-reinforced (musl rules out `git2`/`libgit2`); per-entry decoration + incremental updates, `spawn_blocking`+debounce, the `.git/` whitelist, snapshot-as-truth, read-only, data-layer-only, and single-root recorded as precedent/constraint-decided. The one open decision — git-state granularity (minimal per-file vs. full porcelain + branch) — flagged for the review gate.
- 2026-06-09: Review gate (Agent review, VERDICT READY, no blocking findings). Resolved the open decision via `AskUserQuestion` — **full porcelain**: per-file `index`+`worktree` status pair plus repo-level branch + ahead/behind; the daemon branch is produced/streamed but the statusbar rewire (#18) stays out of scope. Addressed the non-blocking findings: clarified that the `gix`/`git2` reference projects pair `git2` only for writes (excluded here) so the read-path precedent is cleanly `gix`; added `cargo deny check licenses` to Verification for the `gix` transitive tree; noted the `.git/` whitelist is a second watched set on the file-tree `notify` backend, not a separate watcher. Flipped `DRAFT` → `READY` in the same PR (#129).
