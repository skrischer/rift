# Client/daemon protocol

The client and daemon exchange length-delimited JSON frames over the SSH-tunnelled
transport (a `u32` big-endian length prefix per message — see
`crates/protocol/src/frame.rs`; the wire format is serialization-agnostic and may
migrate to MessagePack). The decoder rejects any length prefix above
`MAX_FRAME_LEN` (64 MiB) as stream corruption; both ends treat frame errors as
connection-fatal. Every message is a `serde` enum tagged by a `type`
discriminator. The authoritative definitions live in `crates/protocol/src/lib.rs`
(`ClientMessage`, `DaemonMessage`) — this document describes their contract.

## Handshake

```json
// client → daemon
{ "type": "hello",   "version": 5 }
// daemon → client
{ "type": "welcome", "version": 5 }
```

`version` is the wire `PROTOCOL_VERSION`, independent of the crate semver.

## Versioning policy

`PROTOCOL_VERSION` (`crates/protocol/src/lib.rs`) is a single integer with
**strict equality** semantics: client and daemon must run the exact same
version. There is no cross-version compatibility or message translation —
both binaries build from one repo, so skew is resolved by replacing the stale
binary, never by tolerating it (`docs/spec-connection-robustness.md`).

- **Any** change to the message set requires a bump: a variant added, removed,
  or renamed; a field added, removed, or renamed; a field's **type** changed;
  a serde attribute changed. This holds even for changes that are
  wire-compatible in one direction (e.g. an additive `#[serde(default)]`
  field) — under strict equality, "compatible" skew is still skew.
- The message set is **pinned by a fingerprint test** in `crates/protocol`
  (`PROTOCOL_FINGERPRINT`, kept beside the version constant): a stable FNV-1a
  hash over the serde-visible surface of `ClientMessage` and `DaemonMessage` —
  container serde attributes, variant names, field names, and field types,
  ignoring comments and formatting. Changing either enum without re-pinning
  fails `cargo test -p rift-protocol`; the failure message instructs to bump
  `PROTOCOL_VERSION` and re-pin. A message-set change without a version bump
  therefore cannot pass CI.
- Version skew is negotiated at the handshake, not mid-stream: the daemon
  answers a mismatched `hello` with `welcome` carrying its **own** version and
  closes cleanly without streaming; the client owns the resolution (stop the
  stale daemon via the pidfile, redeploy, respawn, re-handshake).
- The `welcome` is written **per connection**, never on the daemon's shared
  event bus — one client's handshake (matched or mismatched) cannot surface on
  a healthy concurrent connection's stream (relevant for the shared stable+dev
  daemon).

