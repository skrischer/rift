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
| 2e | gpui-component UI foundation | COMPLETED 2026-06-05 | [archive/spec-gpui-component-adoption.md](archive/spec-gpui-component-adoption.md) |
| 3 | Remote daemon | READY (scaffolding) | [spec-daemon-scaffolding.md](spec-daemon-scaffolding.md) |

## Current focus

**Phase 2d: Tab bar + statusbar enrichment**

Tab bar for tmux window switching, CWD from subscriptions instead of snapshot polling, git branch display, pane command name, connection status indicator. This completes the tmux integration before moving to the daemon architecture.

Spec: [spec-phase2d-tabbar.md](spec-phase2d-tabbar.md)

Phase 2e (gpui-component UI foundation) is COMPLETE — it built the substrate for the 2d displays: `Root`/theme wiring (#26), an app-wide Catppuccin theme (#33), the window tab bar (#27), and the statusbar container rebuilt on gpui-component primitives with themed slots (#28). The 2d data fields now land on that statusbar. Spec: [archive/spec-gpui-component-adoption.md](archive/spec-gpui-component-adoption.md)

**Completed track: terminal interaction fixes (dogfooding)**

A batch of pre-SDD terminal/tmux interaction defects surfaced while dogfooding: `capture-pane`-backed scrollback (#39), `Ctrl+=`/`Ctrl+-` font zoom (#40), and drag-to-resize pane borders (#41) — GUI affordances replacing tmux's rendered interactive layer. COMPLETED 2026-06-07; the fourth outcome (pane zoom, #42) was dropped before implementation and the work moved to the pane/window-management track below. Spec: [archive/spec-terminal-interaction-fixes.md](archive/spec-terminal-interaction-fixes.md). It also added the "tmux control-mode interaction model" decision to [architecture.md](architecture.md).

**Completed track: pane & window management (dogfooding)**

Mouse-driven tmux pane/window lifecycle in the GPU UI: closing a pane via `exit` no longer quits the app (#68), the tab bar gains `+`/`x` to create and close windows (#69), a left sidebar lists and manages the active window's panes — focus, close, split (#70), and double-clicking a tab renames the window (#71). COMPLETED 2026-06-08. Spec: [archive/spec-pane-window-management.md](archive/spec-pane-window-management.md). Milestone: [Pane & window management](https://github.com/skrischer/rift/milestone/5).

**Planned: tmux key-table mirroring** — make configured tmux keybindings work in a rift pane (today `send-keys -H` bypasses them). Larger effort, split into its own DRAFT spec: [spec-tmux-keytable-mirroring.md](spec-tmux-keytable-mirroring.md). Not scheduled until the interaction fixes land.

**Planned: tmux status-line mirroring** — under `tmux -CC` the user's `status-left/right/style` config is queryable but never rendered, so it is currently ignored. An opt-in mode would mirror it in the native statusbar via a tmux format-string interpreter. Own DRAFT spec: [spec-tmux-statusline-mirroring.md](spec-tmux-statusline-mirroring.md). Sibling to key-table mirroring (both surface a hidden tmux config primitive); the Phase 2d native statusbar stays the default.

**Meta track: implementation workflow automation** — COMPLETED 2026-06-08. Automated the issue → merged cycle that emerged across the Phase 2d work: a `just pr-merge` recipe (remote-only merge + cleanup), a CI `app-check` job that finally compiles `rift-app`, board status transitions baked into the worktree recipe (`In Progress`) and the skill close-out (`Done`), an interactive tmux reviewer pane, and a `/implement` skill tying it together. Tooling/DX, not a product phase. Spec: [archive/spec-workflow-automation.md](archive/spec-workflow-automation.md). Milestone: [Workflow automation](https://github.com/skrischer/rift/milestone/6).

**Meta track: planning workflow automation** — COMPLETED 2026-06-09. The planning-side sibling to the above, filling the slot it reserved: a `just plan-issues` recipe (milestone + per-step issues from a markdown step-file, with a `PLAN_ISSUES_PREVIEW=1` dry-run) and a `/plan` skill driving readiness → merged `READY` spec → milestone + issues → roadmap, with the review gate on the in-session Agent tool instead of the tmux pane. The spec was itself dogfooded through the cycle it specifies, and the `/plan` skill was then verified end-to-end on a throwaway trial. A sibling `chore(pr-merge)` (#97) made the merge recipe re-poll the transient `UNKNOWN` mergeability state surfaced by the dogfood. Tooling/DX, not a product phase. Spec: [archive/spec-planning-automation.md](archive/spec-planning-automation.md). Milestone: [Planning automation](https://github.com/skrischer/rift/milestone/7) (#93 recipe → #96, #94 skill → #100).

## What comes after Phase 2d

**Phase 3: Remote daemon** — the major architectural shift. Splits the monolithic app into a GPUI frontend + a remote daemon connected over a dedicated `russh` channel (no WebSocket — `russh` already multiplexes channels). The daemon handles file watching (inotify), git status, and language servers (LSP) on the remote host. The frontend becomes a thin rendering client.

The foundational decisions are resolved (see [spec-daemon-scaffolding.md](spec-daemon-scaffolding.md)): daemon form is Lapce-flat dispatch (not Zed `HeadlessProject`), transport lifts Zed's connection-reuse + auto-deploy, the client↔daemon channel is a dedicated `russh` channel. File-sync (Zed worktree `Snapshot` + incremental updates) and LSP lifecycle (daemon-side, lazy per `DocumentSelector`, `async-lsp`) are pre-decided for their own sub-specs. The one genuinely open item is the VTE parsing location (client-side vs. daemon-side), deferred to a spike before the terminal-streaming sub-spec.

Phase 3 is the biggest architectural change since the project began and needs multiple sub-specs (daemon scaffolding, file tree, git status, LSP integration, terminal streaming). The first — daemon scaffolding + transport — is `READY`, with milestone [Phase 3 — Remote daemon](https://github.com/skrischer/rift/milestone/4) and issues #57–#62.

See [prior-art.md](prior-art.md) for reference implementations (Zed `remote_server`, Lapce proxy, Arbor, `async-lsp`) and candidate dependencies to draw from when writing these specs.

## North star

The goal is Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a pane, see the file explorer highlight every file it touches, diagnostics update in real-time, git panel shows clean diffs. Review visually, approve, move on.

Phase 2d gets us the tab bar and enriched statusbar. Phase 3 gets us the file explorer and diagnostics. Together they deliver the core north star scenario.
