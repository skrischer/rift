# Changelog

All notable changes to rift are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions follow SemVer.

## v1.0.0 — 2026-07-08

First tagged release. rift is an agent-centric IDE: a native GPUI shell that
wraps unmodified terminal coding agents running in tmux and wraps reactive code
intelligence around them — a live file tree, git status, LSP diagnostics and
navigation, and a GUI editor, all driven over SSH from a remote daemon.

### Terminal & SSH
- SSH connection and PTY stream over pure-Rust russh, with channel multiplexing.
- GPUI terminal widget on `alacritty_terminal` + `termy_terminal_ui`: GPU-accelerated
  grid rendering at native speed, with mouse and keyboard interaction.
- Terminal output streaming with VTE parsing off the UI thread.

### tmux control-mode integration
- tmux control mode (`-CC`) as the process engine — event-driven notification
  processing, not screen scraping.
- Multi-pane and multi-window awareness; window tabs with per-pane activity
  indicators derived agent-agnostically from OSC-133 shell integration + the bell.
- tmux session switching: daemon session list, live updates, switcher UI, re-attach;
  parallel sessions via a second app instance.
- tmux key-table and status-line mirroring.

### Remote daemon — reactive layer
- Static musl daemon on the remote host, auto-deployable, watching the project root.
- Reactive worktree file-tree sync — the explorer reflects agent edits within seconds.
- Live git status streamed to the client (branch, ahead/behind, per-file state).
- LSP diagnostics streamed from language servers to the client in real time.

### GUI editor + LSP navigation
- First-class GUI editor with remote write-back: read and edit code, save to the remote.
- Inline diagnostics rendered as the agent introduces and resolves errors.
- LSP navigation: hover, go-to-definition, find references, ctrl+click.
- In-file find/replace and go-to-line; editor tabs for multiple open files.

### IDE shell + panels
- Dockable, resizable IDE shell built on `gpui-component` (Dock/Resizable/Tab),
  replacing the fixed three-column layout.
- Explorer panel: git/diagnostic decoration, reveal, keyboard navigation, git
  letter lane, diagnostic dots + rollup, header actions, empty states.
- Source-control panel with visual diff review.
- Problems panel: project-wide diagnostics.
- Command palette; theme & settings.

### Cockpit & editor chrome
- Cockpit chrome: custom title bar (connection/session group), activity rail,
  window-tab redesign, pane headers.
- Composite status line: window list + activity, branch ↑↓ + line totals,
  diagnostic counts, LSP health, Ln/Col, clock.
- Editor chrome: breadcrumb + symbol trail, gutter severity dots, inline
  diagnostic card, hover card, results/outline panels, minimap, conflict dialog.

### Source-control write path
- Stage / unstage / discard / commit via gix — the daemon's first git-write path.
- Per-hunk staging (decompose-and-reapply against HEAD).
- Split and unified diff renderers with word-level emphasis; Split|Unified toggle.

### Robustness & hardening
- Strict message-set version negotiation between app and daemon; client-owned
  daemon version; no silent stream death.
- Stale-daemon restart, stream-death resync, reconnect loop.
- Connection screen as the startup state on every launch; persistent
  daemon-unavailable banner.
- Window-state persistence across restarts.
