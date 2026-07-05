# Spec: tmux session switch

> Status: READY
> Created: 2026-07-05
> Completed: —

See all tmux sessions on the SSH host, switch the cockpit between them, and keep
the session indicator truthful — the daemon gains a session-list capability and
live session events; the client gains a switcher UI (interim placement: statusbar
label + command palette; the custom title bar relocates it in phase 21).

## Outcome

- [ ] The client can request the host's tmux session list and receives name,
      window count, and attached state per session; the list refreshes
      automatically when sessions are created, killed, or renamed (no manual
      refresh anywhere).
- [ ] The user can switch the cockpit to any listed session (and create a new
      named session) from the switcher UI and the command palette; the terminal
      view resets cleanly to the new session's layout.
- [ ] The session indicator always shows the ACTUAL attached session — after
      attach-or-create and rename it already does (since #429/#448); after an
      external `switch-client` the indicator AND the terminal content refresh
      immediately instead of staying stale until the next structural event.
- [ ] All of it is agent-agnostic plumbing: no new signal beyond the tmux
      control-mode stream.

## Scope

### In scope

- `tmux-core`: parse `%sessions-changed` and `%client-session-changed` into
  typed events (`%session-renamed` and `%window-renamed` landed via #429).
- `protocol` (deliberate API change, minimal): a `QuerySessionList` →
  `SessionListReply` request/response pair modeled on the existing
  `QueryKeyTable` → `KeyTableReply`, plus unprompted `SessionListReply` pushes.
  No new attached-session signal: since #429/#448 the `session` string on
  `LayoutSnapshot`/`LayoutUpdate` already carries the ACTUAL session (adopted
  from `%session-changed` at crates/daemon/src/terminal.rs:463-469 and
  `%session-renamed` at :470-479).
- `daemon`: serve the query via `list-sessions` with a tab-separated format,
  session name as the LAST field (mirrors the `LAYOUT_QUERY` convention,
  terminal.rs:52): `#{session_id}\t#{session_windows}\t#{session_attached}\t#{session_name}`,
  under the existing correlated-command mechanism; re-issue it coalesced (like
  the layout re-query) on `%sessions-changed` / `%session-renamed` /
  `%client-session-changed` and push the result. Close the one remaining
  truthful-indicator gap: the `SessionChanged` arm updates `self.session` but
  never triggers `request_layout()` — add it (mirroring the `SessionRenamed`
  arm), so an external `switch-client` refreshes both the indicator and the
  terminal content instead of staying stale until the next structural event.
- `terminal`/`app`: client session model (list + actual attached session);
  switcher popover anchored to the statusbar session label; command-palette
  commands ("Switch Session…", "New Session…"); switching sends the existing
  `Attach { session }` (a second Attach on the same connection already performs
  a clean child swap + fresh `LayoutSnapshot` — zero daemon changes for the
  switch itself).

### Out of scope

- Parallel multi-session rendering inside ONE window (protocol multi-attach
  map). Parallelism v1 = a second app instance (works today, one control child
  per client). Revisit post-v1 if dogfooding demands it.
- Killing sessions from the picker (destructive; not in the design's v1 UI).
- Custom title bar placement of the switcher (phase 21 relocates the indicator
  group; this phase ships the interim statusbar + palette entry points).
- Per-session project roots: worktree/git/LSP state is keyed to the daemon's
  `--root`, not the tmux session — switching to a session in another project
  does NOT switch the explorer/diagnostics root (documented limitation; a
  future phase may bind roots to sessions).

## Constraints

- Protocol additions are deliberate API changes (CLAUDE.md): keep the two new
  message shapes minimal, document them in `docs/protocol.md`, and test them
  with valid + malformed input.
- Control-mode contract (docs/architecture.md): list via commands under
  `%begin/%end` guards; never render tmux chooser UIs (`choose-tree` is
  invisible to control clients).
- Agent-agnostic: session data derives from the control stream only.
- No `.unwrap()` in library code; crate boundaries via `lib.rs`.
- UI contract (distilled from the Paper design, Cockpit — IDE artboard) — all
  colors as THEME TOKENS, never hardcoded hex (reference values are Catppuccin
  Mocha): indicator = green 6px connection dot (success) + "user@host · session
  <name>" (13px, session name mono, muted.foreground); picker = popover
  (popover bg, ref #181825; border, ref #45475a; radius 8, shadow), rows 30px:
  session name mono 13px + "N windows" muted caption right + attached-dot lane
  (fixed slot); current session row = surface bg (ref #313244) + 2px primary
  left bar; footer row "+ New session…" (ghost button style). No emojis.
- The switcher requires the daemon transport (`Attach` is a protocol message);
  on the `RIFT_TERMINAL_LEGACY` escape hatch it is inert (acceptable — the
  legacy path is slated for removal, #285).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Session list via `list-sessions` request/response + notification-driven coalesced re-query and push | Mirrors the proven KeyTableReply and layout re-query patterns already in the daemon; prior-art index Phase 19 (tmux Control Mode; iTerm2 UX reference) | 2026-07-05 |
| Switch = re-send `Attach { session }` on the same connection | The daemon already detaches the old control child, spawns the new one, and sends a fresh `LayoutSnapshot` (terminal.rs:111-114); the client already resets on snapshots (reconnect contract) | 2026-07-05 |
| No new attached-session protocol signal; close the `switch-client` gap with `request_layout()` in the `SessionChanged` arm | Since #429/#448 the daemon adopts `%session-changed`/`%session-renamed` into `self.session`, so `LayoutSnapshot`/`LayoutUpdate` already carry the actual name; only the layout refresh on external `switch-client` is missing (spec-review finding B1) | 2026-07-05 |
| Interim switcher placement: statusbar session label (click → popover) + command palette | The design's title-bar home for the indicator is phase 21 scope; blocking 19 on 21 inverts the roadmap order. The statusbar label already renders the session name today | 2026-07-05 |
| Parallel sessions v1 = second app instance (one OS window per session) | Zero protocol change (pane ids are server-global; each client gets its own control child); the multi-attach map is a real protocol redesign with no current dogfooding need | 2026-07-05 |
| New-session creation reuses attach-or-create (`new-session -A -s <name>`) | The daemon child command is already attach-or-create; "create" is just attaching to a fresh name | 2026-07-05 |
| Protocol-change ordering: the protocol/daemon issue of this phase depends on phase 20's version-negotiation issue | Adding new ClientMessage variants while a stale daemon may be running reproduces the exact skew-death this project just root-caused; land negotiation first (cross-milestone `Depends on:` edge) | 2026-07-05 |

## Prior art

- `docs/prior-art.md` → "v1.0 polish + robustness phases — prior-art index
  (Phases 19–26)", Phase 19 rows: tmux Control Mode (Category 3 #1) for
  `list-sessions` + `%sessions-changed`; iTerm2 tmux integration (Category 3
  #5) for the session-picker UX; the parallel-attach client model is
  `greenfield` (extends rift's own per-client control children).

## Human prerequisites

None — everything runs against the existing SSH host and tmux server; no new
secrets, accounts, or external provisioning.

## Tracking

- Milestone: created after this spec merges (phase 19).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Parser tests: `%sessions-changed`, `%client-session-changed` (valid +
      malformed); protocol round-trip tests for the new messages (valid +
      malformed)
- [ ] Behavioral (dev channel): create a second tmux session on the host →
      picker lists it within one coalesced refresh; switch to it → terminal
      shows its windows and the indicator shows its name; rename it externally
      (`tmux rename-session`) → indicator + list update without any structural
      event; kill it externally → list drops it and, if it was attached, the
      client surfaces the existing `TerminalExit` path
- [ ] "New session…" creates and attaches a fresh named session
- [ ] Two app instances attached to two different sessions work simultaneously
      (parallelism v1)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| New ClientMessage variants hit a stale running daemon → connection killed (the phase-20 smoking gun) | Cross-milestone dependency: this phase's protocol issue depends on phase 20's version-negotiation issue; until then the dev channel restarts the daemon on relaunch |
| `%sessions-changed` bursts (session churn) flooding re-queries | Coalesce exactly like the layout re-query (single in-flight query, trailing-edge re-issue) |
| Session names with spaces/unicode break the `list-sessions -F` line parsing | Tab-separated format with the name as the LAST field (LAYOUT_QUERY convention), `#{session_id}` as the key, parser tested with malformed input |
| Switching away mid-capture/mid-query leaks per-session state | The daemon drops per-connection attach state wholesale on re-Attach (already the case); client resets its layout model on the fresh snapshot |

## Decision log

- 2026-07-05: Spec drafted from the wave-1 daemon recon and the Paper design
  distillation (§1 indicator, §8.6 live list).
- 2026-07-05: Spec review (fresh-context agent, PR #459) — blocking finding B1
  accepted and baked in: #429/#448 (merged the same day) already consume
  `%session-changed`/`%session-renamed`, so the truthful-indicator work
  shrinks to `request_layout()` on external `switch-client`; no new protocol
  signal. Also adopted: tab-separated list format (name last),
  `%client-session-changed` as re-query trigger, theme tokens instead of hex
  literals, legacy-path note. Follow-up recorded: the roadmap row 19 wording
  ("title-bar switcher") and the phase-19 architecture-impact note in
  roadmap.md §"v1.0 polish cut" are corrected in this phase's step-8 roadmap
  update PR (interim statusbar placement; SessionChanged already consumed).
