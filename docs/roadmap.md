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
| 10 | IDE shell — dock + resizable panels | [spec-ide-shell.md](spec-ide-shell.md) | [Phase 100](https://github.com/skrischer/rift/milestone/24) |
| 11 | Explorer panel — decoration, reveal, keyboard nav | [spec-explorer-panel.md](spec-explorer-panel.md) | [Phase 110](https://github.com/skrischer/rift/milestone/25) |
| 12 | Source-control panel + visual diff | [spec-source-control.md](spec-source-control.md) | [Phase 120](https://github.com/skrischer/rift/milestone/26) |
| 13 | Problems panel — project-wide diagnostics | [spec-problems-panel.md](spec-problems-panel.md) | [Phase 130](https://github.com/skrischer/rift/milestone/27) |
| 14 | Status bar — branch, ahead/behind, diagnostic counts | [spec-status-bar.md](spec-status-bar.md) | [Phase 140](https://github.com/skrischer/rift/milestone/28) |
| 15 | Editor tabs — multiple open files | [spec-editor-tabs.md](spec-editor-tabs.md) | [Phase 150](https://github.com/skrischer/rift/milestone/29) |
| 16 | Command palette | — | — |
| 17 | Theme & settings | — | — |

A phase gets a Spec link once `/loopkit:plan` drafts it, and a Milestone link once
it is `READY`. The milestone (open/closed + issue progress) is where status lives.

## v1.0.0 — Agent cockpit (phases 10–17)

The v1.0.0 milestone group. Today rift's process + data layers are complete — the
daemon streams worktree, git status, repo state, LSP diagnostics, and LSP
navigation — but most of those reactive signals are folded into the client model
without ever being **visualised**: git status, repo/branch, and project-wide
diagnostics have no UI surface; only inline editor diagnostics and LSP navigation
are rendered. Phases 10–17 close that gap and replace the fixed three-column flex
layout with a real dockable IDE shell built on `gpui-component` (the library is
already vendored — the gallery exercises Dock/Resizable/Tab, the product does not).
This is the "compete with Zed" cut along rift's own axis ([vision.md](vision.md)):
the reactive **agent cockpit**, not generic-editor feature parity.

- **In v1.0.0:** surface the reactive signals (11–14); ship the IDE shell (10, 15–17).
- **Out (post-v1.0.0):** multi-worktree UI (Scenario 2), project-wide search,
  outline/symbols, LSP completion / code-actions / format / rename, fuzzy
  quick-open — generic-editor depth that `vision.md` deliberately scopes out of v1.

Sequence: phase 10 (dock shell) is the foundation panels 12–13 dock into. Phase
11's git/diagnostic tree decoration and phase 14's status bar read the existing
client model directly (no dock dependency) and are the low-risk quick wins. Phase
12 is the only numbered phase needing a new daemon capability (file diffs).
Explorer **file operations** (create/rename/delete/move) were split out of Phase
11 at planning into a separate daemon-write phase (a write capability needing new
protocol variants, unlike Phase 11's read-only decoration/navigation); it is not
yet sequenced.

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
the editor surface deliver the edit + inline-diagnostics half; the v1.0.0 cockpit
phases (10–17) deliver the remaining half — the visual diff review and the
diagnostics / git surfaces — which is what v1.0.0 ships.
