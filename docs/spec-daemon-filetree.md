# Spec: Phase 3 — Worktree file-tree sync

> Status: DRAFT
> Created: 2026-06-09
> Completed: —

The daemon scans and watches a project root, maintains a Zed-style worktree `Snapshot`, and streams it to the client as an initial snapshot plus incremental updates over the `rift-protocol` channel — giving the client a live, accurate model of the remote file tree. This is the first real payload over the scaffolding transport and the data foundation that git-status and LSP both build on.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] On connect, the daemon scans the project root (honoring VCS ignore rules) and sends the client a complete initial worktree snapshot; the client holds an in-memory tree mirroring the remote file structure.
- [ ] A file created, modified, deleted, or moved on the remote produces an incremental worktree update that the client applies to stay consistent — no full rescan per change.
- [ ] Ignored paths are excluded from both the initial scan and incremental updates: a write inside `.git/`, `target/`, or a `.gitignore`d directory produces no update; a write to a tracked file does.
- [ ] The scan and watch never block the daemon dispatch loop (they run off the loop on a blocking worker), and file-event bursts are coalesced/debounced rather than emitted one message per syscall.
- [ ] The worktree state lives in the daemon's single `State` and is published to consumers via a `watch`/`broadcast` channel — no `Arc<Mutex<State>>`.

## Scope

### In scope

- **`crates/explorer/` worktree library**: a Zed-style `Snapshot` model (entries keyed by relative path, each carrying file kind and ignored status), a background directory scan, and a `notify`-backed recursive watcher with debouncing/coalescing. A daemon-side library — `gpui`-free, musl-clean.
- **Protocol redesign**: replace the placeholder `rift-protocol` file messages (`FileEvent`, `FileSync`) with a proper worktree protocol — an initial `WorktreeSnapshot` message (chunked if large) plus incremental `UpdateWorktree` messages (added / changed / removed entries). A deliberate, additive `crates/protocol/` API change.
- **Daemon wiring**: the daemon owns the explorer worktree in its `State`, runs the scan/watch off the dispatch loop, and routes snapshot + updates onto the client channel.
- **Client-side worktree model**: the client receives the initial snapshot and applies incremental updates, maintaining an accurate in-memory tree. Verifiable headless via tests/logging — this is the consuming state, not yet a rendered panel (see the open scope decision).
- **Single watched root** = the daemon's project root (the directory it is launched in, or a configured project path).

### Out of scope

- **Git-status decoration** of entries — its own sub-spec. The snapshot entry may reserve a status slot, but populating it is not this spec; the premature `git_status` field on the placeholder `FileEvent` is dropped here and re-introduced by the git-status spec on its own terms.
- **LSP / diagnostics** — its own sub-spec.
- **The GPUI file-explorer panel** that renders the tree and highlights touched files — deferred to its own sub-spec **if** the data-layer-only scope is chosen (see Prior decisions, the OPEN row).
- **Multi-root / per-pane-CWD worktree contexts** (`vision.md` Scenario 2) — single root for v1; multi-root is a later phase.
- **Fuzzy file search** (`nucleo`) — a consumer of the tree, not part of the sync foundation.
- **File-content sync** — rift edits happen in tmux/Neovim on the remote; the explorer never needs file *contents* locally. The placeholder `FileSync { content }` message is removed, not redesigned — it contradicts the no-file-sync architecture (`architecture.md` "Why LSP runs on the remote": diagnostics flow as lightweight JSON, never file contents).

## Constraints

