# Spec: Mid-session session lifecycle

> Created: 2026-07-10

When the attached tmux session ends (killed from the cockpit, or its attach
otherwise exits) while the SSH/daemon connection is still alive, rift returns to
the pre-cockpit picker over the live connection instead of tearing down to the
connection screen: the session picker when one or more sessions remain (always —
even for exactly one, no auto-attach), the zero-sessions root picker when none
remain. The connection screen is entered only on a real SSH/transport loss.
"Session ended" stops meaning "disconnected".

## Outcome

- [ ] Killing the attached session with ≥1 other session on the host lands on the
      session picker (with the connected title-bar chrome), keeps the SSH/daemon
      connection alive (no reconnect banner, no connection screen), and a pick
      re-attaches and re-roots the reactive layer.
- [ ] Killing the attached session with 0 sessions remaining lands on the
      zero-sessions root picker (the create flow), connection alive; creating a
      session from it attaches and returns to the cockpit.
- [ ] The mid-session picker never auto-attaches — it is shown even when exactly
      one session remains.
- [ ] A real SSH/transport loss still routes to the reconnect loop and, on
      give-up/cancel, the connection screen — unchanged from phase 20.
- [ ] `docs/architecture.md`'s Connection robustness contract documents the
      mid-session sessionless state and the session-end-vs-transport-loss
      distinction (this spec PR's amendment).
- [ ] No protocol change: `PROTOCOL_VERSION` is unchanged; the change is
      `crates/app`-only.

## Scope

### In scope

- The daemon-terminal path (`crates/app/src/main.rs` `run_daemon_terminal`): on
  `StreamEnd::TerminalExit`, keep the SSH connection, daemon client, tokio
  runtime, and reverse-path bridges alive and re-enter the session-resolution
  path (re-query the live list, re-show the picker, re-`Attach` the pick) over
  the same live client — instead of returning `Ok(())` and unwinding to the
  connection screen.
- The Shell render side (`crates/app/src/main.rs`): drive the pre-cockpit
  `ScreenState::Picker` / `ScreenState::RootPicker` machinery mid-session with a
  live daemon client — the picker-outcome handler receives repeated outcomes over
  a session's life, and a mid-session pick returns to the same (re-rooted)
  `WorkspaceView`.
- The `docs/architecture.md` Connection robustness contract amendment (rides this
  spec PR, ratified at the acceptance gate).

### Out of scope

- Killing a **non-attached** session from the always-visible session strip
  (phase 32): that already works — it is a `kill-session` `TmuxCommand` on another
  session and never ends the attach, so no `TerminalExit` fires. Untouched.
- The legacy `tmux -CC` escape hatch (`RIFT_TERMINAL_LEGACY`): no daemon client,
  no picker machinery, and recovery is already scoped to the daemon path (#475).
  A session kill there keeps its pre-phase-40 behavior.
- Protocol / daemon changes. Every message this reuses (`QuerySessionList`,
  `Attach`, `TerminalExit`, the picker channels) already exists.
- The zero-sessions root-picker create flow itself (phase 36) and the per-session
  re-root chain (phase 35) — reused as-is, not modified.

## Constraints

- Agent-agnostic, app-internal state-machine change — no new signal, no agent
  detection (`docs/constitution.md`).
- The reverse-path bridges resolve their client via `client_rx` per send and must
  survive a re-`Attach` (the phase-20/33 switch/reconnect model): they are spawned
  once, not per session-lifecycle iteration. To spawn them once outside the new
  outer loop, the post-`Attach` bridge block (`main.rs` ~2035-2092) moves above the
  first `await_session_pick` — safe (no render events fire pre-cockpit; mirrors the
  already-early `spawn_dir_browse_bridge`).
- The mid-session re-`Attach` / re-pick targets the **current** client handle —
  the one the recovery engine may have swapped via `client_tx.send_replace` after
  a daemon reconnect (`main.rs` ~2105/2120) — not the original handle passed to the
  first `await_session_pick`. (A reconnect-then-`TerminalExit` within one session
  must re-pick over the reconnected client.)
- `run_daemon_terminal` returns `Err` (a transport-shaped `rift_ssh::SshError` in
  the chain) only on a genuine transport loss, so `is_retryable_session_error`
  keeps driving the reconnect loop; a `TerminalExit` must no longer surface as the
  loop's `Ok(())` orderly-exit break.
- No `.unwrap()` in the changed paths; reuse the existing channels and helpers
  (`await_session_pick`, `resolve_attach_session`, `show_session_picker`,
  `show_root_picker`, the `PickerChannels`) rather than adding parallel plumbing.

## Prior art

- [Mid-session session lifecycle — prior-art index (Phase 40)](../prior-art.md#mid-session-session-lifecycle--prior-art-index-phase-40)
  — tmux `detach-on-destroy off` (switch/return, never disconnect the transport),
  rendered as rift's always-picker; VS Code Remote / Zed / rift's own
  daemon-as-proxy for connection-persists-independent-of-session; reuse rift's own
  phase-33 post-connect picker + phase-20 recovery re-Attach mid-session with a
  live client.
- [spec-post-connect-picker.md](spec-post-connect-picker.md) — the
  `await_session_pick` → `PickerOutcome::ShowPicker` → `show_session_picker` /
  `show_root_picker` seam this reuses mid-session; the empty-vs-non-empty routing.
- [spec-connection-robustness.md](spec-connection-robustness.md) — the phase-20
  contract this amends (transport-loss reconnect loop, connection screen as the
  not-connected state) and the `is_retryable_session_error` classification.

## Human prerequisites

- none

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| On the attached session ending with the connection alive, re-enter the pre-cockpit picker over the LIVE daemon client, not the connection screen | The connection screen (phase 20) owns transport-loss only; a session kill loses no transport. "Session ended" becomes a mid-session transition (prior-art: tmux `detach-on-destroy off` / VS Code Remote connection-vs-session separation) | 2026-07-10 |
| Route by remaining count: ≥1 → session picker; 0 → zero-sessions root picker | Reuses the existing `PickerOutcome::ShowPicker(sessions)` empty-vs-non-empty routing (`main.rs`), superseding the empty-state per phase 36 — one routing rule for post-connect and mid-session | 2026-07-10 |
| The mid-session re-pick ALWAYS shows the picker — never auto-attaches, even for exactly one remaining session | The user's explicit choice (roadmap seed / prior-art): unlike the post-connect first entry (which may honor a `Preferred`/recent name), the mid-session re-entry passes `preferred = None` to `await_session_pick` so it never short-circuits to a direct attach (`resolve_preferred_session(None, …)` returns `None`, so `ShowPicker` is always emitted). The outer loop distinguishes first-attach (pass `watches.preferred_session`) from every re-entry (pass `None`) | 2026-07-10 |
| Keep the SSH connection, daemon client, tokio runtime, and reverse-path bridges alive across the session end; re-`Attach` over the same live client | This IS the "connected, no active session" first-class mid-session state; a re-`Attach`'s fresh `LayoutSnapshot` resets the render layer exactly like a cockpit switch/reconnect, and the phase-35 re-root follows the new session's `@root` | 2026-07-10 |
| A mid-session pick returns to the same eagerly-built `WorkspaceView` (re-rooted by the `Attach`), and records the pick into recents | Matches the cockpit-switch + phase-35 re-root model (stale tabs closed on root change); reusing `show_session_picker`'s Pick handler keeps recents consistent between post-connect and mid-session picks | 2026-07-10 |
| App-only; `PROTOCOL_VERSION` unchanged | Every reused message and the picker seam already exist; this is a `crates/app` state-machine restructure, no `protocol`/daemon touch | 2026-07-10 |
| Depends on Phase 38 (#808, milestone #57): retire the `SessionIntent::Fixed` (`RIFT_SESSION`) fast-path | The mid-session re-picker reuses the Preferred/Pick picker machinery, which the `Fixed` fast-path bypasses (no Shell picker handler, watch seeded non-empty). Phase 38 removes `Fixed`, so post-#808 every connection wires the picker and the re-picker covers it uniformly with no special-casing. **OPEN — confirmed vs. also-handle-Fixed, resolved at the spec-acceptance gate** | 2026-07-10 |
| Foundation impact: `docs/architecture.md` Connection robustness contract gains the mid-session sessionless state + session-end-vs-transport-loss distinction | Authored in this spec PR, ratified at the acceptance gate; the phase-20 contract currently ties any `run_daemon_terminal` end to a teardown | 2026-07-10 |

## Tracking

- Milestone: created at the acceptance gate.
- Issues: created from this spec once it is merged (one per implementable step).

Each issue references this spec path in its body.

## Verification

Machine gates (`docs/workflow.md`):

- [ ] `just ci` green (fmt-check + clippy `-D warnings` + tests, workspace excl.
      `rift-app`).
- [ ] CI `app-check` compiles `rift-app`.
- [ ] The mid-session routing is unit-tested at whatever pure seam the
      implementation exposes: the empty-vs-non-empty routing (`ShowPicker(sessions)
      if sessions.is_empty()`) lives in the GPUI Shell handler today, so testing it
      as a pure function needs a small helper extraction (a
      `route_picker(sessions) -> RootPicker | SessionPicker` classifier); the
      `preferred = None` forces-the-picker behavior is testable directly via
      `resolve_preferred_session`. Whichever is behavioral-only is covered by the
      human QA gate below, not asserted as a pure test.

Human milestone-QA gate (dev channel, `just dev-windows-watch`):

- [ ] Kill the attached session (cockpit chip kill-confirm) with ≥1 other session
      present → the session picker appears with connected chrome; no reconnect
      banner, no connection screen; the SSH/daemon connection is unbroken.
- [ ] Pick a session from the mid-session picker → it attaches and the reactive
      layer (file tree / git / diagnostics) re-roots to that session.
- [ ] Kill the attached session with no other session on the host → the
      zero-sessions root picker appears; create a session → it attaches and the
      cockpit returns.
- [ ] With exactly one other session remaining, the mid-session picker is still
      shown (no silent auto-attach).
- [ ] A real SSH/transport loss (drop the network/SSH) still shows the reconnect
      banner and, on give-up/cancel, the connection screen — unchanged.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Restructuring `run_daemon_terminal` into an outer session-lifecycle loop re-spawns the reverse-path bridges each iteration (duplicate input/resize/… handlers) | Spawn the bridges once, before/outside the lifecycle loop; only the resolve → `Attach` → re-assert-viewport → consume segment repeats, mirroring how a re-`Attach` already reuses the live bridges |
| A daemon stream death while the mid-session picker is showing hangs the pick forever | Reuse `await_session_pick`'s existing race: the pick is consumed against the same stream, so a stream death unblocks it as a transport error → the reconnect loop re-shows the picker (the watch stays unset), exactly like the post-connect picker's reconnect |
| The Shell picker-outcome handler is a single `recv` + `.detach()` today, so a second (mid-session) `ShowPicker` is never received | Make the handler loop over repeated outcomes for the connection's life; a mid-session pick returns to the workspace and the loop keeps serving the next session end. The loop must clone the per-iteration captures (`workspace`, `recent_target`, `picker_choice_tx`) rather than move-consume them once (`main.rs` ~1206-1265) — the picker channels are `flume` clones, so this is a mechanical change |
| Landing before Phase 38 leaves the `SessionIntent::Fixed` path (no picker machinery) with an undefined mid-session kill | Depends-on Phase 38 #808 (the recommended resolution of the open decision); if the human chooses to land earlier, the Fixed mid-session kill is defined explicitly as a second issue |

## Decision log

- 2026-07-10: Scoped to the daemon-terminal path only; the legacy `-CC` escape
  hatch and non-attached-session kills are explicitly out of scope.
- 2026-07-10: Confirmed no protocol change — the re-query/re-attach/picker seam all
  pre-exist; this is a `crates/app` state-machine restructure.