History: version 5 removes the tmux status-line CONTENT mirror pair
`query_status_line` / `status_line_reply` (superseded by the composite status
line's native segments, `docs/spec-status-line.md`); version 4 adds
`RepoState`'s `lines_added`/`lines_removed` working-tree line totals and the
`LspStatus` push (`docs/spec-status-line.md`); version 3 adds the session-list
pair `query_session_list` /
`session_list_reply` (`docs/spec-session-switch.md`); version 2 pins the
message set as of the connection-robustness phase (fingerprint test
introduced); version 1 was the original handshake constant, carried but never
enforced.

## Terminal streaming

The terminal path is a single narrow seam: the client attaches to a named tmux
session, then drives it with input/resize/command messages while the daemon
streams pane bytes and layout state back.

### Client → daemon

```json
{ "type": "attach",       "session": "rift" }
{ "type": "input",        "pane_id": 3, "data": "ls\n" }
{ "type": "resize_pane",  "pane_id": 3, "cols": 120, "rows": 40 }
{ "type": "tmux_command", "cmd": "split-window -h" }
{ "type": "capture_pane", "pane_id": 3, "start": "-", "end": "-128", "join": false }
{ "type": "query_key_table" }
{ "type": "query_session_list" }
```

- `attach` carries the **session name end-to-end**, so the `RIFT_SESSION` knob
  (e.g. `RIFT_SESSION=rift-dev` for the dogfooding isolation channel) survives the
  protocol seam. The daemon runs attach-or-create (`new-session -A -s <session>`)
  per attach, one tmux control-mode attach per connected client.
- `input` carries raw keystroke bytes; `data` is **opaque** — the protocol
  forwards it to tmux and never interprets pane input or output (agent-agnostic).
- `resize_pane` maps to this client's tmux resize/`refresh-client -C`.
- `tmux_command` is a raw command line emitted on this client's control-mode attach.
- `capture_pane` requests a bounded `capture-pane` of pre-attach scrollback:
  `start`/`end` are tmux `-S`/`-E` line addresses (`"-"` for the extreme, a
  negative number for a history offset), `join` is `-J`. The daemon answers with
  exactly one `pane_capture` for this pane — a **request/response** exchange,
  separate from the live `%output` stream.
- `query_key_table` asks the daemon to (re-)run `list-keys` and `show-options`
  for the tmux key-table mirror (`docs/spec-tmux-keytable-mirroring.md`). The
  daemon answers with exactly one `key_table_reply` — a **request/response**
  exchange. The daemon also issues this query unprompted on `attach`/reconnect
  (mirroring the layout query), so the client only sends `query_key_table`
  explicitly: on a user-triggered refresh, or after dispatching a
  binding-mutating bound command (`bind-key`/`unbind-key`/`source-file`, or
  `set-option` touching `prefix`/`prefix2`/`repeat-time`).
- `query_session_list` asks the daemon to (re-)run `list-sessions` for the
  session switcher (`docs/spec-session-switch.md`). The daemon answers with
  exactly one `session_list_reply` — a **request/response** exchange, same
  sibling convention as `query_key_table`. The daemon also re-issues the
  query on its own — coalesced like the layout re-query — whenever tmux
  signals session churn (`%sessions-changed`, `%session-renamed`,
  `%client-session-changed`) and pushes the fresh reply unprompted, so the
  client's list stays live without polling; the client sends
  `query_session_list` explicitly only for an on-demand refresh (e.g.
  opening the switcher).

### Daemon → client

```json
{ "type": "pane_output",     "pane_id": 3, "bytes": [27, 91, 49, 109, ...] }
{ "type": "pane_capture",    "pane_id": 3, "bytes": [27, 91, 51, 49, 109, ...] }
{ "type": "layout_snapshot", "session": "rift", "windows": [ <window>, ... ] }
{ "type": "layout_update",   "session": "rift", "windows": [ <window>, ... ] }
{ "type": "terminal_exit",   "session": "rift", "reason": "server exited" }
{ "type": "key_table_reply", "list_keys": "bind-key -T prefix c new-window\n...", "options": "prefix C-b\nrepeat-time 500\n..." }
{ "type": "session_list_reply", "sessions": [{ "id": 0, "name": "rift", "windows": 3, "attached": true }] }
```

```jsonc
// <window>  (WindowLayout)
{
  "window_id": 1,
  "window_index": 0,
  "name": "editor",
  "active": true,
  "panes": [ <pane>, ... ]
}
// <pane>  (PaneLayout) — geometry in terminal cells, offset from the window's top-left
{ "pane_id": 0, "active": true, "left": 0, "top": 0, "width": 80, "height": 24, "is_shell": true }
```

- `pane_output` carries **raw terminal bytes, not cells** (the VTE-location spike
  verdict, #201): the client feeds them straight into its
  `alacritty_terminal::Term`. The byte run is opaque ANSI; ordering is per-pane.
- `pane_capture` is the reply to a `capture_pane`: the captured pre-attach
  scrollback as opaque bytes (tmux-decoded, ANSI preserved via `capture-pane -e`),
  or empty on a capture error. The client routes it to its **scrollback history**,
  not the live `Term` — this is how pre-attach scrollback keeps working over the
  daemon seam (the "command emission" path of the design spec).
- `layout_snapshot` is the complete window/pane layout for the session, sent once
  per attach as the consistency-contract baseline (below). The client replaces its
  entire layout model with it.
- `layout_update` is the **full latest layout** after a structural change (window
  add/close, pane split/resize, active-window switch) — a replace, not a delta, so
  applying it is idempotent.
- `PaneLayout.is_shell` is tmux's own `#{==:#{pane_current_command},#{b:default-shell}}`
  format comparison (`LAYOUT_QUERY`, `crates/daemon/src/terminal.rs`) — the pane's
  foreground command against the basename of its session's `default-shell` option,
  evaluated server-side into a boolean. The client never carries a shell name list
  or command taxonomy (agent-agnostic, #510); it only reads the resulting flag.
- `terminal_exit` signals the attach's terminal path went down — the tmux server
  exited (`%exit`) or the control-mode child died. It is a per-attach signal, not a
  daemon failure: the daemon keeps serving its other clients, and the client may
  re-`attach` to resume against a still-live session (`reason` is tmux's `%exit`
  text when present, else `null`).
- `key_table_reply` is the reply to `query_key_table` (and the unprompted
  attach-time query): the raw `list-keys` and `show-options` output,
  newline-joined and tmux-decoded but otherwise **uninterpreted by the
  daemon** — the client parses both with `rift_terminal::keytable` into the
  mirrored key-table lookup and prefix/repeat options. This is tmux's own
  config text, not pane content.
- `session_list_reply` is the reply to `query_session_list` (and the
  unprompted churn-driven re-queries): every tmux session on the server,
  parsed daemon-side from a tab-separated `list-sessions` format with the
  session name as the LAST field (the layout-query convention, so names with
  spaces or tabs survive). Per session: `id` is tmux's `$<n>` session id (the
  rename-stable key), `name` the current name, `windows` the window count,
  and `attached` whether at least one client is attached (tmux's
  attached-client count folded to a bool). The client replaces its whole
  session list on every arrival (replace semantics, like `layout_update`).
  Which session THIS client is attached to is not carried here — the
  `session` string on `layout_snapshot`/`layout_update` already owns that.

### Snapshot ↔ live-stream consistency contract

On `attach` the daemon opens this client's tmux control-mode attach, sends exactly
one `layout_snapshot`, and from the same instant streams the live notifications
(`layout_update`, `pane_output`). The seam between the snapshot and the live stream
is **gap-free and duplicate-free**:

- **No gap** — the daemon subscribes to tmux's notification stream before it reads
  the snapshot, so every change at or after the snapshot instant appears in the
  live stream; none is lost in the handover.
- **No duplicate** — the snapshot is the baseline state, not a replay: no layout
  change already reflected in it is re-sent as a live event. Because `layout_update`
  carries the full layout (replace semantics), even a coalesced or reordered change
  converges without double-applying.

On reconnect the daemon reattaches and sends a fresh `layout_snapshot`; the client
resets to it and resumes from the new baseline (tmux is the session persistence, so
no terminal state is lost). Pane **scrollback** that predates the attach is fetched
separately via the `capture_pane` / `pane_capture` request/response pair and is
outside this contract — the contract governs only the seam between the attach
snapshot and the live `%output` stream.

## Worktree, git, and repo state

The reactive file/git messages are defined in `crates/protocol` and specified by
their own phases (explorer / git-status); summarized here for completeness:

```json
{ "type": "worktree_snapshot", "root": "/home/dev/project", "entries": [ ... ], "final_chunk": true }
{ "type": "update_worktree",   "added": [ ... ], "changed": [ ... ], "removed": ["src/old.rs"] }
{ "type": "update_git_status", "changed": [ ... ], "cleared": ["was_dirty.rs"] }
{ "type": "repo_state",        "branch": "main", "ahead_behind": { "ahead": 2, "behind": 1 }, "lines_added": 12, "lines_removed": 3 }
```

> `state_update` (`{ "sessions": [...] }`) was the scaffolding placeholder for
> session/layout state. It was superseded by `layout_snapshot` / `layout_update`
> and **removed** together with the throwaway spike wiring when the daemon took
> ownership of the tmux session (#204).

`repo_state.lines_added`/`lines_removed` are the working-tree line totals
(`docs/spec-status-line.md`): `git diff HEAD --numstat` semantics — current
worktree content vs `HEAD`, regardless of staging — plus untracked text file
additions, computed with `gix` on the same debounced recompute tick as the
rest of this message (no dedicated timer). A rename diffs the destination
path's worktree content against its rewrite **source** blob rather than the
(nonexistent) `HEAD` entry at the new path, so a pure rename contributes
`0`/`0`. Per-file work is capped and a binary file is skipped (mirroring the
source-control diff channel's sentinels below) — one oversized or binary file
contributes nothing rather than failing the whole recompute. Both fields are
always present (`0` on a clean worktree), never optional.

## Diagnostics

```json
{ "type": "diagnostics", "path": "src/main.rs", "server": "rust-analyzer", "items": [{ "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 9 } }, "severity": "error", "message": "...", "source": "rustc", "code": "E0425" }] }
```

`diagnostics` is keyed by `path` (relative to the worktree root, the same key space as the worktree entries) and by `server` (the daemon-assigned id of the publishing language server). `items` is the complete current set that server reports for the file, replacing whatever it last reported — an empty `items` clears that server's diagnostics for the file while leaving other servers' sets intact. `source` and `code` are omitted when the server provides neither. The diagnostic types are rift's own (`Diagnostic` / `Range` / `Position` / `DiagnosticSeverity`); `lsp-types` does not cross the protocol boundary. Push-only — the client never requests diagnostics.

## Language server health (`docs/spec-status-line.md`)

```json
{ "type": "lsp_status", "server": "rust-analyzer", "state": "running" }
```

`lsp_status` is keyed by `server` — the server's **stable name** (the binary,
e.g. `"rust-analyzer"`), NOT the per-spawn server id `diagnostics` keys by: a
restart mints a fresh internal id, but the status-line health dot asks "is my
rust-analyzer OK", which is name-scoped. `state` is one of `starting` /
`running` / `crashed` — there is no `stopped`, since a server the daemon has
observed is never deliberately stopped while a client is attached. Emitted by
the daemon's LSP registry around its observe cycle: `starting` when a
(re)start is triggered, `running` once the `initialize` handshake completes,
`crashed` once a dead instance is pruned (detected on the next observe after
the server exits) or a (re)start attempt fails. Push-only, and replayed once
per known server behind `welcome` so a (re)attaching client sees current
health immediately.

### Live-buffer feed (`spec-editor.md`, cut C)

```json
// client → daemon
{ "type": "buffer_changed", "path": "src/main.rs", "content": "..." }
{ "type": "buffer_closed",  "path": "src/main.rs" }
```

The LSP document model is **disk-backed by default** (`spec-daemon-lsp.md`): the daemon feeds the language server on-disk content as the worktree changes. Once rift's own editor opens a file, the editor's **live buffer** becomes the LSP's source of truth for that file — the disk→buffer source-of-truth shift the LSP spec's forward-note reserved — so an **unsaved** edit's errors surface without a save first.

- `buffer_changed` carries the open buffer's whole current UTF-8 `content` (the editor debounces it on edit). The daemon forwards it to the matching server(s) as a `didChange` (version bumped), so diagnostics recompute against the buffer, not disk. It is **not** a write: nothing touches the filesystem. While a `path` has a live buffer, the daemon suppresses the disk-driven `didChange` for it (the buffer owns it), so an agent's on-disk write does not clobber the live buffer's diagnostics.
- `buffer_closed` ends the feed (the editor closed the file, switched files, or auto-reloaded). The daemon drops the override and reverts `path` to the disk-backed baseline — re-reading on-disk content and pushing it as a `didChange` so diagnostics converge back to the on-disk state, coherently with the pre-buffer behavior.

Both are **additive and push-only** — there is no reply; diagnostics flow back over the existing push-only `diagnostics` path. The `content` carries no `mtime`: the feed is the LSP source of truth, not a save (saves stay on the buffer channel below).

## Buffer channel

The buffer channel is the **first request/response pair in the protocol** and the
only path that carries file content. Worktree, git, and diagnostics all push
structure and decoration; **file content is pulled on open and pushed on save**,
never broadcast — so the worktree structure path stays content-free (the
`FileSync { content }` push was removed in #107 precisely because unsolicited
content on the structure path was the wrong design). Specified by `spec-editor.md`.

```json
// client → daemon
{ "type": "open_file", "path": "src/main.rs" }
{ "type": "save_file", "path": "src/main.rs", "content": "...", "base_mtime": { "secs_since_epoch": 5, "nanos_since_epoch": 7 } }
// daemon → client
{ "type": "file_content",  "path": "src/main.rs", "content": "...", "mtime":      { "secs_since_epoch": 5, "nanos_since_epoch": 7 } }
{ "type": "save_result",   "path": "src/main.rs", "mtime":      { "secs_since_epoch": 9, "nanos_since_epoch": 0 } }
{ "type": "save_conflict", "path": "src/main.rs", "disk_mtime": { "secs_since_epoch": 9, "nanos_since_epoch": 0 } }
```

- `open_file` is the read request: it carries only the `path` (relative to the
  worktree root, the same key space as the worktree entries). The daemon answers
  with exactly one `file_content` for that path — the file's whole UTF-8 content
  plus its `mtime`. The request carries no content.
- `save_file` is the write request: the whole new UTF-8 `content` (no deltas) plus
  `base_mtime`, the `mtime` the editor read on open. The daemon answers with one
  `save_result` (the write landed, carrying the new on-disk `mtime`) or one
  `save_conflict` (the on-disk `mtime` no longer matches `base_mtime`, so the
  write was **rejected, not clobbered**; `disk_mtime` is the current on-disk value
  for the editor to rebase against). The daemon's write is atomic (temp + rename).
- The `mtime` / `base_mtime` / `disk_mtime` fields are the **identical
  `std::time::SystemTime`** (same wire shape `{ secs_since_epoch, nanos_since_epoch }`)
  as the worktree entry's `mtime` (#107), so the base read on the structure path
  can be compared against a save on the buffer path — the concurrent-write
  detector. They are not independently sampled clock values.

## Navigation channel

The navigation channel is the **third request/response family in the protocol**,
adding hover, go-to-definition, and find-references. Specified by
`docs/spec-lsp-navigation.md`.

### Request-id correlation convention

Every navigation request carries an explicit `NavRequestId` (`u64` counter,
client-assigned). The daemon echoes it unchanged in the matching response so
the client can:

1. **Correlate** the response to the inflight request.
2. **Drop stale responses** — when the user has moved on, the client compares
   the echoed `id` against its current inflight id and silently discards the
   response if they differ. A slow server can never land its result on the wrong
   file or position.

This explicit id is the protocol's **request-id correlation convention**,
established by the navigation family. The buffer channel correlates by `path`,
which is sufficient there (each file has at most one open-or-save in flight at
a time); the navigation channel requires an explicit id because concurrent
requests can target the **same file at different positions**, making path-only
correlation ambiguous. Whether the buffer channel retroactively adopts the id
convention is deferred — it is additive either way.

### Client → daemon

```json
{ "type": "hover_request",      "id": 1, "path": "src/main.rs", "position": { "line": 5, "character": 10 } }
{ "type": "definition_request", "id": 2, "path": "src/main.rs", "position": { "line": 5, "character": 10 } }
{ "type": "references_request", "id": 3, "path": "src/main.rs", "position": { "line": 5, "character": 10 } }
```

- `path` is relative to the worktree root — the same key space as worktree
  entries. `position` uses the same `Position` type as `Diagnostics` (one
  convention, never two). `id` is the correlation key; a monotonically
  increasing `u64` counter starting at `0` is the canonical client choice.

### Daemon → client

```jsonc
// hover_response: None when the server has nothing to say (silent no-op for the UI)
{ "type": "hover_response", "id": 1, "content": { "markdown": "**fn foo()** — ...", "range": { "start": {...}, "end": {...} } } }
{ "type": "hover_response", "id": 1, "content": null }

// definition_response: empty targets = no definition found (silent no-op)
{ "type": "definition_response", "id": 2, "targets": [{ "path": "src/lib.rs", "range": {...}, "line_preview": "pub fn foo() {}" }] }
{ "type": "definition_response", "id": 2, "targets": [] }

// references_response: empty locations = no references found (silent no-op)
{ "type": "references_response", "id": 3, "locations": [{ "path": "src/main.rs", "range": {...}, "line_preview": "    foo(x)" }] }
{ "type": "references_response", "id": 3, "locations": [] }
```

- `id` echoes the request's `NavRequestId` — always present on responses.
- `content` is `null` (not absent) when the server has no hover for the
  position; the client shows no popover (silent no-op, never an error).
- A `definition_response` with multiple `targets` (e.g. Rust trait method
  impls) is surfaced in the same jump-list the references path uses; a single
  target jumps directly.
- `out_of_root` is `true` (present on the wire) when `path` is absolute and
  lives outside the worktree root (stdlib / registry dependency). The client
  opens these read-only — no save path. Omitted (defaults to `false`) for
  in-root targets.
- `line_preview` is a trimmed source line for jump-list display; omitted when
  the daemon cannot read the file (never an error path).
- `range` in `hover_content` is omitted when the server does not supply it.

### Daemon-side implementation notes (follow-on issues)

The navigation channel types are defined in `crates/protocol` (#193).
Daemon routing (the LSP request path in `crates/lsp` and `crates/daemon`) is
wired in follow-on issues. Until then, navigation requests received by the
daemon are absorbed by the shared dispatch loop's defensive no-op arm.

The daemon owns **offset-encoding translation**: LSP servers default to UTF-16
offsets; `crates/protocol`'s `Position` speaks rift's own position (UTF-8
character offset), and `crates/lsp` translates against the document text it
already syncs — the client and protocol never see UTF-16.

## Source-control diff channel

The diff channel is the **fifth request/response family in the protocol**,
pulling a structured line diff for the source-control panel's review view.
Specified by `docs/spec-source-control.md`.

```json
// client → daemon
{ "type": "request_diff", "path": "src/main.rs" }
// daemon → client
{ "type": "file_diff", "path": "src/main.rs", "diff": { "kind": "hunks", "hunks": [ <hunk>, ... ] } }
{ "type": "file_diff", "path": "assets/logo.png", "diff": { "kind": "binary" } }
{ "type": "file_diff", "path": "big.bin", "diff": { "kind": "too_large" } }
```

```jsonc
// <hunk>  (DiffHunk) — old_start/new_start are 1-based, matching unified-diff hunk headers
{
  "old_start": 1, "old_len": 3,
  "new_start": 1, "new_len": 3,
  "lines": [
    { "kind": "context", "content": "one" },
    { "kind": "remove",  "content": "two" },
    { "kind": "add",     "content": "TWO" },
    { "kind": "context", "content": "three" }
  ]
}
```

- `request_diff` carries only `path` (relative to the worktree root, the same
  key space as [`WorktreeEntry::path`]). The daemon computes the diff of the
  current on-disk content against `path`'s blob at HEAD — always
  worktree-vs-HEAD, regardless of staging state — and answers with exactly one
  `file_diff` for that path. Computed on request, like `open_file`, not
  pushed: a diff is only needed for the file currently under review. No
  `NavRequestId`: at most one diff is ever inflight per path, so path-keyed
  correlation (like the buffer channel) is sufficient.
- `diff` is tagged by `kind`: `"hunks"` carries the structured line diff
  (`hunks` is `[]` when the worktree content is identical to HEAD — not
  omitted, not a sentinel); `"binary"` means either side is binary content;
  `"too_large"` means the diff exceeds the daemon's size ceiling (~20k changed
  lines or ~2MB per side). Both sentinels carry no `hunks` field.
- Each line's `kind` mirrors unified-diff's context/add/remove roles; `content`
  has its line terminator stripped.

### Daemon-side implementation notes

The diff types and the `gix`-based blob-diff compute (`crates/explorer`) are
implemented in #335. The daemon-side `request_diff` → `file_diff` handler
(#336, `crates/daemon/src/diff.rs`) is answered per connection, the same
request/response shape as the buffer channel's `open_file` → `file_content`:
the path is confined to the worktree root exactly like a buffer write (no
out-of-root carve-out), and the compute runs off the async I/O path via
`spawn_blocking`.

## Rules

All message types live in `crates/protocol/`. Adding a new message type is a
deliberate API change — both daemon and client must be updated. Keep additions
**additive**: existing consumers must keep compiling and deserializing. Every
message-set change — additive or not — bumps `PROTOCOL_VERSION` and re-pins
the fingerprint (see Versioning policy above); the fingerprint test enforces
this mechanically.

Most paths are **push-only** (structure and decoration: worktree, git,
diagnostics — "push is the source of truth"). The request/response
exceptions are deliberate and scoped: `capture_pane` / `pane_capture` for
pre-attach scrollback; `query_key_table` / `key_table_reply` for the tmux
key-table mirror;
`query_session_list` / `session_list_reply` for the session switcher (same
sibling pattern, plus unprompted churn-driven pushes); the **buffer
channel** (`open_file` → `file_content`, `save_file` → `save_result` /
`save_conflict`) for file content; the **navigation channel**
(`hover_request` → `hover_response`, `definition_request` →
`definition_response`, `references_request` → `references_response`) for LSP
pull queries; and the **diff channel** (`request_diff` → `file_diff`) for the
source-control panel's review diff. The push-only rule governs structure and
decoration, never request/response pairs.

The protocol may migrate to MessagePack if JSON serialization becomes a bottleneck.
Keep message types serialization-agnostic (derive `serde::Serialize` +
`serde::Deserialize`, don't hardcode JSON assumptions).
