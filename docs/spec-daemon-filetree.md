# Spec: Phase 3 — Worktree file-tree sync

> Status: READY
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
- **Single watched root** = the daemon's project root. The v1 default is the directory the daemon is launched in (per the scaffolding lifecycle); an explicit configured project path is a later extension, not part of this spec.

### Out of scope

- **Git-status decoration** of entries — its own sub-spec. The snapshot entry may reserve a status slot, but populating it is not this spec; the premature `git_status` field on the placeholder `FileEvent` is dropped here and re-introduced by the git-status spec on its own terms.
- **LSP / diagnostics** — its own sub-spec.
- **The GPUI file-explorer panel** that renders the tree and highlights touched files — its own sub-spec (the data-layer-only scope was chosen at the review gate, see Prior decisions). The panel spec consumes this client-side tree model and can render git-status decoration once the git-status spec lands.
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
| **Scope boundary is data-layer-only**: this spec ends when the client holds an accurate, live in-memory tree model; the rendered GPUI explorer panel is a separate sub-spec | Resolved at the review gate. The data-layer cut is headless-verifiable and keeps the PR small (`CLAUDE.md` "no large PRs"), fits the parallel-dev model (agents verify headless; the GPU station is a review gate), and gives a clean phase boundary: the panel sub-spec then renders this model and can fold in git-status decoration once that spec lands. The deferred cost is that this spec ships nothing the user can *see* — accepted. | 2026-06-09 |

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

