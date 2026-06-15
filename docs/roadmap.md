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
| 2d | Tab bar + statusbar enrichment | [archive/spec-phase2d-tabbar.md](archive/spec-phase2d-tabbar.md) | [Phase 2d](https://github.com/skrischer/rift/milestone/1) |
| 2e | gpui-component UI foundation | [archive/spec-gpui-component-adoption.md](archive/spec-gpui-component-adoption.md) | — |
| 3.1 | Daemon scaffolding + transport | [archive/spec-daemon-scaffolding.md](archive/spec-daemon-scaffolding.md) | [Remote daemon](https://github.com/skrischer/rift/milestone/4) |
| 3.2 | Worktree file-tree sync | [archive/spec-daemon-filetree.md](archive/spec-daemon-filetree.md) | [File-tree sync](https://github.com/skrischer/rift/milestone/9) |
| 3.3 | Git status | [archive/spec-daemon-git-status.md](archive/spec-daemon-git-status.md) | [Git status](https://github.com/skrischer/rift/milestone/11) |
| 3.4 | LSP diagnostics | [archive/spec-daemon-lsp.md](archive/spec-daemon-lsp.md) | [LSP diagnostics](https://github.com/skrischer/rift/milestone/13) |
| 3.5 | Daemon project root (watch the project, not `$HOME`) — data-layer fix, independent of 3.4 | [archive/spec-daemon-project-root.md](archive/spec-daemon-project-root.md) | [Daemon project root](https://github.com/skrischer/rift/milestone/21) |
| 4 | Editor — GUI editing surface | [spec-editor.md](spec-editor.md) | [Editor](https://github.com/skrischer/rift/milestone/14) |
| 5 | LSP navigation (hover / go-to-definition / references) | [spec-lsp-navigation.md](spec-lsp-navigation.md) | [LSP navigation](https://github.com/skrischer/rift/milestone/15) |
| 6 | Terminal streaming (VTE-location spike first) | [archive/spec-terminal-streaming.md](archive/spec-terminal-streaming.md) | [Terminal streaming](https://github.com/skrischer/rift/milestone/16) |
| 7 | tmux key-table mirroring | [spec-tmux-keytable-mirroring.md](spec-tmux-keytable-mirroring.md) | [Key-table mirroring](https://github.com/skrischer/rift/milestone/17) |
| 8 | tmux status-line mirroring | [spec-tmux-statusline-mirroring.md](spec-tmux-statusline-mirroring.md) | [Status-line mirroring](https://github.com/skrischer/rift/milestone/18) |
| 9 | Window-state persistence | [spec-window-state-persistence.md](spec-window-state-persistence.md) | [Window-state persistence](https://github.com/skrischer/rift/milestone/19) |

A phase gets a Spec link once `/loopkit:plan` drafts it, and a Milestone link once
it is `READY`. The milestone (open/closed + issue progress) is where status lives.

## Tracks (tooling/DX, not product phases)

- **Dogfooding fixes** — living papercut backlog: [spec-dogfooding-fixes.md](spec-dogfooding-fixes.md), grouped by the [`papercut` label](https://github.com/skrischer/rift/labels/papercut); never completes.
- **Dogfooding channels** — [spec-dogfooding-channels.md](spec-dogfooding-channels.md), [milestone 12](https://github.com/skrischer/rift/milestone/12).
- **Logging & diagnostics** — professional debug logging for the dev and stable channels: [spec-logging-diagnostics.md](spec-logging-diagnostics.md), [milestone 20](https://github.com/skrischer/rift/milestone/20); prior-art survey in [prior-art.md](prior-art.md) Category 10. Issues are immediately workable (parallel track, no queue edge).
- **gpui rev bump investigation** — [spec-gpui-rev-bump.md](spec-gpui-rev-bump.md), [milestone 22](https://github.com/skrischer/rift/milestone/22); spike spun out of #127 (the live WebView needs a newer gpui than the pinned `4bee412`). Analyse + one trial bump + document go/no-go; lands no production bump.
- **Daemon re-deploy** — a changed same-version daemon binary takes effect on the next relaunch (atomic replace + pidfile restart of the shared daemon): [spec-daemon-redeploy.md](spec-daemon-redeploy.md), [milestone 23](https://github.com/skrischer/rift/milestone/23). Graduated from the reverted papercut #268, after live QA exposed the `ETXTBSY` / reattach-stale seams.
- Completed meta tracks (workflow automation, planning automation, pane & window management, terminal interaction fixes, component gallery) live in [archive/](archive/) and their closed milestones.

## North star

Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a
pane, watch the file explorer light up as it edits, diagnostics update in
real-time, review the clean diff, approve, move on. The Phase 3 data layers and
the editor surface deliver exactly that.
