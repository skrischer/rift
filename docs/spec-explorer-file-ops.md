# Spec: Explorer file operations (daemon write path)

> Status: READY
> Created: 2026-07-08
> Completed: —

Give the explorer create / rename / delete / move by adding a daemon-side file
operation write path — new `protocol` messages the daemon executes with
`std::fs` on the remote host it already owns (never client-side SFTP) — and
surface it through inline rename (artboard **State C**), the write-actions group
of the Phase-29 context menu (artboard **State D**), and drag & drop (move).

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. This is the **file-operations** phase of the explorer overhaul: it
gives the tree its first mutation capability by landing a daemon write path
(mirroring the buffer-save and Phase-24 git-write precedents) and the three
client surfaces the "Explorer — Redesign" artboard reserves for it (State C
inline rename; the write group of State D; drag & drop). It does **not** touch
icons (Phase 28), the context-menu framework itself (Phase 29 ships the shell;
this phase adds only its write items), or search / filter (Phase 31).

- [ ] The `protocol` message set gains a **file-operation channel** — four
      request messages (`CreateFile` / `CreateDir` / `RenamePath` /
      `DeletePath`) and one reply (`FileOpResult`, carrying a typed `FileOp`
      echo and an optional typed `FileOpError`) — as a **deliberate, reviewed
      API extension**: `PROTOCOL_VERSION` bumps `8 → 9`, the fingerprint is
      re-pinned, and `docs/protocol.md` documents the channel. The message
      contract is fully specified below; the implementer only wires it.
- [ ] The **daemon owns the write** (the roadmap's phase-30 foundation impact,
      ratified here): every file op runs **daemon-side** via `std::fs` /
      `tokio::fs` on the remote host, confined to the watched worktree root with
      the same resolver a buffer **write** uses (`buffer::resolve`) — **never**
      client-side SFTP. This mirrors Zed's remote server (prior-art index Phase
      30). No new dependency.
- [ ] **Conflict / overwrite semantics are safe by construction**: create
      (file or dir) and rename **refuse** when the target already exists
      (`AlreadyExists`) rather than clobber; rename refuses a missing source
      (`NotFound`); a path escaping the root is refused (`InvalidPath`). No file
      op ever overwrites existing content.
- [ ] A **self-inflicted op does not double-apply or flicker**: the client's
      `WorktreeModel` tree is mutated by the **push-only** `UpdateWorktree`
      snapshot-diff **alone** (the single writer). A create / rename / delete
      triggers the same daemon watcher rescan an agent's edit would, so the tree
      change arrives through the identical delta path (create → `added`, rename
      → `removed` + `added`, delete → `removed`); the `FileOpResult` reply drives
      only UX transitions (close the rename editor, dismiss the confirm dialog,
      surface an error), never the tree mutation. This is the git-write
      channel's "state converges via the push recompute" contract, applied to
      tree structure.
- [ ] **Inline rename** (artboard **State C**): `F2` or the context-menu
      *Rename* item replaces the selected row's name with a seeded `gpui-component`
      text input (name selected, extension not); `Enter` commits a `RenamePath`,
      `Escape` cancels; an `AlreadyExists` / `InvalidPath` reply re-opens the
      editor with the error, never losing the typed name.
- [ ] The **write group of the Phase-29 context menu** (artboard **State D**):
      *New File…*, *New Folder…*, *Rename*, *Delete* dock into the Phase-29
      menu. New File / New Folder use the same inline text-input affordance
      (a transient row under the target directory) to name the entry, then send
      `CreateFile` / `CreateDir`. *Delete* is gated behind the destructive
      confirm dialog (the `DiscardFile` `#420` pattern), never batched, then
      sends `DeletePath`.
- [ ] **Drag & drop move**: dragging a row onto a directory sends a `RenamePath`
      that moves it there; a no-op move (same parent) and a directory dropped
      into its own subtree are refused **client-side** before the send, and the
      daemon guards the same cases. A drag preview and a drop-target highlight
      make the target legible.
