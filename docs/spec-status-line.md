# Spec: composite status line

> Status: READY
> Created: 2026-07-05
> Completed: —

One 28px status line per the Paper design — window list with live activity on
the left, IDE state (branch, line totals, diagnostic counts, LSP health,
cursor, clock) on the right — replacing today's THREE competing bars (the
24px workspace strip, SessionView's own 28px statusbar, and the env-gated
mirrored tmux bar).

## Outcome

- [ ] Exactly one status line exists (28px, all mono 12px, status-line bg
      token): left `>_ rift` wordmark (primary) · tmux window list
      (`1:agent*` — active window gets a surface chip, activity dots on busy/
      attention windows, click selects the window) · PREFIX/key-table
      indicator while active; right: branch (primary) with ahead/behind
      `↑n ↓m` (from the existing `RepoState.ahead_behind`) · `+N −M`
      working-tree line totals (success/danger) · `●e ⚠w` workspace
      diagnostic counts · language-server health dot + name ·
      `Ln L, Col C` (editor cursor) · clock (HH:MM, minute tick).
- [ ] The window list mirrors tmux live (§8.4): create/close/rename/select/
      activity reflect without refresh, consistent with the tab bar's states.
- [ ] `+N −M` and the LSP health dot are fed by real streams (new minimal
      daemon capabilities), not placeholders.
- [ ] SessionView's internal statusbar and the `RIFT_STATUSLINE_MIRROR` mode
      are removed — their surviving information (session name, user@host,
      cwd, PREFIX indicator, grid size on resize) relocates into the one bar
      or the title bar (phase 21) per the design.
- [ ] Zero hardcoded hex in touched rendering code.

## Scope

### In scope

- `protocol`/`daemon` (deliberate, minimal API changes; PROTOCOL_VERSION
  bumped per the fingerprint policy):
  - `RepoState` gains working-tree line totals (`lines_added`,
    `lines_removed`), computed with the existing `gix` dependency during the
    existing debounced git recompute (worktree vs HEAD, matching the
    RequestDiff semantics).
  - A `DaemonMessage::LspStatus { server, state }` push (state: starting |
    running | crashed | stopped), emitted by the existing LSP registry on
    lifecycle transitions and replayed to new connections behind Welcome.
- `app`/`terminal`: the composite bar as the single workspace-level status
  line: left segments read SessionView's existing window/activity state
  (window list entries clickable → select-window through the existing
  command channel; PREFIX indicator relocates as a transient segment); right
  segments read the existing repo/diagnostics models plus the two new
  streams, the editor cursor, and a minute clock.
- `terminal`: remove SessionView's internal statusbar; `app`: remove the
  mirrored-bar mode (`RIFT_STATUSLINE_MIRROR`, status_bar.rs:149) and its
  render path — superseded by this design (see Prior decisions).
- Full removal of the now-consumerless phase-8 mirror machinery (a deliberate
  protocol REMOVAL, version-bumped per the fingerprint policy): the
  `QueryStatusLine`/`StatusLineReply` protocol pair, the daemon
  `StatusLineQuery` bundle (terminal.rs:282-309, per-attach issue + interval
  re-fetch), the workspace `status_line_rx` fold (workspace.rs:481-502), and
  the `crates/terminal/src/statusline.rs` style-run parser. None of these has
  a consumer besides the mirror (the keytable path has its OWN query/parser —
  the two are parallel structures, not shared).
- Relocations per design: session name + user@host live in the phase-21
  title bar (already specced); cwd moves into the phase-21 pane headers; the
  grid-size readout (today a persistent segment, session_view.rs:1130) is
  removed and replaced by a brief resize-feedback overlay near the terminal;
  the "refresh keys" escape hatch (session_view.rs:1361-1373, mandated by the
  keytable-mirroring spec) relocates to a command-palette entry
  ("Refresh tmux key tables") driving the same `key_table_request_tx`.
- Docs disposition: `spec-tmux-statusline-mirroring.md` and
  `spec-status-bar.md` are set COMPLETED with a superseded-by note and moved
  to `docs/archive/` (links repointed) as part of this phase.

### Out of scope

- Editor chrome segments beyond Ln/Col (phase 23 owns breadcrumb etc.).
- Per-worktree/multi-instance aggregation (Parallel-Agents artboard status
  variants — post-v1 with multi-worktree UI).
- Rendering tmux `status-left`/`status-right` CONTENT (the phase-8 mirror):
  superseded, see Prior decisions.
