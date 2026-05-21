# rift — Project context

Quick reference for planning sessions. For full detail, read the linked documents.

## What is rift?

An agent-centric IDE for terminal-based coding agents. Native GUI shell (Rust + GPUI) that wraps tmux and provides reactive IDE features — file explorer, LSP diagnostics, git status — while agents run unmodified in terminal panes. Remote-first via SSH.

See [vision.md](vision.md) for the full problem statement and positioning.

## Tech stack

Rust (2021 edition), Tokio, GPUI (from Zed), alacritty_terminal, termy_terminal_ui, russh. Build target: Linux/X11 (GPUI native). Daemon: x86_64-unknown-linux-musl (static).

## Repository

GitHub: `skrischer/rift` — GPL-3.0-or-later

Workspace with 8 crates: `app` (GPUI binary), `ssh` (connection/PTY), `terminal` (widget), `daemon` (remote, Phase 3+), `tmux-core` (Phase 3+), `explorer` (Phase 3+), `protocol` (shared types), `plugin-api` (Phase 3+).

See [architecture.md](architecture.md) for the full architecture.

## Current status

**Phase 2c complete** (2026-05-20): SSH -> tmux control mode -> per-pane VTE -> split-tree layout -> focus routing. Multi-pane awareness working.

**Next: Phase 2d** — Tab bar for tmux window switching, statusbar enrichment (CWD subscriptions, git branch, pane command). See [spec-phase2d-tabbar.md](spec-phase2d-tabbar.md).

**After that: Phase 3** — Remote daemon with file tree, git status, and LSP on the remote host.

See [roadmap.md](roadmap.md) for the full phase overview.

## Key documents in this directory

| File | Purpose |
|---|---|
| [roadmap.md](roadmap.md) | Phase overview and current status |
| [architecture.md](architecture.md) | System architecture (current + target) |
| [vision.md](vision.md) | Problem, solution, positioning, principles |
| [patterns.md](patterns.md) | Coding patterns reference (for Claude Code) |
| [protocol.md](protocol.md) | WebSocket protocol spec (Phase 3) |
| [tmux-reference.md](tmux-reference.md) | tmux control mode protocol and pitfalls |
| [spec-template.md](spec-template.md) | SDD spec template for new implementation plans |
| [handover-conventions.md](handover-conventions.md) | Rules for Cowork <-> Claude Code exchange |
