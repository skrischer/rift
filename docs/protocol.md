# Client/daemon protocol

The client and daemon exchange length-delimited JSON frames over the SSH-tunnelled
transport (a `u32` big-endian length prefix per message — see
`crates/protocol/src/frame.rs`; the wire format is serialization-agnostic and may
migrate to MessagePack). Every message is a `serde` enum tagged by a `type`
discriminator. The authoritative definitions live in `crates/protocol/src/lib.rs`
(`ClientMessage`, `DaemonMessage`) — this document describes their contract.

## Handshake

```json
// client → daemon
{ "type": "hello",   "version": 1 }
// daemon → client
{ "type": "welcome", "version": 1 }
```

`version` is the wire `PROTOCOL_VERSION`, independent of the crate semver.

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

### Daemon → client

```json
{ "type": "pane_output",     "pane_id": 3, "bytes": [27, 91, 49, 109, ...] }
{ "type": "pane_capture",    "pane_id": 3, "bytes": [27, 91, 51, 49, 109, ...] }
{ "type": "layout_snapshot", "session": "rift", "windows": [ <window>, ... ] }
{ "type": "layout_update",   "session": "rift", "windows": [ <window>, ... ] }
{ "type": "terminal_exit",   "session": "rift", "reason": "server exited" }
```

```jsonc
// <window>  (WindowLayout)
{
  "window_id": 1,
  "name": "editor",
  "active": true,
  "panes": [ <pane>, ... ]
}
// <pane>  (PaneLayout) — geometry in terminal cells, offset from the window's top-left
{ "pane_id": 0, "active": true, "left": 0, "top": 0, "width": 80, "height": 24 }
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
- `terminal_exit` signals the attach's terminal path went down — the tmux server
  exited (`%exit`) or the control-mode child died. It is a per-attach signal, not a
  daemon failure: the daemon keeps serving its other clients, and the client may
  re-`attach` to resume against a still-live session (`reason` is tmux's `%exit`
  text when present, else `null`).

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
{ "type": "repo_state",        "branch": "main", "ahead_behind": { "ahead": 2, "behind": 1 } }
```

> `state_update` (`{ "sessions": [...] }`) was the scaffolding placeholder for
> session/layout state. It was superseded by `layout_snapshot` / `layout_update`
> and **removed** together with the throwaway spike wiring when the daemon took
> ownership of the tmux session (#204).

## Diagnostics

```json
{ "type": "diagnostics", "path": "src/main.rs", "server": "rust-analyzer", "items": [{ "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 9 } }, "severity": "error", "message": "...", "source": "rustc", "code": "E0425" }] }
```

`diagnostics` is keyed by `path` (relative to the worktree root, the same key space as the worktree entries) and by `server` (the daemon-assigned id of the publishing language server). `items` is the complete current set that server reports for the file, replacing whatever it last reported — an empty `items` clears that server's diagnostics for the file while leaving other servers' sets intact. `source` and `code` are omitted when the server provides neither. The diagnostic types are rift's own (`Diagnostic` / `Range` / `Position` / `DiagnosticSeverity`); `lsp-types` does not cross the protocol boundary. Push-only — the client never requests diagnostics.

## Rules

All message types live in `crates/protocol/`. Adding a new message type is a
deliberate API change — both daemon and client must be updated. Keep additions
**additive**: existing consumers must keep compiling and deserializing.

The protocol may migrate to MessagePack if JSON serialization becomes a bottleneck.
Keep message types serialization-agnostic (derive `serde::Serialize` +
`serde::Deserialize`, don't hardcode JSON assumptions).