- 2026-06-09: Spec created from `/plan file-tree`. File-sync strategy (Zed `Snapshot` + incremental `UpdateWorktree`, `notify` + `jwalk`), ignore-rule honoring, single-root scope, snapshot-as-truth, and move-as-remove+add recorded as precedent/constraint-decided. The placeholder `rift-protocol` `FileEvent`/`FileSync` messages are flagged for redesign, with `FileSync { content }` slated for removal as it contradicts the no-file-sync architecture.
- 2026-06-09: Review gate (Agent review, VERDICT READY, no blocking findings). Resolved the one open decision — **scope boundary is data-layer-only**; the GPUI explorer panel is deferred to its own sub-spec. Pinned the v1 watched root to the daemon's launch directory. Flipped `DRAFT` → `READY` in the same PR (#106).
- 2026-06-10 (#107): The worktree entry carries an **`mtime`** field beyond the issue's `(path, kind, ignored)` listing. The move-as-remove+add prior decision pins #109 on restat+snapshot-diff reconciliation rather than trusting `notify` event kinds; a diff over purely structural fields is blind to a content modification, so without `mtime` the strategy cannot satisfy the Verification line "modifying a file emits the matching incremental update" (a structural-only tree is identical before and after a content edit, making that test vacuous). Zed's `proto/worktree.proto` entry — named in `prior-art.md` as "exactly the protocol rift needs" — carries `mtime` for the same reason. Modeled as `std::time::SystemTime` (serde-clean, the natural type from `Metadata::modified()`); no `size` field for v1 (`mtime` alone is a sufficient change detector — minimal scope). The `changed` variant carries the full entry, not just the path, so the client upserts blindly. The same `mtime` field lands on the explorer `Snapshot` entry in #108.
- 2026-06-11 (#108): Resolved the prior-decision table's open question — **`ignore`'s `WalkBuilder` subsumes `jwalk`**. `ignore` performs gitignore-aware recursive traversal itself (gitignore matching is required regardless), so the scan adds one new dependency (`ignore`) instead of two; `jwalk` is dropped and `notify` deferred to the watcher step (#109). The v1 scan uses the sequential walker for clean per-entry error handling (skip-and-log on unreadable/unstattable entries, never fatal); `WalkBuilder::build_parallel()` stays a drop-in optimization without a new dependency if the initial scan ever becomes a bottleneck. The walker runs with `hidden(false)` (unignored dotfiles like `.gitignore`/`.github` are kept), `require_git(false)` (honor `.gitignore` even outside a checked-out repo), and `git_global(false)`/`parents(false)` (self-contained, host-independent); `.git/` is excluded with a hard `filter_entry`. Symlinks are recorded as non-followed `File` leaves, so loops cannot arise. The explorer `Snapshot` entry mirrors the protocol entry's fields; its `ignored` flag is uniformly `false` in v1 because ignored paths are excluded from the scan — the flag reserves room to surface greyed-out ignored entries later (whether ignored entries ever go on the wire is left to #109/#110). `crates/explorer` is `gpui`-free and builds clean for `x86_64-unknown-linux-musl`.
- 2026-06-11 (#109): The `notify` watcher reconciles by **rescan-on-flush + snapshot diff**, not targeted per-event restat — `notify` events are only a *trigger*. `Snapshot::diff` is a pure `BTreeMap` comparison (`Added`/`Changed`/`Removed`) and `Snapshot::apply` its exact inverse (`apply(diff(a,b)) == b`, pinned as a test invariant); a move falls out as remove+add and an ignored write yields no delta for free (the path is absent from both ignore-pruned scans), so no per-path gitignore matching is needed in the watcher. Reusing `Snapshot::scan` keeps the ignore logic in one place. **Watch-set pruning**: only non-ignored directories are watched (root + every `Dir` entry, non-recursively, reconciled against the snapshot each flush). Recursive-watch-on-root is less code but would register a watch per file under `target/` — hundreds of thousands on a real Rust checkout — and exhaust `max_user_watches`; pruning keeps the watched set bounded to tracked dirs, which is the spec's primary inotify mitigation. A per-directory watch failure (the watch limit) is logged **once** via a `warned` flag and degraded, never fatal; a failure to create the `notify` backend at all is the only error `Watcher::new` returns. Debounce/coalesce is **hand-rolled** with `recv_timeout` (100 ms quiet window, 1 s max-coalesce cap, 500 ms idle poll so the worker notices shutdown) — no `notify-debouncer-*` crate. `crates/explorer` stays **tokio-free**: the watcher runs on a `std::thread` with `std::sync::mpsc`, leaving the tokio bridge to the daemon (#110). `notify = "7"` reuses the version already in the lock (transitive via `gpui-component`), so it adds **zero new crates** and no new license surface; musl-clean confirmed locally (`inotify`/`inotify-sys`/`mio`/`filetime`, pure Rust + libc). The watcher's `Change` and the snapshot diff use owned `PathBuf`/`Entry` (the deltas are emitted across a channel, outliving the snapshot borrow) — ownership at the boundary, not a borrow-checker clone.
- 2026-06-12 (#110): The worker **arms the watcher before delivering the snapshot** to the dispatch loop. `Watcher::new` registers its watch set synchronously, so once a consumer has observed the snapshot, any later write is guaranteed to produce an event; the reverse order races — a write right after the snapshot lands precedes the watches, and with no event the rescan-on-event watcher never surfaces it (caught by the daemon integration tests). Scan + watcher + the blocking relay of change batches all live on one `spawn_blocking` worker feeding the dispatch loop through an internal `mpsc`; the loop `select!`s it alongside `ClientMessage`s, keeping the single-`State`-plus-channels shape (no `Arc<Mutex<State>>`). A scan/watch failure degrades to "no worktree" and the daemon keeps serving.
- 2026-06-12 (#110): A `Hello` **re-broadcasts the current snapshot on the shared event bus** so a (re)attaching client starts from the full tree — the scaffolding transport has no per-client send path. Already-attached clients receive the repeat too; a full snapshot is an idempotent replace, so this is redundant but never inconsistent. The client model (#111) must treat a `WorktreeSnapshot` chunk arriving after a `final_chunk` as the start of a new accumulation, not a continuation. Chunking is 1024 entries per `WorktreeSnapshot` frame, `final_chunk` only on the last; an empty tree still yields one final message. Because the dispatch loop is the only emitter, a snapshot's chunks are never interleaved with updates.
- 2026-06-12 (#110): `serve`/`serve_uds` take the watched root as an explicit `Option<PathBuf>` parameter; the binary passes its launch directory (cwd) in both `--serve-uds` and stdio mode, and tests pass `None`/fixture roots. Test-helper fix recorded for future protocol tests: a `FrameDecoder` must be **caller-owned per connection** — one read may deliver several back-to-back frames, and a per-call decoder silently discards the buffered tail along with itself (this hung every test reading more than one message off a connection).
