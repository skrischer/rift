# WebSocket protocol

All communication runs over a single WebSocket connection, tunneled through SSH port forwarding. Messages are JSON-encoded with a `type` discriminator.

## Frontend → Daemon

```json
{ "type": "input",        "pane_id": 3, "data": "ls\n" }
{ "type": "resize_pane",  "pane_id": 3, "cols": 120, "rows": 40 }
{ "type": "tmux_command",  "cmd": "split-window -h" }
```

## Daemon → Frontend

```json
{ "type": "pane_output",  "pane_id": 3, "cells": [...] }
{ "type": "state_update", "sessions": [...] }
{ "type": "worktree_snapshot", "root": "/home/dev/project", "entries": [{ "path": "src/main.rs", "kind": "file", "ignored": false, "mtime": {...} }], "final_chunk": true }
{ "type": "update_worktree",   "added": [...], "changed": [...], "removed": ["src/old.rs"] }
{ "type": "git_status",   "files": [{ "path": "src/main.rs", "status": "modified" }, ...] }
{ "type": "diagnostics",  "path": "src/main.rs", "server": "rust-analyzer", "items": [{ "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 9 } }, "severity": "error", "message": "...", "source": "rustc", "code": "E0425" }] }
```

`diagnostics` is keyed by `path` (relative to the worktree root, the same key space as the worktree entries) and by `server` (the daemon-assigned id of the publishing language server). `items` is the complete current set that server reports for the file, replacing whatever it last reported — an empty `items` clears that server's diagnostics for the file while leaving other servers' sets intact. `source` and `code` are omitted when the server provides neither. The diagnostic types are rift's own (`Diagnostic` / `Range` / `Position` / `DiagnosticSeverity`); `lsp-types` does not cross the protocol boundary. Push-only — the client never requests diagnostics.

## Rules

All message types live in `crates/protocol/`. Adding a new message type is a deliberate API change — both daemon and frontend must be updated.

The protocol may migrate to MessagePack if JSON serialization becomes a bottleneck. Keep message types serialization-agnostic (derive `serde::Serialize` + `serde::Deserialize`, don't hardcode JSON assumptions).
