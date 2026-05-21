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
{ "type": "file_event",   "kind": "modify", "path": "src/main.rs", "git_status": "modified" }
{ "type": "file_tree",    "root": "/home/dev/project", "entries": [...] }
{ "type": "git_status",   "files": [{ "path": "src/main.rs", "status": "modified" }, ...] }
{ "type": "diagnostics",  "uri": "src/main.rs", "items": [{ "range": {...}, "severity": "error", "message": "..." }] }
```

## Rules

All message types live in `crates/protocol/`. Adding a new message type is a deliberate API change — both daemon and frontend must be updated.

The protocol may migrate to MessagePack if JSON serialization becomes a bottleneck. Keep message types serialization-agnostic (derive `serde::Serialize` + `serde::Deserialize`, don't hardcode JSON assumptions).