- Any change to activity signal derivation (#428/#491 own it).

## Constraints

- Theme tokens only; bar bg = the darkest ground token (ref #11111b), text
  mono 12px (JetBrains Mono), segment padding 12-16px; active window chip =
  surface token; counts colored success/danger/warning per §0.
- The clock ticks once per minute (no per-second wakeups); the line-totals
  computation rides the EXISTING git recompute tick — no new timers, no
  per-keystroke work.
- Diff totals must not scan ignored/untracked binary noise: totals cover
  tracked modified/deleted/renamed files plus untracked text files the git
  status already reports, mirroring `git diff --numstat` + untracked
  additions semantics; cap per-file work and skip files git deems binary.
- Window-list interactions go through the existing tmux command channel
  (select-window), never a parallel path.
- Constitution: channels for state, no `.unwrap()` in libs, crate
  boundaries, protocol changes documented + tested valid/malformed.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| One composite bar replaces all three existing bars | The design shows exactly one status line; three coexisting bars are the confirmed wave-1 gap (24px strip with 2 segments, SessionView bar, env-gated mirror) | 2026-07-05 |
| The phase-8 tmux status-line CONTENT mirror is superseded and FULLY removed (protocol pair, daemon query bundle, app fold, style-run parser) | The composite bar's window list + rift-native segments cover the design; the mirror was env-gated, default-off, and mutually exclusive with native segments. NOTE: this consciously overturns the gate-ratified phase-8 decision ("native default + opt-in mirror", 2026-06-05/2026-06-12) — the one-bar design contract is the new precedent; presented as such at this spec's acceptance gate. The keytable path is unaffected (it owns parallel machinery) | 2026-07-05 |
| Ahead/behind stays a bar segment (`↑n ↓m` beside the branch) | The design's Git artboard shows `develop ↑2`; the data already streams on `RepoState.ahead_behind` | 2026-07-05 |
| LSP health identity = stable server NAME (e.g. "rust-analyzer"), not per-spawn ServerId; states starting/running/crashed (no "stopped" — servers are never deliberately stopped while attached); detection rides the registry observe cycle | Restarts mint fresh ServerIds (that keying stays for diagnostics); the bar wants "is my language server ok", which is name-scoped. Crash detection is observe-granular today (prune_dead) — acceptable latency, no crates/lsp API change needed | 2026-07-05 |
| Line-totals semantics = `git diff HEAD --numstat` + untracked text additions; renames diff against the rewrite SOURCE blob (explorer/git.rs Change::Rewrite carries it) | Pins the acceptance semantics; a pure rename contributes 0/0 instead of full-file adds | 2026-07-05 |
| `+N −M` = daemon-computed worktree-vs-HEAD line totals on RepoState | The design shows line totals, not file counts; gix is already the daemon's git engine and the recompute tick already walks the status — totals are an incremental extension, replayed to new clients like all state | 2026-07-05 |
| LSP health = registry lifecycle push, not diagnostics-flow inference | The registry owns start/crash/restart (#273/#497 context); inferring health from diagnostics traffic is wrong on idle-but-healthy servers | 2026-07-05 |
| Grid-size readout becomes a transient overlay near the terminal | It is resize feedback (like an OSD), not persistent status; the design's bar has no such segment | 2026-07-05 |
| Clock is client-local time | It mirrors the design's tmux-style clock; remote time would add a protocol field for no dogfooding value | 2026-07-05 |

## Prior art

- `docs/prior-art.md` → Phases 19–26 index, Phase 22 row: `zed`
  `crates/status_bar` (segment slots), `zellij` status bar (window list +
  discoverability), rift's own `spec-tmux-statusline-mirroring.md`
  (superseded content mirror; its option-query machinery stays for keytable).

## Human prerequisites

None.

## Tracking

- Milestone: created after this spec merges (phase 22) — with
  `Depends on milestone: Phase 210` (the relocation homes: title bar, pane
  headers).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Protocol tests for the RepoState fields + LspStatus (valid + malformed)
- [ ] Behavioral: agent edits files → `+N −M` updates on the recompute
      cadence and matches `git diff HEAD --numstat` totals plus untracked
      text additions (spot-check incl. a rename → 0/0); reverting all
      changes hides the segment
- [ ] Ahead/behind (`↑n ↓m`) tracks push/pull state live
- [ ] The palette entry "Refresh tmux key tables" performs the manual
      key-table refresh (escape hatch preserved)
- [ ] Behavioral: killing rust-analyzer flips the health dot (crashed) and a
      restart flips it back — no app restart
- [ ] Behavioral: window create/close/rename/select/activity reflect in the
      window list live; clicking an entry selects the window; the active
      chip follows external `select-window` too
- [ ] Ln/Col tracks the editor cursor; clock ticks at most once per minute
- [ ] Exactly one status bar renders; `RIFT_STATUSLINE_MIRROR` is gone
      (env var has no effect); SessionView bar gone
- [ ] Visual match vs the Cockpit — IDE artboard at the QA gate

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gix diffstat on large changesets slows the recompute tick | Compute per changed file with a size cap, skip binaries, and reuse the debounced tick — never per-keystroke; measure on the rift repo itself at the QA gate |
| Removing the SessionView bar orphans a consumer of its state (session name, cwd) | Relocations are explicit in scope (title bar / pane headers, both phase-21 — this milestone depends on phase 21 landing those homes first) |
| Line totals diverge from `git diff --numstat` on renames/untracked | Acceptance pins the semantics (numstat + untracked additions); parser/diff tests cover a rename and an untracked file |

## Decision log

- 2026-07-05: Spec drafted from the wave-1 gap analysis (three-bar
  fragmentation CONFIRMED; missing segments enumerated) and the design
  distillation §1/§8.4.
- 2026-07-05: Fresh-context review (PR #517): three blocking findings baked
  in — (B1) full disposition of the phase-8 machinery (protocol pair, daemon
  bundle, app fold, parser all removed; "stays for keytable" claim was wrong
  — parallel structures), (B2) ahead/behind restored as a segment (it is in
  the design's Git artboard and already streams), (B3) the "refresh keys"
  escape hatch relocates to a palette entry. Non-blocking adoptions: LSP
  health identity/granularity pinned, numstat-vs-HEAD + rename semantics,
  grid-size wording, archive disposition for the two superseded specs,
  milestone dependency on Phase 210, and the explicit note that the mirror
  removal overturns the phase-8 ratified decision.
