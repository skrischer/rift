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
| 16 | Command palette | [spec-command-palette.md](spec-command-palette.md) | [Phase 160](https://github.com/skrischer/rift/milestone/30) |
| 17 | Theme & settings | [spec-theme-settings.md](spec-theme-settings.md) | [Phase 170](https://github.com/skrischer/rift/milestone/31) |
| 18 | Window-tab pane-activity indicators — active-pane count + aggregate state (free / busy / attention) per window, derived agent-agnostically from per-pane OSC-133 shell integration + the terminal bell | [archive/spec-pane-activity-indicators.md](archive/spec-pane-activity-indicators.md) | [Phase 180](https://github.com/skrischer/rift/milestone/32) |
| 19 | tmux session switch — daemon session list + live updates, title-bar switcher, re-attach / parallel attach | — | — |
| 20 | Protocol & connection robustness — message-set version negotiation, stale-daemon restart, stream-death resync, reconnect loop, connect screen | — | — |
| 21 | Cockpit chrome — custom title bar (connection/session group), activity rail, window-tab redesign, pane headers | — | — |
| 22 | Composite status line — window list + activity, branch ± counts, diagnostic counts, LSP health, Ln/Col, clock | — | — |
| 23 | Editor chrome — breadcrumb + symbol, gutter severity dots, inline diagnostic card, hover card, references/outline panels, conflict dialog | — | — |
| 24 | Source-control write path — stage/unstage/commit, hunk staging, split diff + word-level emphasis | — | — |
| 25 | Explorer design parity — header actions, git letter lane, diagnostic dots + rollup, empty states | — | — |
| 26 | Settings shell + theme unification — full settings page, theme-driven terminal palette, hardcoded-hex cleanup | — | — |

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

## v1.0 polish cut (phases 19–26)

Seeded 2026-07-05 from idea sparring backed by a verified defect/gap analysis
(73 confirmed defects, 62 design gaps vs the Paper design; live behavioral QA on
the dev channel). The Paper design file `rift` (7 artboards: Styleguide,
Cockpit — IDE, Connection — Startup, Parallel Agents — Worktrees, Editor — LSP
Navigation, Git — Diff Review, Settings) is the visual contract these phases
close on. Ordering: 19–20 are the feature/correctness backbone — the live-QA
smoking gun (an app/daemon protocol skew silently killing the whole reactive
stream, no recovery, no user-visible error) is the root cause behind "reactive
signals dead" reports — and 21–26 are per-surface design parity.

Foundation impact (authored and ratified in each phase's `/loopkit:plan` spec
PR, never edited from here):

- Phase 19 — `architecture.md`: the connection model grows a multi-session
  client dimension (one control child per (client, session)); `protocol` gains
  session-list messages (deliberate API change). `%sessions-changed` handling
  replaces today's discarded `Event::SessionChanged`.
- Phase 20 — `architecture.md` "Connection lifecycle": a reconnect loop
  replaces quit-on-disconnect; Hello/Welcome carries a message-set version and
  the client restarts a stale running daemon via the pidfile mechanism
  (papercuts #425/#426/#438/#441 shipped the small halves; this phase owns
  negotiation + UX, including the Connection — Startup screen).
- Phase 24 — `protocol` gains git-write messages (stage/unstage/commit) — a
  deliberate, reviewed API extension; the daemon gains its first write
  capability beyond file save.
- Phase 26 — resolves the constitution tech-debt row on the hardcoded terminal
  palette (terminal ANSI colors follow the active theme).

Backing prior art: "v1.0 polish + robustness phases — prior-art index
(Phases 19–26)" in [prior-art.md](prior-art.md).

## Tracks (tooling/DX, not product phases)

- **Dogfooding fixes** — living papercut backlog: [spec-dogfooding-fixes.md](spec-dogfooding-fixes.md), grouped by the [`papercut` label](https://github.com/skrischer/rift/labels/papercut); never completes.
- **Dogfooding channels** — [spec-dogfooding-channels.md](spec-dogfooding-channels.md), [milestone 12](https://github.com/skrischer/rift/milestone/12).
- **Logging & diagnostics** — professional debug logging for the dev and stable channels: [spec-logging-diagnostics.md](spec-logging-diagnostics.md), [milestone 20](https://github.com/skrischer/rift/milestone/20); prior-art survey in [prior-art.md](prior-art.md) Category 10. Issues are immediately workable (parallel track, no queue edge).
- **Daemon re-deploy** — a changed same-version daemon binary takes effect on the next relaunch (atomic replace + pidfile restart of the shared daemon): [spec-daemon-redeploy.md](spec-daemon-redeploy.md), [milestone 23](https://github.com/skrischer/rift/milestone/23). Graduated from the reverted papercut #268, after live QA exposed the `ETXTBSY` / reattach-stale seams.
- Completed meta tracks (workflow automation, planning automation, pane & window management, terminal interaction fixes, component gallery, gpui rev bump investigation) live in [archive/](archive/) and their closed milestones. The gpui rev bump investigation (milestone 22) concluded **NO-GO for now**: the candidate rev breaks the single-`gpui` invariant via the `termy_terminal_ui` fork's bare `gpui` pin — see [archive/spec-gpui-rev-bump.md](archive/spec-gpui-rev-bump.md) decision log for the full findings and the ordered prerequisite steps.

## North star

Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a
pane, watch the file explorer light up as it edits, diagnostics update in
real-time, review the clean diff, approve, move on. The Phase 3 data layers and
the editor surface deliver the edit + inline-diagnostics half; the v1.0.0 cockpit
phases (10–17) deliver the remaining half — the visual diff review and the
diagnostics / git surfaces — which is what v1.0.0 ships.