- [ ] The explorer stays **agent-agnostic**: file ops derive only from user
      intent over the filesystem; nothing detects or parses an agent. Git
      decoration on a renamed/created/deleted path converges through the
      existing push-only `UpdateGitStatus` / `RepoState` recompute — git's own
      **native rename detection** makes a move git-aware with no divergent index
      write (see Prior decisions).
- [ ] `cargo clippy --workspace -- -D warnings` and `cargo test --workspace`
      pass; CI `app-check` compiles the app.

## Scope

### In scope

The **file-operation protocol channel**, its **daemon-side handlers**, and the
**three client surfaces** the artboard reserves. The binding visual reference is
the Paper **"Explorer — Redesign"** artboard (file `rift`) — **State C** (inline
rename) and the write-actions group of **State D** (context menu). Phase 30
realizes those two contracts; Phase 27 authored the artboard, Phase 29 ships the
context-menu shell they dock into.

**Protocol message contract (fully specified — the implementer only wires it).**
Add to `ClientMessage` (all paths worktree-root-relative, the same key space as
[`WorktreeEntry::path`]):

- `CreateFile { path: String }` — create an empty regular file at `path`.
  Missing parent directories are created (as a buffer **write** does). Refused
  `AlreadyExists` if `path` already exists — never overwrites. One
  `FileOpResult` reply.
- `CreateDir { path: String }` — create a directory (and missing intermediates)
  at `path`. Refused `AlreadyExists` if a file or directory already occupies it.
  One `FileOpResult` reply.
- `RenamePath { from: String, to: String }` — rename/move `from` to `to`. **One
  message covers both** inline rename (same parent, new name) and drag-drop move
  (new parent) — they are the same `fs::rename`. Missing parent directories of
  `to` are created. Refused `AlreadyExists` if `to` exists (no clobber);
  `NotFound` if `from` is gone. One `FileOpResult` reply.
- `DeletePath { path: String }` — delete `path`: a file via `remove_file`, a
  directory **recursively** via `remove_dir_all`. **Destructive**: the client
  gates it behind a confirm dialog and never batches it (the `DiscardFile`
  `#420` pattern). One `FileOpResult` reply.

Add to `DaemonMessage`:

- `FileOpResult { op: FileOp, ok: bool, error: Option<FileOpError> }` — the
  reply to **every** file-op request: whether `op` succeeded, or a typed `error`
  when it did not (`error` omitted on success via
  `skip_serializing_if = "Option::is_none"`). This is the **only** signal the
  file-op path returns; the resulting tree change is **never** echoed here — it
  arrives through the push-only `UpdateWorktree` recompute, the protocol's one
  source of truth for tree structure (mirrors `GitOpResult`).

Add the two supporting types:

- `FileOp` — a `#[serde(tag = "kind", rename_all = "snake_case")]` enum echoing
  the request so the client can correlate the reply (mirrors `GitWriteOp`):
  `CreateFile { path }`, `CreateDir { path }`, `Rename { from, to }`,
  `Delete { path }`.
- `FileOpError` — a `#[serde(rename_all = "snake_case")]` typed reason (mirrors
  `BufferErrorReason`): `AlreadyExists`, `NotFound`, `PermissionDenied`,
  `InvalidPath` (path escaped the root or was empty), `Io` (generic fallback).

Bump `PROTOCOL_VERSION` `8 → 9`, re-pin `PROTOCOL_FINGERPRINT` (the failing
fingerprint test prints the new value), add serde round-trip tests for every new
variant (valid + unknown-tag rejection, as the existing tests do), and extend
`docs/protocol.md` with a "File-operation channel" section and a version-9
history line.

