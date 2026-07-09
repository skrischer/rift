# Spec: post-connect session picker

> Status: READY
> Created: 2026-07-08
> Completed: —

Pick the tmux session AFTER connecting, not before: the SSH connect + daemon
handshake no longer need a session name up front, a picker (reusing the phase-32
session surface) sits between connect and cockpit, and the hardcoded `"rift"`
default on the connect card is retired.

## Outcome

- [ ] Connecting to a host no longer requires a session name up front: the
      Connection screen's Session field is removed and no baked `"rift"` default
      forces a choice — the ENTRY POINT decides the session (a recent's remembered
      session, `RIFT_SESSION`, or the post-connect picker).
- [ ] When the entry point does not resolve a session (the plain "Connect →"
      button, or a recent whose remembered session is gone), after the SSH connect
      + daemon handshake the app shows a session picker — the live host session
      list (the phase-19 `QuerySessionList`) rendered with the phase-32
      session-row/chip component (switch + new are the picker's job; rename / kill
      ride along only because the reused component includes them) — and only
      attaches the cockpit once the user picks or creates a session.
- [ ] `DEFAULT_SESSION = "rift"` and the card Session field are removed; the entry
      point / picker (not a baked name) resolves which session the cockpit attaches.
- [ ] The reconnect / resync contract (phase 20) still holds: a drop after a pick
      re-attaches the picked session (the engine's current-session watch), and a
      drop before a pick returns to the picker, never a blind attach.
- [ ] The flow is agent-agnostic and reuses the existing streams; no protocol or
      daemon change.

## Scope

### In scope

- `app`: a third `Shell` state (crates/app/src/main.rs, the #477 Connection-screen
  / WorkspaceView split) — a **session picker** shown post-connect, pre-attach.
  Insertion point: between `provision_daemon` (main.rs:1068, the handshake) and the
  daemon-path `ClientMessage::Attach` (in `run_daemon_terminal`, main.rs ~:1207/1213)
  — issue `QuerySessionList` on the handshaken client, render the picker from the
  `SessionListReply` (the existing `spawn_session_list_bridge` main.rs:2383 +
  consumer main.rs:1988), then `Attach` the chosen session. The picker reuses the
  phase-32 session-ROW/chip component + its new / rename / kill affordances — NOT
  the phase-32 title-bar strip, which is a cockpit-only surface (no WorkspaceView
  exists pre-attach) — hosted in this new pre-cockpit container, like the
  Connection screen.
- `app`: the (re)connect path gates the picker on the current-session watch. The
  reconnect engine (#475/#476) already owns connect + handshake + Attach and tracks
  the current session via a watch (`EngineWatches.session`, main.rs:890, post-#509).
  Change: post-handshake, if the watch is UNSET (unspecified + not yet picked), show
  the picker and Attach only on the pick (seeding the watch); once the watch is
  seeded (a specified fast-path or a completed pick), reconnects re-`Attach` it
  directly WITHOUT re-showing the picker. A drop before the pick re-shows the
  picker; a drop after re-attaches the picked session — no blind Attach. The session
  is read only at that daemon-path Attach today (main.rs ~:1207/1213; the :1078 read
  feeds the legacy non-daemon branch), so this is a data-flow + one-branch change,
  not a new capability.
- `connection_screen`: the Session field is REMOVED from the connect card;
  `build_request` (connection_screen.rs:360-406) no longer carries or defaults a
  session; `DEFAULT_SESSION` (connection_screen.rs:43) is deleted. The recents store
  already carries the last session per host (recents.rs:38 `session`, and its dedupe
  already excludes the session from identity so it updates to the newest, :58-60) —
  no store schema change; a successful attach records the (host, attached session).
- `app`: the entry point decides the session, applied after the post-handshake
  `QuerySessionList`:
  - `RIFT_SESSION` set (env-driven launch, e.g. the dev channel) → attach-or-create
    that session directly, no picker (the dogfooding fast-path);
  - connect via a RECENT row → if the recent's remembered session is present in the
    live list, attach it directly; if it is gone, show the picker;
  - connect via the "Connect →" button (a fresh connect) → always show the picker.
- `app`: architecture-doc amendment (foundation impact) — extend TWO bullets of the
  phase-20 "Connection robustness contract" in `docs/architecture.md`: the
  startup-state / not-connected flow (now connect → session-pick → cockpit) AND the
  "no silent stream death / terminal re-Attach" bullet (the re-Attach now carries an
  unset-session-until-pick precondition — reconnect re-shows the picker if unpicked,
  re-attaches the picked session otherwise). Authored in this spec's PR, ratified at
  the spec-acceptance gate.

### Out of scope

- The session management operations themselves — rename / reorder / kill / new and
  the glanceable surface are phase 32 (`spec-session-management.md`); this phase
  reuses them in a pre-cockpit picker, adding no new operation.
- Any protocol or daemon change: `QuerySessionList` / `Attach` already exist;
  `PROTOCOL_VERSION` stays 8.
- Multi-host connection management (phase-20 out-of-scope; recents stay convenience
  prefills, not a session manager).
- SSH auth / passphrase changes (phase 20 owns those).
- Auto-connect on launch — phase 20's "Connection screen is the startup state, no
  auto-connect" decision stands; this phase adds a step AFTER the explicit connect,
  it does not remove the explicit connect.

## Constraints

- Preserve the phase-20 startup contract: the Connection screen is still the
  startup state and connect is still explicit; the picker is a NEW intermediate
  state between connect and cockpit, never an auto-connect.
- Preserve the phase-20 reconnect/resync contract: the reconnect engine already
  tracks the current session via a `watch` (post-#509); with a post-connect pick
  the watch starts unset — a drop before the pick returns to the picker (or
  Connection screen), a drop after re-attaches the picked session. No blind attach
  to a stale/startup name.
- No protocol / daemon change — the picker drives `QuerySessionList` + `Attach`,
  both existing.
- Agent-agnostic; theme tokens only (the picker reuses the Connection-screen +
  phase-32 tokens); no `.unwrap()` in libs; crate boundaries via `lib.rs`;
  English; no emojis.
- The `RIFT_TERMINAL_LEGACY` escape hatch has no daemon transport, so it cannot
  query a session list — it keeps its current fixed-session behavior (the legacy
  path is slated for removal, #285).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The picker is a third `Shell` state (post-connect, pre-attach) reusing the phase-32 session-ROW/chip component + its new / rename / kill affordances (NOT the title-bar strip, a cockpit-only surface), hosted in a new pre-cockpit container | The Shell already switches between Connection screen and WorkspaceView (#477); a pre-cockpit picker is the same pattern; reusing the phase-32 row component (not the strip) avoids a second session UI. The session name is consumed only at the daemon-path Attach (main.rs ~:1207/1213), so the connect + handshake need no session | 2026-07-08 |
| No protocol / daemon change — the picker drives the existing `QuerySessionList` + `Attach` | The client already runs `QuerySessionList` post-connect (phase 19, main.rs:2383) and switches via `Attach`; this phase only moves the pick ahead of the cockpit commit | 2026-07-08 |
| The ENTRY POINT decides the session: `RIFT_SESSION` (env) → attach-or-create directly, no picker; a RECENT-row connect → attach its remembered session if present on the host, else the picker; the "Connect →" button → always the picker. The card's Session field is removed | The dogfooding channel launches `RIFT_SESSION=rift-dev just dev-windows-watch` and must be honored without a picker (spec-dogfooding-channels.md); a recent already stores its last session (recents.rs:38), so a returning host reattaches where you left off (when it still exists); a fresh "Connect →" is exactly the "pick after connect" case. This resolves the flow model completely — no open residue; iTerm2's attach-server-then-Dashboard is the precedent | 2026-07-09 |
| The picker lives on the (re)connect path, gated on the current-session watch: post-handshake, if the watch is unset the picker shows and Attach fires only on the pick; once seeded (fast-path or a completed pick), reconnects re-Attach directly without re-showing it | The reconnect engine (#475/#476) owns connect + handshake + Attach and tracks the current session via a watch (post-#509); gating on that watch means a mid-session drop after a pick re-attaches the picked session and a drop before it re-shows the picker — no blind Attach, no second sync path | 2026-07-08 |
| `DEFAULT_SESSION = "rift"` and the card Session field removed | Not forcing a baked default at connect time is the whole point; the entry point (recent / `RIFT_SESSION` / picker) resolves the session, so the card needs no Session input | 2026-07-09 |
| Foundation impact: amend TWO bullets of the phase-20 `architecture.md` connection contract — the startup-state flow (connect → pick → cockpit) AND the "no silent stream death / terminal re-Attach" bullet (the re-Attach now has an unset-session-until-pick precondition) | Phase 20 authored that contract in its own PR (#470); inserting a pick step changes both the startup flow and the re-Attach precondition, so both are updated in this spec's PR and ratified at the gate | 2026-07-08 |

## Prior art

- `docs/prior-art.md` → "Session management & post-connect picker — prior-art index
  (Phases 32–33)", Phase 33 rows: iTerm2 tmux integration (attach the `-CC` server
  first, the Dashboard / session list then drives which session shows — the pick is
  post-attach, never a pre-connect requirement) as the flow reference; WezTerm
  launcher (fuzzy pick, create-on-select); de-hardcoding the default session is a
  greenfield refactor of rift's own connect card.

## Human prerequisites

None — everything runs against the existing SSH host and tmux server; no new
secrets, accounts, or external provisioning.

## Tracking

- Milestone: created after this spec merges (phase 33). Depends on milestone:
  Phase 320 (#49, phase 32) — the picker reuses its session-row component; the
  parseable `Depends on milestone: #49` edge lives in the milestone description.
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Behavioral (dev channel): connecting via the "Connect →" button → after the
      handshake the picker shows the live host session list; picking one attaches
      the cockpit to it; creating a new one attaches to the fresh session
- [ ] `DEFAULT_SESSION` and the card Session field are gone: connecting no longer
      silently attaches `"rift"`; grep confirms no forced default remains
- [ ] Reconnect: drop SSH after a pick → the engine re-attaches the PICKED session
      (not a startup default); drop before a pick → returns to the picker /
      Connection screen, never a blind attach
- [ ] Entry-point flow: `RIFT_SESSION` set → attaches directly, no picker
      (dogfooding); connect via a recent whose remembered session still exists →
      attaches it directly; connect via a recent whose session is gone → picker;
      the "Connect →" button → always picker (even with one session on the host)
- [ ] Zero-sessions edge: a fresh host with no sessions yields an empty list → the
      picker renders "+ New session…" only and `new-session -A` attaches the fresh
      one (and the list updates live if a session is created/killed while open)
- [ ] A successful attach records the (host, attached session) in recents, so the
      next connect via that recent remembers it
- [ ] `docs/architecture.md` reflects the connect → pick → cockpit flow AND the
      re-Attach's unset-session-until-pick precondition (both phase-20 bullets)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A drop between connect and pick leaves the engine with no session to re-attach | The picker is a Shell state; a drop there returns to the picker / Connection screen (no blind attach); the current-session watch stays unset until the pick |
| Removing `DEFAULT_SESSION` breaks the dogfooding channels' `RIFT_SESSION=rift-dev` isolation (spec-dogfooding-channels.md) | `RIFT_SESSION` is an entry-point fast-path (prior decision): it attaches directly without the picker, so the dev channel lands on its session unchanged |
| The picker adds a step to the common "always the same host + session" path | Connecting via that host's RECENT row reattaches its remembered session directly (if still present); only a fresh "Connect →" or a vanished session shows the picker, so the returning-user case is unaffected |
| Reusing the phase-32 surface pre-cockpit couples the two milestones | Cross-milestone `Depends on:` edge (phase 33's picker issue depends on phase 32's surface issue); phase 33 is sequenced after phase 32 |

## Decision log

- 2026-07-08: Spec drafted from the phase-33 seam map. Key finding: the session
  name is consumed only at `Attach` (main.rs:1213), NOT for the SSH connect or the
  daemon handshake (main.rs:1049-1068), so "connect first, then pick" is a data-flow
  reorder inserting a picker between `provision_daemon` and `Attach` — no protocol
  or daemon change. This is an `[app]`-only phase reusing phase-32's session
  surface. The roadmap's foundation note (architecture.md connection-flow) is
  authored in this spec's PR and ratified at the gate.
- 2026-07-08: Fresh-context review (PR #688): REQUEST_CHANGES → resolved. Blocking:
  the specified-session fast-path was effectively forced by the accepted
  dogfooding-channels workflow (`RIFT_SESSION=rift-dev`), so it is promoted from the
  OPEN flow question to a prior decision; the OPEN residue narrows to single-session
  auto-attach + remembered-last-as-fast-path-source. Non-blocking adopted: the
  reconnect-engine branch (picker gated on the current-session watch) named as owned
  scope; the reuse unit clarified to the phase-32 session-ROW component (not the
  title-bar strip, which is cockpit-only); rename/kill framed as riding along, not a
  picker feature; the insertion citation corrected (:1078 is the legacy branch, the
  daemon Attach is ~:1207/1213); the foundation amendment scoped to TWO
  architecture.md bullets (startup flow + re-Attach precondition); a zero-sessions
  verification bullet added.
- 2026-07-09: Spec-acceptance gate — the residual flow question was resolved by an
  ENTRY-POINT model (the user's refinement): `RIFT_SESSION` (env) attaches directly
  (dogfooding); a RECENT-row connect attaches its remembered session if present on
  the host else shows the picker (the recents store already carries the last session
  per host, recents.rs:38); the "Connect →" button always shows the picker. This
  removed the card's Session field entirely and closed the single-session /
  remembered-last residue (no auto-attach by count — the entry point decides). Human
  prerequisites: none. Spec accepted.
