# rift — Roadmap

Living document tracking project phases, current status, and planned work.

## Phase overview

| Phase | Name | Status | Spec |
|---|---|---|---|
| 1 | SSH terminal + GPUI rendering | COMPLETED | (predates spec system) |
| 2 (migration) | termy_terminal_ui adoption | COMPLETED 2026-05-07 | [archive/spec-terminal-migration.md](archive/spec-terminal-migration.md) |
| 2a+2b | tmux control mode integration | COMPLETED 2026-05-08 | [archive/spec-phase2ab-control-mode.md](archive/spec-phase2ab-control-mode.md) |
| 2c | Multi-pane awareness | COMPLETED 2026-05-20 | [archive/spec-phase2c-multipane.md](archive/spec-phase2c-multipane.md) |
| 2d | Tab bar + statusbar enrichment | IN PROGRESS | [spec-phase2d-tabbar.md](spec-phase2d-tabbar.md) |
| 2e | gpui-component UI foundation | IN PROGRESS | [spec-gpui-component-adoption.md](spec-gpui-component-adoption.md) |
| 3 | Remote daemon | PLANNED | — |

## Current focus

**Phase 2d: Tab bar + statusbar enrichment**

Tab bar for tmux window switching, CWD from subscriptions instead of snapshot polling, git branch display, pane command name, connection status indicator. This completes the tmux integration before moving to the daemon architecture.

Spec: [spec-phase2d-tabbar.md](spec-phase2d-tabbar.md)

Phase 2e (gpui-component UI foundation) is being built first as the substrate for the 2d displays: `Root`/theme wiring (#26), an app-wide Catppuccin theme (#33), and the window tab bar (#27) have merged; the statusbar container rebuild (#28) is the remaining step before the 2d data fields land on it. Spec: [spec-gpui-component-adoption.md](spec-gpui-component-adoption.md)

## What comes after Phase 2d

**Phase 3: Remote daemon** — the major architectural shift. Splits the monolithic app into GPUI frontend + remote daemon connected via WebSocket over SSH port-forward. The daemon handles file watching (inotify), git status, and language servers (LSP) on the remote host. The frontend becomes a thin rendering client.

Key open decisions for Phase 3:
- VTE parsing location: client-side (current, simpler) vs. daemon-side (less data over SSH)
- File sync strategy: daemon serves file tree on demand vs. full directory sync
- LSP lifecycle: daemon starts/stops language servers, or always-on per project

These decisions need specs before implementation starts. Phase 3 is the biggest architectural change since the project began and will likely need multiple sub-specs (daemon scaffolding, file tree, git status, LSP integration, protocol migration).

See [prior-art.md](prior-art.md) for reference implementations (Zed `remote_server`, Lapce proxy, Arbor, `async-lsp`) and candidate dependencies to draw from when writing these specs.

## North star

The goal is Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a pane, see the file explorer highlight every file it touches, diagnostics update in real-time, git panel shows clean diffs. Review visually, approve, move on.

Phase 2d gets us the tab bar and enriched statusbar. Phase 3 gets us the file explorer and diagnostics. Together they deliver the core north star scenario.
