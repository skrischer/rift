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
| 8 | tmux status-line mirroring | [archive/spec-tmux-statusline-mirroring.md](archive/spec-tmux-statusline-mirroring.md) | [Status-line mirroring](https://github.com/skrischer/rift/milestone/18) |
| 9 | Window-state persistence | [spec-window-state-persistence.md](spec-window-state-persistence.md) | [Window-state persistence](https://github.com/skrischer/rift/milestone/19) |
| 10 | IDE shell — dock + resizable panels | [spec-ide-shell.md](spec-ide-shell.md) | [Phase 100](https://github.com/skrischer/rift/milestone/24) |
| 11 | Explorer panel — decoration, reveal, keyboard nav | [spec-explorer-panel.md](spec-explorer-panel.md) | [Phase 110](https://github.com/skrischer/rift/milestone/25) |
| 12 | Source-control panel + visual diff | [spec-source-control.md](spec-source-control.md) | [Phase 120](https://github.com/skrischer/rift/milestone/26) |
| 13 | Problems panel — project-wide diagnostics | [spec-problems-panel.md](spec-problems-panel.md) | [Phase 130](https://github.com/skrischer/rift/milestone/27) |
| 14 | Status bar — branch, ahead/behind, diagnostic counts | [archive/spec-status-bar.md](archive/spec-status-bar.md) | [Phase 140](https://github.com/skrischer/rift/milestone/28) |
| 15 | Editor tabs — multiple open files | [spec-editor-tabs.md](spec-editor-tabs.md) | [Phase 150](https://github.com/skrischer/rift/milestone/29) |
| 16 | Command palette | [spec-command-palette.md](spec-command-palette.md) | [Phase 160](https://github.com/skrischer/rift/milestone/30) |
| 17 | Theme & settings | [spec-theme-settings.md](spec-theme-settings.md) | [Phase 170](https://github.com/skrischer/rift/milestone/31) |
| 18 | Window-tab pane-activity indicators — active-pane count + aggregate state (free / busy / attention) per window, derived agent-agnostically from per-pane OSC-133 shell integration + the terminal bell | [archive/spec-pane-activity-indicators.md](archive/spec-pane-activity-indicators.md) | [Phase 180](https://github.com/skrischer/rift/milestone/32) |
| 19 | tmux session switch — daemon session list + live updates, switcher UI (interim statusbar + palette; title-bar home lands with phase 21), re-attach; parallel sessions via second instance | [spec-session-switch.md](spec-session-switch.md) | [Phase 190](https://github.com/skrischer/rift/milestone/33) |
| 20 | Protocol & connection robustness — message-set version negotiation, stale-daemon restart, stream-death resync, reconnect loop, connect screen (startup state on every launch) | [spec-connection-robustness.md](spec-connection-robustness.md) | [Phase 200](https://github.com/skrischer/rift/milestone/34) |
| 21 | Cockpit chrome — custom title bar (connection/session group), activity rail, window-tab redesign, pane headers (sidebar removed) | [spec-cockpit-chrome.md](spec-cockpit-chrome.md) | [Phase 210](https://github.com/skrischer/rift/milestone/35) |
| 22 | Composite status line — window list + activity, branch ↑↓ + line totals, diagnostic counts, LSP health, Ln/Col, clock; supersedes the phase-8 content mirror | [spec-status-line.md](spec-status-line.md) | [Phase 220](https://github.com/skrischer/rift/milestone/36) |
| 23 | Editor chrome — breadcrumb + symbol, gutter severity dots, inline diagnostic card, hover card, results/outline panels, minimap, conflict dialog | [spec-editor-chrome.md](spec-editor-chrome.md) | [Phase 230](https://github.com/skrischer/rift/milestone/37) |
| 24 | Source-control write path — stage/unstage/commit, hunk staging, split diff + word-level emphasis | [spec-source-control-write.md](spec-source-control-write.md) | [Phase 240](https://github.com/skrischer/rift/milestone/38) |
| 25 | Explorer design parity — header actions, git letter lane, diagnostic dots + rollup, empty states | — | — |
| 26 | Settings shell + theme unification — full settings page, theme-driven terminal palette, hardcoded-hex cleanup | — | — |
| 27 | Explorer redesign — new Paper artboard + overhauled visual language (row anatomy, density, icon / context-menu / filter / file-op affordances); the visual contract phases 28–31 build on | — | — |
| 28 | Explorer file-type icons — SVG icon-theme asset embedding, file-type → icon mapping, folder / open-folder / chevron glyphs replacing today's text markers | — | — |
| 29 | Explorer context menu — right-click action framework over the tree; ships the client-capable actions (open, reveal, copy path / relative path, reveal-in-terminal, collapse-all) | — | — |
| 30 | Explorer file operations — create / rename / delete / move via a daemon write path, surfaced through the context menu + inline rename + drag & drop | — | — |
| 31 | Explorer search & filter — in-panel fuzzy narrowing, jump-to-file quick-open, multi-select, keyboard-first navigation | — | — |

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

- Phase 19 — `protocol` gains session-list messages (deliberate API change);
  `%sessions-changed`/`%client-session-changed` become typed events. The
  multi-attach-map idea was resolved at planning: parallel sessions v1 = one
  app instance per session (no architecture change); `%session-changed` is
  already consumed since #448 — the remaining gap is a layout refresh on
  external `switch-client` (see spec-session-switch.md).
- Phase 20 — `architecture.md` gained the "Connection robustness contract"
  section (ratified with the spec PR #470): strict message-set versioning,
  client-owned daemon version, no silent stream death, Connection screen as
  the startup state (papercuts #425/#426/#438/#441 shipped the small halves;
  the milestone owns negotiation + UX).
- Phase 24 — `protocol` gains git-write messages (stage/unstage/commit) — a
  deliberate, reviewed API extension; the daemon gains its first write
  capability beyond file save.
- Phase 26 — resolves the constitution tech-debt row on the hardcoded terminal
  palette (terminal ANSI colors follow the active theme).

Backing prior art: "v1.0 polish + robustness phases — prior-art index
(Phases 19–26)" in [prior-art.md](prior-art.md).

## Explorer overhaul (phases 27–31)

Seeded 2026-07-08 from idea sparring (research mode: websearch) after v1.0.0
shipped. The file explorer completed Phase 11 (decoration / reveal / keyboard
nav) and Phase 25 (design parity) as a **read-only** tree with **text-glyph**
markers — parity explicitly deferred real file-type icons (the product binary
does not embed SVG icon assets) and file operations (a daemon write capability),
and the tree still has no context menu, no search / filter, and no quick-open.
This block is the full overhaul into a first-class explorer.

Ordering is a DAG, not a strict chain: **27 (redesign) is the visual foundation**
the other four build on; **28 (icons)** and **31 (search)** are independent client
work against the new artboard; **29 (context menu)** is the interaction shell that
**30 (file operations)** surfaces its write actions through, so 29 precedes 30.
Phase 30 carries the only foundation impact.

Foundation impact (authored and ratified in each phase's `/loopkit:plan` spec
PR, never edited from here):

- Phase 27 — supersedes the "Cockpit — IDE" artboard's explorer panel as the
  explorer's visual contract; the new artboard is authored in this phase's spec
  PR (design-doc → issue → PR). No constitution / architecture change — a design
  artifact plus the client-side visual shell phases 28–31 render against.
- Phase 30 — `protocol` gains file-operation messages (create / rename / delete /
  move) — a deliberate, reviewed API extension (`docs/protocol.md`); the daemon
  gains its file-op write handlers, executing `std::fs` on the remote host (its
  second write capability after Phase-24 git-write and buffer save). The daemon
  owns the filesystem, so ops run **daemon-side**, not client-side SFTP — the
  same model as Zed's remote server. Ratified in Phase 30's spec PR.

Backing prior art: "Explorer overhaul — prior-art index (Phases 27–31)" in
[prior-art.md](prior-art.md).

## Tracks (tooling/DX, not product phases)

- **Dogfooding fixes** — living papercut backlog: [spec-dogfooding-fixes.md](spec-dogfooding-fixes.md), grouped by the [`papercut` label](https://github.com/skrischer/rift/labels/papercut); never completes.
- **Dogfooding channels** — [spec-dogfooding-channels.md](spec-dogfooding-channels.md), [milestone 12](https://github.com/skrischer/rift/milestone/12).
- **Logging & diagnostics** — professional debug logging for the dev and stable channels: [spec-logging-diagnostics.md](spec-logging-diagnostics.md), [milestone 20](https://github.com/skrischer/rift/milestone/20); prior-art survey in [prior-art.md](prior-art.md) Category 10. Issues are immediately workable (parallel track, no queue edge).
- **Daemon re-deploy** — a changed same-version daemon binary takes effect on the next relaunch (atomic replace + pidfile restart of the shared daemon): [spec-daemon-redeploy.md](spec-daemon-redeploy.md), [milestone 23](https://github.com/skrischer/rift/milestone/23). Graduated from the reverted papercut #268, after live QA exposed the `ETXTBSY` / reattach-stale seams.
- **Visual UI harness** — give the coding agent eyes on the real rift / rift-gallery UI plus deterministic E2E (show UI bugs, check design parity against the Paper contract). Two ordered phases, specs/milestones created at planning: **(1) gpui Linux/WSLg headless renderer** — implement the one missing `PlatformHeadlessRenderer` impl for off-macOS (offscreen wgpu texture + readback), unblocking `capture_screenshot` on rift's platforms; **(2) visual/E2E harness** — `capture_screenshot` + `TestAppContext` driving over a named snapshot registry (rift views + gallery components), Paper-MCP diff, optional CI pixel baseline. Foundation impact (Phase 1, ratified in its `/loopkit:plan` spec, never edited from here): an **additive `[patch]` fork** of `gpui` on the frozen `4bee412` base — not a commit bump, so no API churn — redirecting every gpui consumer (rift, gpui-component, `termy_terminal_ui`) to the fork, with a **mandatory single-`gpui`-invariant trial** before landing; pin-mechanics precedent in [archive/spec-gpui-rev-bump.md](archive/spec-gpui-rev-bump.md). Backing prior art: "Visual UI harness — prior-art index" in [prior-art.md](prior-art.md).
- Completed meta tracks (workflow automation, planning automation, pane & window management, terminal interaction fixes, component gallery, gpui rev bump investigation) live in [archive/](archive/) and their closed milestones. The gpui rev bump investigation (milestone 22) concluded **NO-GO for now**: the candidate rev breaks the single-`gpui` invariant via the `termy_terminal_ui` fork's bare `gpui` pin — see [archive/spec-gpui-rev-bump.md](archive/spec-gpui-rev-bump.md) decision log for the full findings and the ordered prerequisite steps.

## North star

Scenario 1 from [vision.md](vision.md): connect to a VPS, run Claude Code in a
pane, watch the file explorer light up as it edits, diagnostics update in
real-time, review the clean diff, approve, move on. The Phase 3 data layers and
the editor surface deliver the edit + inline-diagnostics half; the v1.0.0 cockpit
phases (10–17) deliver the remaining half — the visual diff review and the
diagnostics / git surfaces — which is what v1.0.0 ships.
