# rift — Roadmap

> Living document: the sequenced queue of phases — the hand-off to `/loopkit:plan`,
> which picks the next unplanned phase, creates its spec + issues, and links them
> back here. No status markers — progress lives in the linked GitHub milestones
> and issues; specs carry only `DRAFT`/`READY`.

## Phase overview

| Phase | Name | Spec | Milestone |
|---|---|---|---|
| 1 | SSH terminal + GPUI rendering | (predates spec system) | — |
| 2 | termy_terminal_ui adoption | [archive/spec-terminal-migration.md](archive/spec-terminal-migration.md) | — |
| 2a+2b | tmux control mode integration | [archive/spec-phase2ab-control-mode.md](archive/spec-phase2ab-control-mode.md) | — |
| 2c | Multi-pane awareness | [archive/spec-phase2c-multipane.md](archive/spec-phase2c-multipane.md) | — |
| 2d | Tab bar + statusbar enrichment | [spec-phase2d-tabbar.md](spec-phase2d-tabbar.md) | [Phase 2d](https://github.com/skrischer/rift/milestone/1) |
| 2e | gpui-component UI foundation | [archive/spec-gpui-component-adoption.md](archive/spec-gpui-component-adoption.md) | — |
| 3.1 | Daemon scaffolding + transport | [archive/spec-daemon-scaffolding.md](archive/spec-daemon-scaffolding.md) | [Remote daemon](https://github.com/skrischer/rift/milestone/4) |
| 3.2 | Worktree file-tree sync | [spec-daemon-filetree.md](spec-daemon-filetree.md) | [File-tree sync](https://github.com/skrischer/rift/milestone/9) |
| 3.3 | Git status | [spec-daemon-git-status.md](spec-daemon-git-status.md) | [Git status](https://github.com/skrischer/rift/milestone/11) |
| 3.4 | LSP diagnostics | [spec-daemon-lsp.md](spec-daemon-lsp.md) | [LSP diagnostics](https://github.com/skrischer/rift/milestone/13) |
| 4 | Editor — GUI editing surface | [spec-editor.md](spec-editor.md) | [Editor](https://github.com/skrischer/rift/milestone/14) |
| 5 | LSP navigation (hover / go-to-definition / references) | [spec-lsp-navigation.md](spec-lsp-navigation.md) | [LSP navigation](https://github.com/skrischer/rift/milestone/15) |
| 6 | Terminal streaming (VTE-location spike first) | [spec-terminal-streaming.md](spec-terminal-streaming.md) | [Terminal streaming](https://github.com/skrischer/rift/milestone/16) |
| 7 | tmux key-table mirroring | [spec-tmux-keytable-mirroring.md](spec-tmux-keytable-mirroring.md) | [Key-table mirroring](https://github.com/skrischer/rift/milestone/17) |
| 8 | tmux status-line mirroring | [spec-tmux-statusline-mirroring.md](spec-tmux-statusline-mirroring.md) | [Status-line mirroring](https://github.com/skrischer/rift/milestone/18) |
| 9 | Window-state persistence | [spec-window-state-persistence.md](spec-window-state-persistence.md) | [Window-state persistence](https://github.com/skrischer/rift/milestone/19) |

A phase gets a Spec link once `/loopkit:plan` drafts it, and a Milestone link once
it is `READY`. The milestone (open/closed + issue progress) is where status lives.

## Tracks (tooling/DX, not product phases)

- **Dogfooding fixes** — living papercut backlog: [spec-dogfooding-fixes.md](spec-dogfooding-fixes.md), grouped by the [`papercut` label](https://github.com/skrischer/rift/labels/papercut); never completes.
- **Component gallery** — [spec-component-gallery.md](spec-component-gallery.md), [milestone 10](https://github.com/skrischer/rift/milestone/10); WebView follow-up (#127) open.
- **Dogfooding channels** — [spec-dogfooding-channels.md](spec-dogfooding-channels.md), [milestone 12](https://github.com/skrischer/rift/milestone/12).
- **Logging & diagnostics** — professional debug logging for the dev and stable channels: [spec-logging-diagnostics.md](spec-logging-diagnostics.md), [milestone 20](https://github.com/skrischer/rift/milestone/20); prior-art survey in [prior-art.md](prior-art.md) Category 10. Issues are immediately workable (parallel track, no queue edge).
- Completed meta tracks (workflow automation, planning automation, pane & window management, terminal interaction fixes) live in [archive/](archive/) and their closed milestones.

## Current focus

**Phase 3.2: Worktree file-tree sync** — the data layer everything downstream
consumes: git status (3.3) decorates its entries, LSP documents (3.4) follow its
watcher, the editor (4) renders it. Remaining issues #110, #111; the already-planned
3.3 / 3.4 / 4 / 5 / 6 / 7 / 8 / 9 queues unblock behind it. **All roadmap phases
and tracks are planned** — `/loopkit:plan` is waiting for new phases; the
Logging & diagnostics track (milestone 20) is immediately workable in parallel.

## North star

Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a
pane, watch the file explorer light up as it edits, diagnostics update in
real-time, review the clean diff, approve, move on. The Phase 3 data layers and
the editor surface deliver exactly that.