**Daemon handlers (`std::fs`, confined, daemon-side).** A new
`crates/daemon/src/file_ops.rs` module — the shape of `git_write.rs`: an
`async fn reply(state, msg) -> DaemonMessage` that clones the canonical root out
of `State` (releasing the borrow before any `await`, as `git_write::reply` /
`request_reply` do), confines every path with `buffer::resolve(&root, …)`
(both `from` and `to` for a rename), runs the fs mutation on
`tokio::task::spawn_blocking` (disk-bound), and maps the outcome to a
`FileOpResult`. Wire the four new variants into `serve_connection`'s
per-connection reply dispatch beside the git-write arm (`file_ops::reply(&state,
msg).await` → `encode_frame` → write back to the requesting socket). Map
`std::io::ErrorKind` to `FileOpError` the way `buffer_error_reason` maps buffer
errors (`AlreadyExists` / `NotFound` / `PermissionDenied` → the matching
variant; `resolve`'s `PathEscape` → `InvalidPath`; else `Io`), plus an explicit
up-front existence check for the create/rename `AlreadyExists` cases (so the
refusal is deterministic, not `ErrorKind`-dependent).

**Inline rename — artboard State C (`file_tree.rs`).** A per-tree rename-editor
state: the row being renamed carries a `gpui-component` `InputState` seeded with
the current name (name portion selected, extension not — the standard IDE
affordance); `render_row` swaps the name label for the input while active.
`Enter` commits `RenamePath { from, to: <parent>/<new> }` and closes the editor;
`Escape` cancels. On an error reply the editor re-opens with the typed name and
an inline message. Reuse the existing `InputState` widget (already used by the
commit textarea); never fork it.

**Context-menu write group — artboard State D (`file_tree.rs`).** Extend the
Phase-29 context menu with *New File…*, *New Folder…*, *Rename*, *Delete*. New
File / New Folder insert a transient inline input row under the target directory
(reusing the State-C inline-editor mechanism) and, on commit, send `CreateFile`
/ `CreateDir`. *Rename* triggers the State-C editor. *Delete* opens the
destructive confirm dialog (the `#420` pattern used by `SourceControlPanel::confirm_discard`
and the editor's dirty-close dialog), then sends `DeletePath`; never batched.

**Drag & drop move (`file_tree.rs`).** Make a row a drag source and a directory
row a drop target (gpui `on_drag` / `on_drop` / drag-move affordances). A drop
resolves the target directory (the dropped-on dir, or a dropped-on file's
parent) and sends `RenamePath { from: <dragged>, to: <dir>/<basename> }`. Refuse
**client-side** before the send: a no-op move (target parent equals the current
parent) and a directory dropped into itself or a descendant. Render a drag
preview and highlight the drop target.

**Client plumbing (`main.rs` / `workspace.rs`).** A `file_op_tx`
`Sender<ClientMessage>` bridged onto the protocol exactly as the git-write
`git_op_tx` is, and `FileOpResult` routed back to the `FileTree` for the UX
transitions above (close editor / dismiss dialog / surface error). The tree
never mutates its `WorktreeModel` from a file op — the push `UpdateWorktree` is
the single writer. A **pending-reveal** affordance: on a successful create /
rename the client records the new path and selects + reveals its row when the
next `UpdateWorktree` adds it (a small client-side follow, not a protocol
concern).

### Out of scope — each its own phase or deliberately deferred

- **File-type icons + SVG asset embedding — Phase 28.** Unchanged here; the
  reserved icon slot stays as Phase 27 left it.
- **The context-menu framework itself — Phase 29.** Phase 29 ships the
  right-click shell and its client-capable actions (open, reveal, copy path,
  reveal-in-terminal, collapse-all) with **no** write items. Phase 30 adds only
  the *New File / New Folder / Rename / Delete* group into that existing menu;
  it does **not** build the menu. Phase 30 implementation assumes Phase 29 has
  merged (the roadmap sequences 29 before 30).
- **Search / filter / quick-open + multi-select — Phase 31.** File ops act on
  the single selected row / drag source; multi-select delete/move is Phase 31's
  once multi-select exists.
- **`git mv`-style index staging of a move (gix index rename).** A move is a
  plain `fs::rename`; git's **native rename detection** surfaces it through the
  existing push-only status recompute (that is what "git-aware" means here). An
  explicit gix index rename — staging the removal of the old path and the
  addition of the new so `git status` shows a rename *before* the user stages —
  is **deliberately deferred**: it duplicates the already-shipped stage-file
  capability (the user stages the move via the source-control panel) and adds
  index-mutation risk to the fs write path. See Prior decisions. Consequently
  `crates/explorer` is **untouched** (no new gix code); the daemon handlers are
  plain `std::fs`.
- **Overwrite-on-move / overwrite-on-create.** v1 refuses every clobber
  (`AlreadyExists`). An explicit "replace existing?" confirm flow is a future
  refinement, not this phase.
- **Trash / undo.** No remote trash exists; delete is a confirmed permanent
  `remove_*`. Undo is out of scope (git already recovers tracked content).
- **Symlink creation, chmod, copy/duplicate.** Not in the artboard's write
  group; out of scope.

## Human prerequisites

None. The daemon already runs on the remote host with write access (it saves
buffers and mutates the git index); file ops are the same `std::fs` capability
on paths it already confines. No new dependency (plain `std::fs` / `tokio::fs`;
`gpui`'s drag primitives are already available; `gpui-component`'s `InputState`
and confirm-dialog are already vendored and in use). No secrets, no
provisioning. The `PROTOCOL_VERSION` bump is client+daemon lockstep, already the
project's deploy discipline (the daemon binary is redeployed per session).

## Constraints

- **Daemon owns the filesystem — ops run daemon-side, never client SFTP**
  (roadmap phase-30 foundation impact; architecture: the daemon watches, reads,
  and writes the remote tree). The client sends intent; the daemon executes with
  `std::fs`. This is the same seam as buffer save and git-write — no SFTP layer,
  no second transport. Mirrors Zed's remote server (prior-art index Phase 30).
- **`protocol` is a deliberate API surface** (constitution). The four requests,
  the reply, and the two supporting types are an intentional, reviewed
  extension; `PROTOCOL_VERSION` bumps `8 → 9` and the fingerprint test re-pins,
  so the message-set change cannot merge without the bump (CI-enforced). Adding
  to `protocol` is reviewed here, not improvised in implementation.
- **Root confinement, reusing the buffer write resolver.** Every path is
  confined with `buffer::resolve` (textual `..`/absolute/prefix guard + a
  canonicalize-the-existing-prefix symlink guard), exactly as a buffer **write**
  and a git-write op are. A rename confines **both** endpoints. There is **no**
  out-of-root carve-out — writes never leave the root (the read-only carve-out is
  a read-path concept and does not apply).
- **Single writer to the client tree — the push-only `UpdateWorktree`.** The
  file op must **not** optimistically mutate the `WorktreeModel`. The daemon's
  watcher rescans on the same fs event the op causes and emits the delta; the
  client applies that one delta. This is why a self-inflicted op cannot
  double-apply (the client never applied it) and cannot flicker (one writer).
  The ~100 ms watcher debounce means the row settles a moment after the op
  confirms — the same latency every reactive tree change already has, and the
  inline editor / confirm dialog close on the reply so the UI never appears
  stuck.
- **Safe-by-construction conflict semantics.** Create and rename refuse an
  existing target; rename refuses a missing source. The refusal is an up-front
  existence check (deterministic), backstopped by the `ErrorKind` mapping. No op
  overwrites content — the buffer channel's "reject, never clobber" ethos,
  applied to structure.
- **Destructive delete is gated and unbatched** (the `#420` pattern the
  `DiscardFile` op established). Recursive directory delete is confined to the
  root and confirmed per path; it is never fanned out over a selection.
- **Agent-agnostic** (constitution): a file op is user intent over the
  filesystem; no code detects or parses an agent. Git-awareness is git's own
  rename detection over the existing status recompute — no agent-specific path.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`): the inline
  editor, the drop-target highlight, the confirm dialog, and any error surface
  use existing theme roles (input/border, list-active, danger), never hardcoded
  hex. Layout dimensions stay plain layout constants, matching Phase 27.
- **Reuse `gpui-component` / `gpui` primitives, never fork them**: `InputState`
  for the inline editor, the vendored confirm-dialog for delete, gpui's
  `on_drag`/`on_drop` for move. No new widget crate.
- **No `.unwrap()` in library code**; `thiserror`-free daemon (it is a binary —
  `anyhow` + a hand-written `Display` for the fs error, matching `buffer.rs`);
  no `todo!()` in merged code.
- **Cohesive `file_tree.rs` slices.** The three client surfaces are sequenced so
  each is a disjoint, self-contained slice of `file_tree.rs` (rename editor →
  context-menu write group + create → drag & drop), minimizing rebase churn; the
  shared client plumbing lands with the first slice.

## Prior art

Consulted the "Explorer overhaul — prior-art index (Phases 27–31)" in
`prior-art.md` (Phase 30 row), the shipped write precedents, and the artboard.

- **Zed remote server** (GPL-3.0, study-only) — the daemon owns the remote
  filesystem and executes file operations **server-side**; the client issues
  intent, not SFTP calls. Phase 30 mirrors this: new `protocol` messages the
  daemon runs with `std::fs`. `remotefs-ssh` / `russh-sftp` are the reference
  fallback the index names and Phase 30 explicitly does **not** take — the
  daemon already runs on the host and needs no SFTP.
- **rift's own write precedents** — `crates/daemon/src/buffer.rs` (atomic
  whole-file write, root confinement via `resolve`, typed `BufferErrorReason`,
  `create_dir_all` for missing parents, the "reject-never-clobber" conflict
  check) and `crates/daemon/src/git_write.rs` + `crates/explorer/src/git_write.rs`
  (the `reply(state, msg) -> DaemonMessage` shape, `spawn_blocking` for
  disk-bound work, the `GitOpResult { op, ok, error }` reply whose state change
  converges through the push recompute, the `#420` confirm-gated `DiscardFile`).
  Phase 30's channel is these patterns applied to tree structure.
- **Paper "Explorer — Redesign" artboard (file `rift`)** — the binding visual
  contract. **State C** is inline rename (the seeded input over the row); the
  write group of **State D** (*New File / New Folder / Rename / Delete*) docks
  into the Phase-29 context menu. Phase 30 realizes State C and the State-D write
  group; the artboard's other columns are earlier/other phases.
- **rift-local grounding**: `crates/app/src/source_control.rs` (the `git_op_tx`
  bridge, `send_op`, `confirm_discard` `#420` dialog — the model for the
  `file_op_tx` bridge and the delete confirm), `crates/app/src/file_tree.rs` +
  `crates/app/src/worktree.rs` (the tree, `WorktreeModel`, `reveal`, the
  push-only `UpdateWorktree` fold), `crates/daemon/src/lib.rs` (`serve_connection`
  reply dispatch, `request_reply`, `buffer_error_reason`).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **File ops run daemon-side via `std::fs`, never client-side SFTP** | The daemon owns the remote filesystem (it watches, reads, saves, and mutates the git index there). File ops are the same capability on paths it already confines. A client SFTP layer would be a second transport for a job the daemon is already positioned to do — the roadmap's phase-30 foundation impact, mirroring Zed's remote server. Ratified in this spec PR. | 2026-07-08 |
| **One `RenamePath { from, to }` message covers both rename and move** | Inline rename (same parent) and drag-drop move (new parent) are the same `fs::rename`. A separate "move" message would duplicate the contract and the handler. The client decides `to`; the daemon does not care whether the parent changed. | 2026-07-08 |
| **The reply carries only `ok`/`error`; the tree change comes through the push-only `UpdateWorktree`** | The git-write channel already proved this: one source of truth for state, reached by the existing recompute. A self-inflicted op triggers the same watcher rescan an agent's edit does, so the delta path is identical (create → `added`, rename → `removed`+`added`, delete → `removed`). The client never optimistically mutates the tree, so a self-inflicted op cannot double-apply or flicker. | 2026-07-08 |
| **Create and rename refuse an existing target (`AlreadyExists`); no op overwrites** | Safe by construction — the buffer channel's "reject, never clobber" applied to structure. An overwrite-move / replace-on-create needs an explicit confirm the artboard does not show; deferred. The refusal is an up-front existence check so it is deterministic, not `ErrorKind`-dependent. | 2026-07-08 |
| **Delete is a confirmed, unbatched, recursive `remove_*`; no trash/undo** | No remote trash exists on a headless host. The `DiscardFile` `#420` precedent already gates a destructive op behind a per-item confirm dialog. Directory delete is recursive (`remove_dir_all`), confined to the root, one confirm per path, never fanned out. Git recovers tracked content; undo is out of scope. | 2026-07-08 |
| **"Git-aware move" = git's native rename detection over the existing status recompute; no gix index rename** | A tracked file moved by `fs::rename` surfaces through the push-only `UpdateGitStatus` / `RepoState` recompute exactly as git sees it, and git collapses delete+add into a rename once the user stages both (via the shipped source-control channel). An explicit gix index rename (`git mv` staging) duplicates that shipped capability and adds index-mutation risk to the fs write path. Deferred; `crates/explorer` stays untouched. This is the minimal solution honoring "git-aware moves where relevant" — the relevance threshold is not met for v1. | 2026-07-08 |
| **Handlers live in a new `crates/daemon/src/file_ops.rs` (no `explorer` change)** | The ops are plain `std::fs` (no gix, given the decision above), so — like `buffer.rs` — they need no crate abstraction (no premature abstraction). The module reuses `buffer::resolve` (`pub(crate)`, already shared with `git_write.rs`) for confinement and mirrors `git_write::reply`'s shape. | 2026-07-08 |
| **`FileOpError` is a typed enum, `FileOp` a tagged echo — mirroring `BufferErrorReason` / `GitWriteOp`** | The client distinguishes `AlreadyExists` (inline rename re-prompt) from `NotFound` / `PermissionDenied` / `InvalidPath` / `Io` for precise UX. No `std::io::Error` crosses the wire (protocol stays dependency-light, the established precedent). The tagged `FileOp` correlates the reply to its request. | 2026-07-08 |
| **Client surfaces sequenced as disjoint `file_tree.rs` slices; plumbing lands with inline rename** | Inline rename, the context-menu write group + create, and drag & drop touch disjoint seams of `file_tree.rs`. Sequencing them (rename → menu/create → drag) and landing the shared `file_op_tx` plumbing with the first slice minimizes rebase churn and avoids a dead-plumbing issue with no consumer. | 2026-07-08 |
| **Phase 30 assumes the Phase-29 context menu has merged** | The roadmap sequences 29 (context-menu shell) before 30. Phase 30 adds only the write items into that existing menu; it does not build the menu. The context-menu write-group issue depends on the Phase-29 menu being present. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Phase 30 — Explorer file operations (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step),
  ordered so the protocol + daemon capability lands before the app surfaces.

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] `PROTOCOL_VERSION` is `9`; the fingerprint test passes with the re-pinned
      value; `docs/protocol.md` documents the file-operation channel and a
      version-9 history line. Serde round-trip tests cover every new variant
      (valid + unknown-tag rejection), and `FileOpResult` omits `error` on
      success.
- [ ] Daemon handler tests (mirroring `git_write.rs` / `buffer.rs` tests):
      `CreateFile` creates an empty file (and missing parents); a second
      `CreateFile` on the same path replies `AlreadyExists` and leaves the file
      untouched; `CreateDir` creates a directory and refuses an existing one;
      `RenamePath` moves a file and refuses a clobber (`AlreadyExists`) and a
      missing source (`NotFound`); `DeletePath` removes a file and a directory
      recursively; a `../escape` path on any op replies `InvalidPath` and
      touches nothing outside the root.
- [ ] Reconciliation: after a daemon op, the client tree reflects the change
      **only** via the subsequent `UpdateWorktree` (create → `added`, rename →
      `removed`+`added`, delete → `removed`); a headless assertion confirms the
      `FileTree` does not mutate its `WorktreeModel` from a `FileOpResult`, and a
      successful create/rename selects + reveals the new row when its `added`
      arrives (pending-reveal).
- [ ] Inline rename (State C): `F2` / *Rename* opens the seeded input with the
      name selected; `Enter` sends `RenamePath` and closes the editor; `Escape`
      cancels with no send; an `AlreadyExists` reply re-opens the editor with the
      error and the typed name intact. Asserted headlessly over the rename-editor
      state where the tests reach it; the visual treatment is the QA gate.
- [ ] Context-menu write group (State D): *New File… / New Folder… / Rename /
      Delete* appear in the Phase-29 menu; New File / New Folder name via the
      inline input and send `CreateFile` / `CreateDir`; *Delete* opens the
      confirm dialog and only a confirm sends `DeletePath` (cancel sends
      nothing, one dialog per file).
- [ ] Drag & drop: dropping a row onto a directory sends `RenamePath` targeting
      that directory; a same-parent drop and a directory dropped into its own
      subtree send **nothing** (guarded client-side). Asserted headlessly over
      the drop-resolution / guard logic; the preview + highlight are the QA gate.
- [ ] `grep` confirms no agent detection introduced, no client-side SFTP, no new
      dependency, and no `crates/explorer` change; the daemon fs logic is
      confined to `crates/daemon/src/file_ops.rs`.
- [ ] Milestone QA (dev channel): create a file and a folder via the context
      menu; rename a file inline; drag a file into a folder; delete a file (with
      confirm) — each appears/moves/disappears in the tree within the reactive
      window with no flicker or duplicate row, and a name collision is refused
      with a legible error. The write surfaces read like the artboard's State C
      and the State-D write group.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A self-inflicted op double-applies (once optimistically, once via the push) and flickers | The single-writer rule: the tree is mutated **only** by `UpdateWorktree`. The op reply drives UX only. A headless test asserts the `FileTree` never mutates its model from a `FileOpResult`. |
| The ~100 ms watcher debounce makes the row appear to "lag" the rename | The inline editor / confirm dialog close on the **reply**, so the UI is never stuck; the row settles a moment later, the same latency every reactive change already has. Pending-reveal selects the new row when it arrives so focus is not lost. |
| Recursive delete removes more than intended | Gated behind the `#420` confirm dialog (per path, never batched), confined to the worktree root by `buffer::resolve`, and the dialog names the path (and warns "and its contents" for a directory). Matches the shipped `DiscardFile` destructive precedent. |
| A rename/move clobbers an existing target | Every create/rename refuses an existing target with `AlreadyExists` (up-front existence check); no op overwrites. Daemon tests assert the refusal leaves the target untouched. |
| A directory dragged into its own subtree corrupts the tree | Refused client-side before the send (target is the source or a descendant) **and** guarded by the daemon (`fs::rename` errors → `Io`). Headless test over the guard. |
| The message-set change merges without the version bump | The fingerprint test fails on any message-set change until `PROTOCOL_VERSION` bumps and the fingerprint re-pins — CI-enforced, the same gate every prior protocol change passed. |
| Phase 29's context menu is not yet merged when the write-group issue runs | The roadmap sequences 29 before 30; the write-group issue depends on the Phase-29 menu and is blocked until it lands (the loop parks it if not). |
| Two app slices both edit `file_tree.rs` → rebase churn | Disjoint seams, sequenced (rename → menu/create → drag); shared plumbing lands with the first slice. `pr-merge` rebases on `BEHIND`. |
| Adding a typed `FileOpError` drifts from the buffer channel's `BufferErrorReason` | It deliberately mirrors it (same enum shape, same "no `std::io::Error` on the wire" rule); reviewed as part of the protocol issue against the existing precedent. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 30 — Explorer
  file operations, the file-ops phase of the explorer overhaul 27–31). Realizes
  the "Explorer — Redesign" artboard's **State C** (inline rename) and the
  write-actions group of **State D** (context menu), plus drag & drop (move).
  Foundation impact ratified here: `protocol` gains the file-operation channel
  (`CreateFile` / `CreateDir` / `RenamePath` / `DeletePath` requests +
  `FileOpResult` reply + `FileOp` / `FileOpError`), `PROTOCOL_VERSION` bumps
  `8 → 9`, and the daemon gains `std::fs` file-op handlers on the remote host it
  already owns — **daemon-side, never client SFTP** (mirroring Zed's remote
  server). Reconciliation of a self-inflicted op is the git-write channel's
  contract applied to tree structure: the push-only `UpdateWorktree` is the
  single writer, so no double-apply or flicker. Git-aware move = git's native
  rename detection over the existing status recompute; a gix index rename is
  deferred, leaving `crates/explorer` untouched. Client surfaces are disjoint,
  sequenced `file_tree.rs` slices with shared plumbing landing on the first.
