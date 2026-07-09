# Changelog

All notable changes to rift are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions follow SemVer.

## v1.1.0 — 2026-07-09 — Explorer overhaul

Turns the file explorer from a read-only tree into a first-class file manager: a
redesigned visual language, real file-type icons, a right-click context menu,
create / rename / delete / move file operations over the remote daemon, and
in-panel fuzzy search with quick-open — all agent-agnostic, driven only by the
streamed worktree model, and rendered from the new "Explorer — Redesign" Paper
contract.

### Explorer redesign
- New row anatomy: a reserved icon slot, a re-spaced trailing decoration cluster
  (diagnostic dot + right-aligned git-letter lane), redesigned density, and
  refined hover / selected treatment — the visual baseline the rest builds on.
- Redesigned `EXPLORER` header band + action row, re-densified workspace-root
  row, and restyled loading / empty-root placeholders.

### File-type icons
- Real folder, open-folder, chevron, and language-tinted file-type glyphs replace
  the text-glyph markers — a curated MIT Seti icon set embedded in the release
  binary via a delegating asset source (no dev-only gallery gate).
- Extension → icon / tint mapping in the Zed icon-theme shape; tints follow theme
  tokens, not hardcoded colors.

### Context menu
- Right-click menu over tree rows: Open, Reveal in tree, Copy path, Copy relative
  path, Reveal in terminal (an agent-agnostic new tmux window at the target), and
  Collapse all — reusing gpui-component, pointer-only so terminal keys stay untouched.

### File operations
- Create file / create folder, rename (inline editor), delete (with a confirm
  dialog), and move (drag & drop) — executed daemon-side with `std::fs` on the
  remote host over a new protocol file-operation channel (PROTOCOL_VERSION 8 → 9),
  reconciled through the single-writer push-only worktree stream (no flicker,
  no double-apply).

### Search & filter
- In-panel fuzzy filter bar with match emphasis that narrows the tree and
  force-expands the ancestors of matches (nucleo-matcher).
- Jump-to-file quick-open modal over the streamed worktree.
- Discrete multi-select with keyboard range extension and open-many.

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
