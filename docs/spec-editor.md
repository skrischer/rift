# Spec: Editor — the GUI editing surface

> Status: DRAFT
> Created: 2026-06-11
> Completed: —

rift's GUI becomes a first-class editor: it renders the worktree as a navigable file tree, opens a file into a syntax-highlighted code editor, and — at the v1 boundary resolved at the review gate — saves edits back to the remote over a dedicated request/response buffer channel. This is the **render debut** that turns the Phase 3 data layers (worktree, git, diagnostics) into a visible IDE surface, while every process keeps running in tmux.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The worktree renders as a navigable file tree in the GUI (the long-deferred panel render debuts here, consuming the client worktree model); selecting a file opens it.
- [ ] A selected file opens into a `gpui-component` code editor with Tree-sitter syntax highlighting for its language; a large file (tens of thousands of lines) opens and scrolls without loading-time failure (virtualized rendering).
- [ ] File contents move only over a dedicated, request/response **buffer channel**: opening a file issues a read request and the daemon returns that file's content with its `mtime`; no file content ever travels on the worktree structure path.
- [ ] **(write-back — gated by the v1 scope decision)** Editing the buffer and saving writes the whole file back to the remote with the base `mtime`; the daemon rejects a save whose base `mtime` is stale rather than clobbering a newer on-disk version, and the editor surfaces the conflict.
- [ ] **(concurrent writes — gated as above)** When an agent (in a pane) changes a file open in the editor: a **clean** buffer auto-reloads silently (the agent's edit is watched live); a **dirty** buffer surfaces a conflict instead of silently losing either side.
- [ ] The editor is a GUI surface, not a tmux pane: opening files changes no tmux pane/window state, and there is no agent- or editor-process detection or special-casing anywhere in the path.

## Scope

### In scope

- **File-tree render** (`crates/app`): a navigable tree rendered from the client worktree model (`spec-daemon-filetree.md`, issue #111), virtualized for large directories (`gpui-component` `VirtualList`). Bounded to **navigate + open a file** — this is the first consumer of the deferred panel render.
- **Code editor surface** (`crates/app`): open a file into a `gpui-component` code editor (`InputState` in code mode + Tree-sitter highlighting — the component now demoed in the gallery, #180). Read-only viewing always; editing per the v1 scope decision below.
- **Buffer channel** (`crates/protocol` + daemon + `crates/app`): an additive, **request/response** message set — a read request (`OpenFile { path }`) answered by a content response (`FileContent { path, content, mtime }`), and — if write-back is in v1 — a write request (`SaveFile { path, content, base_mtime }`) answered by an ack (`SaveResult { path, mtime }`) or a conflict (`SaveConflict { path, disk_mtime }`). This is the **first request/response pair in the protocol** (the worktree, git, and diagnostics paths are all push-only); the worktree structure path stays content-free.
- **Daemon buffer service** (a daemon module, not a new crate): whole-file read and — if in v1 — atomic whole-file write (temp + rename) on the remote, honoring the same root as the worktree watcher. UTF-8 text; non-UTF-8 / binary is out (see below).
- **Concurrent-write handling** (if write-back is in v1): use the worktree snapshot's per-entry `mtime` (#107) as the "file changed under you" detector for a path open in the editor — clean buffer auto-reloads, dirty buffer surfaces a conflict (depth bounded to detect + reload/keep choice; a full merge UI is out).
- **Single watched root** = the worktree root the file-tree spec watches.

### Out of scope — the hard not-in-v1 list

> `architecture.md` ("The GUI is the editor"): *"Editors eat roadmaps — the editor must never let 'compete with Zed on editor features' pull the roadmap off that axis."* The differentiator is the tmux process layer, not the editor surface. This list is load-bearing.

- **LSP navigation** (hover / go-to-definition / find-references) — the committed sibling sub-spec deferred onto this track from `spec-daemon-lsp.md`; it needs its own request/response data layer and is the natural **next** editor sub-spec once this surface exists.
- **Inline diagnostics render** (squiggles in the editor) and the **buffer→LSP `didChange` shift** (LSP reading the live buffer instead of disk — `spec-daemon-lsp.md` forward-note) — gated into v1 only under scope cut C; otherwise the immediate follow-on.
- **Editor power features** — multi-cursor, find/replace, command palette, minimap, code folding, split editor groups, snippets, autocomplete/completion, format-on-save, rename/refactor, code actions. Explicitly not v1.
- **Rich explorer-panel operations** — create / rename / delete / move files, drag-and-drop, and full git/diagnostics decoration **on the tree** — a follow-on "explorer panel" sub-spec; v1's tree only navigates and opens.
- **Diff view / git hunk staging** in the editor (the GitComet / hunk territory) — future.
- **Pluggable per-file-type editors** (image / binary / notebook viewers, the `EditorHost` contract) — future; v1 is UTF-8 source text only.
- **Multi-window / detached editor windows** — future.

### Open decision (resolved at the review gate)

- **v1 editor scope cut** — how much ships in the first editor milestone. Marked `OPEN` in Prior decisions; resolved via `AskUserQuestion`, then baked in before `READY`.

## Constraints

- **Editor surface is `gpui-component`'s code editor**, not a hand-rolled text engine — it is already a rift dependency and is now demoed in the gallery (#180). Pin the GPUI commit alongside it (the upstream warns to). If its editor proves insufficient, the fallback borrow is `hunk-text` (rope + undo) + `hunk-language` (Tree-sitter registry) (`prior-art.md`, GPL-3.0-compatible) — not a hard dependency.
- **Implementation sequences after the client worktree model (#111)** lands: the editor renders that tree and opens files from it. The buffer channel itself is independent new work but the surface needs the tree. This spec can reach `READY` in parallel; implementation is gated on the file-tree client model.
- **The buffer channel is the deliberate request/response exception** to "state flows through channels" (`CLAUDE.md`): structure and decoration push; **file content is pulled on open and pushed on save**, never broadcast. This must be documented as intentional, not read as a violation.
- **`crates/protocol` stays serialization-agnostic** and content-free on the worktree path; the buffer messages are additive (`protocol.md` Rules). Supersedes nothing on the structure path — the `FileSync { content }` push was already removed (#107).
- **The daemon buffer service must cross-compile to static musl and stay `gpui`-free** — it is daemon-side I/O. Whole-file read/write is plain `tokio::fs`; no new crate (no premature abstraction).
- **Whole-file sync, no deltas** — mirrors the LSP disk-backed full-text decision; the editor sends the whole new file on save, the daemon returns the whole file on open. Editor-delta sync is a later optimization, not v1.
- **Large-file rendering is virtualized** (`prior-art.md` pattern #6) — render only visible lines; mandatory for big files.
- **Atomic writes** — the daemon writes via temp-file + rename so a crash mid-save never truncates the user's file.
- **Agent-agnostic** — no Neovim/editor-process integration, protocol, or detection; the editor is rift's own GUI surface, panes stay black boxes (`CLAUDE.md`, `architecture.md`).
- `anyhow` in the daemon binary, `thiserror` in libraries; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **rift's GUI is a first-class editor with write-back; the process runtime stays tmux** | The 2026-06-10 vision/architecture pivot (#153): `vision.md` reframed engine/cockpit, `architecture.md` "The GUI is the editor". tmux runs agents + dev servers + scripts; the GUI reads/edits/saves code. | 2026-06-10 |
| **Editor surface = `gpui-component` code editor** (`InputState` code-mode + Tree-sitter), not a hand-rolled engine | Precedent-decided: `gpui-component` ships it and it is already a rift dep (demoed in the gallery, #180); Zed is the reference; `hunk` pairs a rope model + Tree-sitter registry (`prior-art.md` Category 1). Don't rebuild a text engine. | 2026-06-11 |
| **Buffer channel is a dedicated request/response path**: read on open, write on save, whole-file, with an `mtime` conflict check; never on the worktree structure path | Constraint-determined: `architecture.md` "File buffer channel" + "File contents and the worktree path are kept separate, on purpose"; the `FileSync { content }` push was removed (#107) precisely because unsolicited content on the structure path was the wrong design. | 2026-06-10 |
| **First request/response pair in the protocol**; the worktree / git / diagnostics paths stay push-only | Constraint-determined: editing is an explicit pull/push (`architecture.md`); the LSP spec deliberately stayed push-only and left request/response to this surface. The "push is source of truth" rule applies to structure/decoration, not to file content. | 2026-06-11 |
| **Concurrent writes via the worktree `mtime` detector**: clean buffer → silent auto-reload (a feature — watch the agent edit live); dirty buffer → conflict surface, never silent clobber | Constraint-determined: `architecture.md` "Named concern: concurrent writes" — the signal already exists in the worktree snapshot (#107). v1 depth is detect + reload/keep choice; a full merge UI is out. | 2026-06-10 |
| **File-tree render lands here** (the deferred panel render debuts), consuming the #111 worktree model; bounded to navigate + open | Constraint-determined: the file-tree / git-status / diagnostics specs each deferred "the rendered panel" to a later sub-spec; the editor is that surface's first real consumer. A tree that opens nothing, or an editor with no way to pick a file, are each half a feature — they belong together. Rich operations + decoration are a later explorer-panel sub-spec. | 2026-06-11 |
| **Daemon buffer service is a module, not a new crate**; whole-file `tokio::fs` with atomic temp+rename writes | No premature abstraction (`CLAUDE.md`): whole-file read/write is simple I/O; a crate is unjustified until complexity demands it. | 2026-06-11 |
| **No Neovim/editor-process integration** — the editor is rift's own GUI surface | Agent-agnostic core (`CLAUDE.md`, `architecture.md` "The GUI is the editor"): panes are black boxes; special-casing an editor process is the one thing the project forbids. | 2026-06-10 |
| **v1 editor scope cut** — A: view-only debut (tree + open + view + buffer-read) · B: view + write-back MVP (B = A plus save + `mtime` conflict + concurrent-write handling) · C: B plus inline diagnostics render — **OPEN, resolved at the review gate** | Neither precedent nor constraint settles where the first milestone cuts. The pivot fixed the *destination* (full write-back editor); the *first bite* is a product judgment the developer owns. `AskUserQuestion` at the review gate. | 2026-06-11 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under an editor-track milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (editor-track milestone)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with the buffer service linked
- [ ] Opening a fixture file renders it in the editor with language-appropriate Tree-sitter highlighting, and the rendered content matches the file on disk byte-for-byte (UTF-8)
- [ ] A read request returns exactly the requested file's content + `mtime`; inspection confirms no file content travels on any worktree / git / diagnostics message (the structure path stays content-free)
- [ ] A large fixture file (≥ 50k lines) opens and scrolls without a loading-time or frame-time failure (virtualized)
- [ ] **(write-back, if in v1)** Editing + saving updates the remote file; re-opening returns the saved content; a save with a stale base `mtime` is rejected as a conflict (no clobber), verified with a daemon-side test
- [ ] **(concurrent writes, if in v1)** An external change to a path open with a **clean** buffer auto-reloads the buffer; an external change with a **dirty** buffer surfaces a conflict and loses neither side, verified with a test driving an out-of-band write
- [ ] Opening / editing files leaves tmux pane and window state unchanged (the editor is GUI, not a pane); a `grep` confirms no agent/editor-process name detection in the editor path

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| **Editor scope creep** — "compete with Zed" pulls the roadmap off the tmux-process-layer axis | The hard not-in-v1 list (Out of scope) is load-bearing and quoted from `architecture.md`; every editor feature past v1 is a deliberate later sub-spec, not a drive-by. |
| `gpui-component`'s code editor is immature or its API churns (git dep) | Spike the editor widget early against a real file; pin the GPUI commit; `hunk-text` + `hunk-language` are the pre-vetted fallback borrow (`prior-art.md`) if the component's editor is insufficient. |
| Concurrent agent writes race the human's edit | The `mtime` detector + conflict surface; the daemon rejects a stale-base save; never a silent clobber. Clean-buffer auto-reload is the intended live-edit feature, not a hazard. |
| Large-file open / scroll jank | Virtualized rendering (pattern #6); bound and measure; the `hunk` perf harness (25k-line diffs) is the reference. |
| Partial / corrupt writes on crash, or non-UTF-8 content | Atomic temp+rename writes; v1 is UTF-8 text only — non-UTF-8 / binary is detected and refused (pluggable viewers are a future sub-spec), never silently mangled. |
| The editor depends on not-yet-built foundation (#111 worktree client model, app↔daemon data wiring) | The spec reaches `READY` now; implementation sequences after the file-tree client model lands — noted in Constraints, mirrors how the Phase 3 sub-specs sequence after each other. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-11: Spec created from `/plan editor`, opening the **editor track** seeded by the 2026-06-10 vision/architecture pivot (#153). Recorded as precedent / constraint-decided: the `gpui-component` editor surface, the dedicated request/response buffer channel (the protocol's first request/response, content off the structure path), the `mtime`-based concurrent-write handling, the file-tree render debut consuming #111 (bounded to navigate + open), the daemon buffer module (no new crate), whole-file sync, and the agent-agnostic no-Neovim stance. The one open decision — the v1 scope cut (A view-only / B view+write-back / C +inline-diagnostics) — flagged for the review gate. The hard not-in-v1 list is recorded to hold the roadmap on the tmux-process-layer axis per `architecture.md`.