- Builds on the daemon scaffolding transport seam (`spec-daemon-scaffolding.md`). The spec can reach `READY` in parallel, but file-tree **implementation** sequences after the scaffolding milestone issues (#58, #60, #61, #62) land — the round-trip must exist before there is a channel to stream a snapshot over.
- `crates/explorer/` must cross-compile to static musl and stay `gpui`-free — it becomes a daemon dependency, and the scaffolding dep-trim (PR #99) established that a daemon dep must be `gpui`-free and musl-clean before it is re-added to `crates/daemon/Cargo.toml`. Verify `notify` / `jwalk` / `ignore` are musl-clean (pure-Rust + libc; expected clean) in the `daemon-musl` CI job.
- Snapshot is the source of truth; the client never optimistically mutates its tree — it only applies daemon updates. This mirrors the established tmux snapshot discipline for panes/windows (`archive/spec-pane-window-management.md`).
- Adding to `crates/protocol/` is a deliberate API change — both sides depend on it, never on each other.
- `notify`, `jwalk`, and `ignore` are new dependencies (see Prior decisions for justification). Adding them needs the dependency-rule sign-off per `CLAUDE.md`; all three are MIT/Apache and have no native-API equivalent for recursive watching, parallel traversal, and gitignore matching respectively.
- `thiserror` in the explorer library, `anyhow` in the daemon binary; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| File-sync strategy is the **Zed `crates/worktree` `Snapshot` model + incremental `UpdateWorktree` messages** (serve tree + deltas, never full re-sync) | Pre-recorded in the scaffolding spec's "Recorded for later sub-specs". Validated by Zed `crates/worktree` + `proto/worktree.proto` — `prior-art.md` #1 calls it "exactly the daemon→client protocol rift needs" — and by nexus-explorer (GPUI + jwalk + notify). | 2026-06-09 |
| File watching via **`notify`**; parallel directory traversal via **`jwalk`** | Both named in the scaffolding spec's recorded decision and the `prior-art.md` candidate-dependency table; used together by nexus-explorer (`prior-art.md` Category 1/5). MIT / MIT-Apache. | 2026-06-09 |
| **Honor VCS ignore rules** (`.git/`, `.gitignore`) by default | Zed's worktree does this; it is essential, not cosmetic — `target/` and `node_modules/` are gigabytes (`architecture.md` "Why LSP runs on the remote"), and an un-pruned scan/watch would swamp the initial snapshot and exhaust inotify watches over SSH. The first issue confirms whether the `ignore` crate's parallel `WalkBuilder` can subsume `jwalk` (one dependency instead of two) since gitignore matching is required regardless; otherwise `jwalk` + `ignore::gitignore`. | 2026-06-09 |
| **Snapshot-as-source-of-truth**; no client-side optimistic tree mutation | The established pane/window discipline (`CLAUDE.md` "state flows through channels"; pane-window-management spec) — the UI emits an intent and re-derives from the next authoritative update, never mutates local state speculatively. | 2026-06-09 |
| **Single watched root** (the daemon's project root); multi-root deferred | Minimal scope, no premature abstraction. Per-worktree explorer contexts (`vision.md` Scenario 2) are a later phase and would force a multi-root abstraction this spec does not need. | 2026-06-09 |
| Model a move as **remove + add** at the snapshot level | `notify` rename events are unreliable and backend-specific across platforms; Zed reconciles renames through the snapshot diff rather than trusting a rename event. The protocol carries add/change/remove; a dedicated rename variant is not required. | 2026-06-09 |
| **OPEN — scope boundary: data-layer-only vs. include the GPUI explorer panel.** Resolved at the review gate. | Neither precedent nor a codebase constraint settles whether this spec ends at "the client holds an accurate live tree model" (panel = its own sub-spec) or also ships the rendered GPUI file-explorer panel that highlights touched files. It is a product-visibility vs. PR-size judgment: the data-layer-only cut is headless-verifiable and keeps the PR small (per `CLAUDE.md` "no large PRs"), but ships nothing the user can *see*; the full slice delivers the first visible north-star moment but needs GPU-station review and is larger. | — |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under a Phase 3 sub-milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (file-tree sub-milestone under Phase 3)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with the explorer dependencies linked
- [ ] Integration test: scanning a fixture tree yields a snapshot matching the on-disk structure; creating / modifying / deleting a file emits the matching incremental update, and applying it to the client model reproduces the new tree
- [ ] A write inside an ignored directory (`target/foo`, a `.gitignore`d path, `.git/`) emits no update; a write to a tracked file does
- [ ] A `grep` confirms no `Arc<Mutex<State>>` in the daemon crate and that `crates/explorer` pulls no `gpui`/`gpui-component` (inspect its resolved dependency tree)
- [ ] (only if the GPUI-panel scope is chosen) GPU-station check: the explorer panel renders the tree and visibly highlights a file as it is modified

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| First scan of a large repo over SSH is slow or floods the channel | Ignore rules exclude the heavy directories (the dominant cost); chunk the initial `WorktreeSnapshot`; run the scan on a blocking worker so the dispatch loop stays responsive. |
| `notify` exhausts the kernel inotify watch limit (`max_user_watches`) on a large tree | Ignore-pruning keeps the watched set bounded to tracked files; if the limit is still hit, log once and degrade (e.g. coarser watching) rather than panicking. |
| File-event storms (an agent rewriting many files at once) | Debounce/coalesce within a short window before emitting `UpdateWorktree`, collapsing repeated events on the same path. |
| musl cross-compile of `notify` / `jwalk` / `ignore` is unproven in this toolchain | The `daemon-musl` CI gate (from the scaffolding spec) builds the daemon with the explorer deps linked; confirm in the first issue. All three are pure-Rust + libc and expected clean. |
| Symlink loops or permission-denied directories during the scan | Skip and log per entry; never abort the whole scan on one unreadable path. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-09: Spec created from `/plan file-tree`. File-sync strategy (Zed `Snapshot` + incremental `UpdateWorktree`, `notify` + `jwalk`), ignore-rule honoring, single-root scope, snapshot-as-truth, and move-as-remove+add recorded as precedent/constraint-decided. The placeholder `rift-protocol` `FileEvent`/`FileSync` messages are flagged for redesign, with `FileSync { content }` slated for removal as it contradicts the no-file-sync architecture. Scope boundary (data-layer-only vs. include the GPUI explorer panel) left OPEN for the review gate.
