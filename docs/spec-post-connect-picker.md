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
      Connection screen's Session field is optional and carries no baked
      `"rift"` default that forces a choice.
- [ ] After the SSH connect + daemon handshake, the app shows a session picker —
      the live host session list (the phase-19 `QuerySessionList`) rendered with
      the phase-32 surface (switch / new / rename / kill) — and only attaches the
      cockpit once the user picks or creates a session.
- [ ] `DEFAULT_SESSION = "rift"` is removed as the forced connect default; the
      picker (not a baked name) resolves which session the cockpit attaches.
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
  initial `ClientMessage::Attach` (main.rs:1213) — issue `QuerySessionList` on the
  handshaken client, render the picker from the `SessionListReply` (the existing
  `spawn_session_list_bridge` main.rs:2383 + consumer main.rs:1988), then `Attach`
  the chosen session. The picker reuses the phase-32 session rendering + new /
  rename / kill; it is a pre-cockpit view, like the Connection screen.
- `app`: the connect pipeline no longer threads the session name through the SSH
  connect. `run_session_with_reconnect` / `EngineWatches.session` (main.rs:890) is
  seeded from the PICK, not the connect card; `run_ssh_session` (main.rs:1040)
  connects + provisions without reading a session (it is read only at Attach today,
  main.rs:1078/1213), so the reorder is a data-flow change, not a new capability.
- `connection_screen`: the Session field becomes optional (a prefill / preselect,
  not a requirement); `build_request` (connection_screen.rs:360-406) no longer
  defaults an empty field to `"rift"`; `DEFAULT_SESSION` (connection_screen.rs:43)
  is removed or demoted to "preselected in the picker". `RIFT_SESSION` stays a
  preselect hint (see the flow decision, resolved at the gate).
- `app`: architecture-doc amendment (foundation impact) — extend the phase-20
  "Connection robustness contract" / startup-state description in
  `docs/architecture.md` to the connect → session-pick → cockpit flow, including
  the reconnect interaction (current-session watch starts unset until the pick).
  Authored in this spec's PR, ratified at the spec-acceptance gate.

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
| The picker is a third `Shell` state (post-connect, pre-attach), reusing the phase-32 session surface | The Shell already switches between Connection screen and WorkspaceView (#477); a pre-cockpit picker is the same pattern; reusing phase-32 avoids a second session UI. The session name is consumed only at Attach (main.rs:1213), so the connect + handshake need no session | 2026-07-08 |
| No protocol / daemon change — the picker drives the existing `QuerySessionList` + `Attach` | The client already runs `QuerySessionList` post-connect (phase 19, main.rs:2383) and switches via `Attach`; this phase only moves the pick ahead of the cockpit commit | 2026-07-08 |
| `DEFAULT_SESSION = "rift"` retired as the forced connect default | The whole point is to not force a session at connect time; the picker resolves it. `RIFT_SESSION` survives as a preselect hint only | 2026-07-08 |
| Foundation impact: amend the phase-20 `architecture.md` connection contract to connect → pick → cockpit | Phase 20 made "Connection screen is the startup state" an architecture contract; inserting a pick step (and its reconnect interaction) changes that flow, so the contract is updated in this spec's PR and ratified at the gate | 2026-07-08 |
| Flow model — mandatory picker after every connect vs a fast-path when a session is already specified (card / `RIFT_SESSION` / remembered last), and single-session auto-attach | OPEN — resolved at the spec-acceptance gate | 2026-07-08 |

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
  Phase 320 (phase 32 — the picker reuses its session surface).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Behavioral (dev channel): connect with the Session field left blank →
      after the handshake the picker shows the live host session list; picking one
      attaches the cockpit to it; creating a new one attaches to the fresh session
- [ ] `DEFAULT_SESSION` is gone: connecting no longer silently attaches `"rift"`;
      grep confirms no forced default remains
- [ ] Reconnect: drop SSH after a pick → the engine re-attaches the PICKED session
      (not a startup default); drop before a pick → returns to the picker /
      Connection screen, never a blind attach
- [ ] The flow decided at the gate holds (e.g. a specified session / `RIFT_SESSION`
      fast-path attaches directly when chosen; otherwise the picker shows)
- [ ] `docs/architecture.md` reflects the connect → pick → cockpit flow

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| A drop between connect and pick leaves the engine with no session to re-attach | The picker is a Shell state; a drop there returns to the picker / Connection screen (no blind attach); the current-session watch stays unset until the pick |
| Removing `DEFAULT_SESSION` breaks the dogfooding channels' `RIFT_SESSION=rift-dev` isolation (spec-dogfooding-channels.md) | `RIFT_SESSION` survives as a preselect / fast-path hint; the dev channel still lands on its session (the flow decision preserves the specified-session fast-path) |
| The picker adds a step to the common "always the same session" path | The gate's flow decision can keep a fast-path (specified session / remembered last / single session auto-attaches), so the picker only shows when the session is genuinely unspecified |
| Reusing the phase-32 surface pre-cockpit couples the two milestones | Cross-milestone `Depends on:` edge (phase 33's picker issue depends on phase 32's surface issue); phase 33 is sequenced after phase 32 |

## Decision log

- 2026-07-08: Spec drafted from the phase-33 seam map. Key finding: the session
  name is consumed only at `Attach` (main.rs:1213), NOT for the SSH connect or the
  daemon handshake (main.rs:1049-1068), so "connect first, then pick" is a data-flow
  reorder inserting a picker between `provision_daemon` and `Attach` — no protocol
  or daemon change. This is an `[app]`-only phase reusing phase-32's session
  surface. The roadmap's foundation note (architecture.md connection-flow) is
  authored in this spec's PR and ratified at the gate.
