# Spec: Project-optional session model

> Created: 2026-07-13

rift is usable the moment SSH connects: a project root is an optional per-session
property — picked at create OR set any time after — never a precondition, and no
screen on the connect&#8594;usable path is ever a dead-end. Reverses the phase-36
"session = project at creation" mandatory coupling and the phase-33/40
"Connect/kill always shows the picker" policy, while retaining the phase-40
connected-sessionless substrate (#813).

## Outcome

- [ ] Connecting to a host with **&#8805;1 session** auto-attaches a live session
      (the recents `preferred`, else the first session in the app's display order —
      the `session_order`-applied order the switcher renders, phase 32) and lands
      in the cockpit with no forced picker; the other sessions are reachable in the
      in-cockpit switcher (phases 19/32), and the picker stays available on demand.
- [ ] A session can be created with **no project root** (name-only): the root
      picker completes without a selected root, and the daemon attaches the new
      session root-less (no `-c`, no `@root`), exactly as it already does for a
      session created outside rift.
- [ ] A **missing or stale seeded root is a non-event**: the picker opens at the
      same default as a fresh pick (the daemon home) with no notice, no error
      banner, and no recovery buttons; there is no distinct "broken-seed" state.
- [ ] The root picker is **always escapable**: a persistent `Back` /
      `Disconnect` control and a `Start without a project root` action are present
      in every state, so the picker is never the only path to a usable state.
- [ ] The **active session's project root can be set from the cockpit**: a
      root-less session shows an explorer empty-state whose `Set project root`
      action browses the host and re-roots the reactive layer (file tree / git /
      LSP) by re-`Attach`ing the same session with the picked root (stamps
      `@root`, phase-35 re-root).
- [ ] Killing/exiting the active session with the connection alive **auto-switches**
      to the display-order head of the remaining sessions (0 remaining &#8594; the
      escapable create picker), never to the connection screen; a real transport
      loss still routes to the reconnect loop (unchanged from phase 20/40).
- [ ] `docs/architecture.md`'s Connection robustness contract documents the
      root-optional post-connect routing and the mid-session set-root affordance
      (this spec PR's amendment); `docs/spec-session-lifecycle.md` carries a
      superseding note for the reversed phase-40 routing policy.
- [ ] **No protocol change**: `PROTOCOL_VERSION` is unchanged — root-less attach
      and `@root` re-root are already in the wire format; the change is
      `crates/app`-only (plus the daemon browse-seed fallback, if touched).

## Scope

### In scope

- **Root-optional create** (`crates/app` root picker): a create path that emits
  `RootPickerEvent::Picked { root: None, name }`, wired through
  `resolve_attach_session` into `ClientMessage::Attach { session, root: None }`
  (the daemon already handles a root-less attach — `effective_attach_root(None,
  None) == None`, `crates/daemon/src/terminal.rs`). Surfaced as the
  `Start without a project root` action; `Create`/`Open` no longer requires a
  non-empty `current_path`.
- **Never-dead-end the picker** (`crates/app` root picker). The seed&#8594;home
  fallback **already shipped** (PR #882, in develop): a failed seed browse falls
  back to `$HOME`, and a stale recent root resolves silently via
  `apply_dir_entries_reply` / `seed_fallback_attempted` (`crates/app/src/root_picker.rs`;
  `browse.rs resolve_path` `""`&#8594;`$HOME`). The residual deliverable here is
  the **persistent `Back` / `Disconnect` control** on the root-picker chrome
  (`crates/app/src/main.rs` `render_root_picker_screen`, `root_picker.rs`) — the
  picker today has no cancel/back/disconnect — plus a QA confirmation that the
  stale-recent-root case resolves silently (no notice, no error UI) as designed.
- **Auto-attach entry model** (`crates/app/src/main.rs` post-connect routing): the
  `SessionIntent::Pick` post-connect resolution auto-attaches a live session — the
  recents `preferred` if still live, else the first session in the app display
  order (`session_order`, phase 32; falls back to the daemon's `SESSION_LIST_QUERY`
  order when unset) — via the existing `PickerOutcome::Attached` path instead of
  unconditionally emitting `ShowPicker` (`await_session_pick`, `main.rs`). The
  target selection is a pure function over (`preferred`, the session list,
  `session_order`) — **no session-activity data is added** (the wire `SessionEntry`
  carries none, so "most-recently-active" is deliberately NOT used — it would need
  a protocol change). The auto-attached session is recorded as the new `preferred`;
  the picker becomes an on-demand affordance. Includes the mid-session routing per
  the accepted gate policy (the phase-40 `run_daemon_terminal` re-pick, #813).
- **Set project root mid-session** (`crates/app` explorer/cockpit): a root-less
  session's explorer empty-state exposes `Set project root`, which reuses the
  remote browse (phase 36) and re-`Attach`es the current session with
  `root: Some(path)` — the existing attach-with-root path stamps `@root` and
  drives `reroot_connection` (`crates/daemon/src/lib.rs`), re-rooting file tree /
  git / LSP.
- **Foundation docs (ride this spec PR, ratified at the acceptance gate):** the
  `docs/architecture.md` Connection-robustness-contract amendment; the
  `docs/spec-session-lifecycle.md` superseding note (its always-picker /
  root-mandatory-on-zero routing is superseded here; its connected-sessionless
  substrate #813 is retained).
- **Design contract:** `docs/design.md` (added in this PR) plus the
  `Phase 47 — Project-optional session flows` Paper artboard as the visual
  contract for the picker, the root-less cockpit empty-state, and the routes.

### Out of scope

- **Changing or clearing an existing root** (a session that already has `@root`):
  deferred. The delivered need is **setting** a root on a **root-less** session
  (surfaced on the explorer empty-state, which is gone once a root exists);
  changing root A&#8594;B (recreate / switch the session instead) and an explicit
  un-set (`set -u @root` + reroot to `session_path`) are rare edges with **no
  surfaced entry point in this phase**. Recorded as a follow-up.
- **Multi-root / add-folder-to-project** (Zed-style multiple roots in one window):
  out — rift stays single-root-per-session (vision Scenario 2 deferred).
- **An empty cockpit with no attached session:** the connected-no-session state
  stays the escapable **picker** (the phase-40 substrate), NOT a cockpit render
  with no session — a session (possibly root-less) is always attached before the
  cockpit shows.
- **Protocol / daemon-message changes**, agent detection, the clone flow
  (phase 42, unchanged), and the in-cockpit session switcher itself
  (phases 19/32, reused as-is).

## Constraints

- **Agent-agnostic, app-internal state-machine change** — no new signal, no agent
  detection (`docs/constitution.md`). No `.unwrap()` in changed paths; reuse the
  existing channels/helpers (`await_session_pick`, `resolve_attach_session`,
  `route_picker`, `show_root_picker`, `reroot_connection`) rather than parallel
  plumbing.
- **Root-less attach is already modelled end-to-end** (phase 41,
  `spec-retire-project-root-env.md`): the daemon starts root-less, `serve_uds(None)`
  &#8594; `PrimaryContext::Standalone`, `resolve_session_root` falls back
  `@root` &#8594; `session_path`. A name-only create is a root-less `Attach`; the
  `Some(root)`-gated `@root`-stamp / reroot block (`terminal.rs`) is simply skipped
  — nothing new on the daemon side except (optionally) the browse-seed fallback.
- **Set-root reuses attach-with-root:** re-`Attach { session, root: Some(path) }`
  on the current session is exactly the phase-35 re-root path — no new message.
  The current-client handle (which the recovery engine may have swapped via
  `client_tx.send_replace`) is the re-`Attach` target, not the original handle.
- **The seed fallback must never trap:** the `""`&#8594;`$HOME` daemon resolution
  (`browse.rs`) is the fresh-pick default; an invalid recent root must resolve to
  that same default rather than surfacing `DirBrowseError::NotFound` as a terminal
  picker state. `#502`'s no-`$HOME`-watch guard is unaffected (this is the browse
  listing, not the watched root).
- **PROTOCOL_VERSION is unchanged** — asserted by the pinned fingerprint test; if
  a diff bumps it, the change strayed out of the `crates/app` scope.

## Prior art

- [Project-optional session model — prior-art index (Phase 47)](prior-art.md#project-optional-session-model--prior-art-index-phase-47)
  — VS Code Remote-SSH "empty window by design" (usable with no project, folder
  opened after); Zed "Add Folders to Project" (root is a post-open, changeable
  property); tmux `detach-on-destroy off` (session decoupled from transport);
  session-is-not-a-project (tmux/Zellij).
- [spec-session-lifecycle.md](spec-session-lifecycle.md) — the phase-40
  connected-sessionless substrate (#813) this builds on and whose routing policy
  it reverses.
- [spec-post-connect-picker.md](spec-post-connect-picker.md) /
  [spec-connection-robustness.md](spec-connection-robustness.md) — the
  `route_picker` / `PickerOutcome` seam and the phase-20 contract this amends.
- [archive/spec-session-root-picker.md](archive/spec-session-root-picker.md) /
  [archive/spec-per-session-project-root.md](archive/spec-per-session-project-root.md)
  — the phase-36 root picker and phase-35 `@root` re-root reused here.

## Human prerequisites

- none — `crates/app` (+ optional daemon browse-seed) and docs only; no secret,
  provisioning, or account is required to build or QA this.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| A project root is **optional** — a session may be created and used with none, and a root is settable any time after | The vision is a GUI for tmux + agentic IDE; the reactive layer is an enhancement a root lights up, not a precondition. Reverses the phase-36 "session = project at creation" mandatory coupling. Prior art: VS Code Remote empty-window-by-design, Zed add-folder-after-open. | 2026-07-13 (design review) |
| **Name-only create** emits `Attach { root: None }`; the daemon attaches root-less (no `-c` / `@root`), watched at `session_path` | The root-less attach path already exists end-to-end (phase 41); a name-only create reuses it with no daemon change. | 2026-07-13 |
| A **stale/absent seeded root is invisible**: the picker opens at the home default like a fresh pick — no notice, no error banner, no recovery buttons, no distinct broken-seed state | Design review: two recovery buttons (and even a fallback notice) were rejected as over-engineered. The daemon already falls `""`&#8594;`$HOME` (#872); make an invalid recent root resolve there silently. Removes the screenshot dead-end at its root. | 2026-07-13 (design review) |
| The picker's only escapes are a persistent **Back / Disconnect** and **Start without a project root** — no per-error recovery UI | Minimal, always-present exits make the picker unconditionally escapable; the silent home fallback removes the need for error-specific recovery affordances. | 2026-07-13 (design review) |
| **Set project root** (root-less &#8594; root) is surfaced on the root-less session's **explorer empty-state** ("No project root &#8594; Set project root"), reusing the remote browse + re-`Attach`-with-root; when the explorer area is hidden, its activity-rail toggle (phase 39) is the reveal path | Design review picked the explorer empty-state (where the absence is felt) over a command-palette-only entry; the mechanism is the existing phase-35 re-root, no new message. Scoped to setting a root on a root-less session — changing/clearing an existing root is out of scope. | 2026-07-13 (design review) |
| **Post-connect auto-attach:** connecting with &#8805;1 session auto-attaches `preferred` (recents) else the **first session in the app display order** (`session_order`, phase 32; daemon list order when unset), via the existing `PickerOutcome::Attached`; the picker is on-demand | Design review (Path A): "the picker is unnecessary — the switcher is always up." Reuses the phase-33 `Preferred` direct-attach; the Connect button stops forcing `ShowPicker`. The target is app-side data only (no `#{session_activity}` / protocol change) — so "most-recently-active" is deliberately not used; display order is deterministic and already what the switcher renders. | 2026-07-13 (design review) |
| The connected-no-session state stays the **escapable picker** (phase-40 substrate), not an empty cockpit render | A cockpit render with no attached session is a large, separate change (the WorkspaceView assumes an attached session/layout); a session — possibly root-less — is always attached before the cockpit shows. Keeps scope minimal. | 2026-07-13 |
| App-only; **`PROTOCOL_VERSION` unchanged** | Every reused message (`Attach` with/without root, `QuerySessionList`, the browse channel, `PickerOutcome`) already exists; this is a `crates/app` state-machine change (plus an optional daemon browse-seed tweak). | 2026-07-13 |
| **Mid-session kill policy (accepted):** killing/exiting the active session with the connection alive **auto-switches** to the display-order head of the remaining sessions (the same app-side target rule as post-connect); with **0 remaining** it lands on the escapable create picker (connected, no session). No mid-session picker is forced. | Fully symmetric with the post-connect auto-attach model — the switcher shows the rest. Reverses phase-40's "always-picker mid-session, no auto-attach". Same app-side target rule, no protocol data. | 2026-07-13 (accepted) |

## Design surface

- Visual contract: the Paper artboard **`Phase 47 — Project-optional session
  flows`** (`rift` file, `app.paper.design/file/01KTZZQ3CGGMPQXSTRVFBS5CTY`) —
  the escapable root picker (recent-root and stale/absent-seed states), the
  root-less cockpit `Set project root` empty-state, and the connect&#8594;usable
  routes. Governed by `docs/design.md`. The `(sparring)` artboard is the review
  copy; the durable surfaces extend the shipped `Connection — Startup` /
  `Cockpit — IDE` / `Explorer — Redesign` artboards and must match their language.

## Tracking

- Milestone: created at the spec-acceptance gate.
- Issues: created from this spec after merge (one per implementable step); the
  step list lives only as issues.

Each issue references this spec path in its body.

## Verification

Machine gates (`docs/workflow.md`):

- [ ] `just ci` green (fmt-check + clippy `-D warnings` + tests, workspace excl.
      `rift-app`); CI `app-check` compiles `rift-app`.
- [ ] `PROTOCOL_VERSION` unchanged (pinned fingerprint test green).
- [ ] Pure-seam unit tests: name-only create resolves to `Attach { root: None }`;
      the post-connect auto-attach target selection — a pure function over
      (`preferred`, the session list, `session_order`) returning `preferred`-if-live
      else the display-order head — is unit-tested. (The stale-recent-root &#8594;
      home resolution already ships and is covered by #882's tests; this phase adds
      the escapable-chrome + QA, not a new fallback.)

Human milestone-QA gate (dev channel, `just dev-windows-watch`):

- [ ] Connect to a host with sessions &#8594; a session auto-attaches into the
      cockpit, no picker; the others are in the switcher.
- [ ] Connect to a host with **zero** sessions &#8594; the escapable create picker;
      click `Start without a project root` &#8594; a root-less session attaches and
      the cockpit shows with a working terminal.
- [ ] Reproduce the original lockout — connect where the last-used root no longer
      exists (e.g. an exec-wrapper/container host) &#8594; the picker opens at home
      like a fresh pick, no error, no dead-end; create or skip to a usable cockpit.
- [ ] In a root-less cockpit, the explorer shows `No project root &#8594; Set
      project root`; using it browses the host and lights up the file tree / git /
      diagnostics on the chosen root.
- [ ] Kill the active session &#8594; auto-switches to another session (the
      display-order head); with none remaining, the escapable create picker;
      connection alive, no connection screen; a real transport loss still shows the
      reconnect banner.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Auto-attach on connect surprises the user by attaching an unexpected session | Attach the recents `preferred` (the last session used on this host) first, only falling back to the display-order head; the auto-attached session becomes the new `preferred` so it resumes the same next time; the switcher makes the rest one click away, and a fresh host (no sessions) never auto-attaches. |
| A name-only (root-less) session lands at an unhelpful cwd (daemon home) | Correct and expected — root-less means "no watched project"; the `Set project root` empty-state is the one-click path to bind one. `#502`'s no-`$HOME`-watch guard keeps the reactive layer empty (not wrongly watching home) until a root is set. |
| Reversing the phase-40 always-picker mid-session leaves `spec-session-lifecycle.md` internally inconsistent | This PR adds a superseding note to that spec and amends the architecture contract; the mid-session policy is the one gate decision, so the reversal is explicitly ratified. |
| Making `Create`/`Open` root-optional weakens the "session = project" intent for users who want it | Picking a root is still the default first-class path (folder &#8594; `@root`), unchanged; root-optional only removes the *hard requirement*. |
| The connected-no-session picker render must survive a mid-session re-entry with a live client | Reuse the phase-40 #813 substrate (the `run_daemon_terminal` re-pick over the current client); this spec changes the routing policy on top, not the substrate. |

## Decision log

- 2026-07-13: Spec drafted from the Phase 47 roadmap seed + a Paper design review
  (the `Phase 47 — Project-optional session flows` artboard). Motivating failure:
  connecting the stable channel to a host whose seeded root no longer exists
  dead-ended the root picker, forcing a repo clone to proceed.
- 2026-07-13 (design review): resolved the broken-seed handling to a **silent home
  fallback** (no notice, no recovery buttons — the user rejected both as
  over-engineered); confirmed the `Start without a project root` skip affordance,
  the persistent Back/Disconnect exits, the explorer-empty-state `Set project
  root` surface, and the post-connect auto-attach entry model. `docs/design.md`
  authored as the design contract (Paper medium).
- 2026-07-13: One open decision (the mid-session kill policy + 0-remaining
  landing) carried to the spec-acceptance gate; recommendation recorded
  (auto-switch, symmetric with post-connect).
- 2026-07-13 (spec-acceptance gate): mid-session kill policy **accepted** —
  auto-switch to the display-order head on kill (the same target rule as
  post-connect), 0-remaining &#8594; the escapable create picker; no forced
  mid-session picker. Reverses phase-40's always-picker. Human prerequisites:
  none. Spec accepted; milestone + issues follow.
- 2026-07-13 (spec review): two blocking findings fixed. (1) The auto-attach
  fallback "most-recently-active" is not derivable app-side — the wire
  `SessionEntry` carries no activity and `SESSION_LIST_QUERY` fetches none, so it
  would force a `PROTOCOL_VERSION` bump; replaced with the **display-order head**
  (`session_order`, phase 32; daemon list order when unset), a pure app-side
  function, preserving the no-protocol-change invariant. (2) "Change an existing
  root" had no entry point — explicitly scoped **out** alongside clear; the
  delivered need is setting a root on a root-less session. Non-blocking folded in:
  the seed&#8594;home fallback already ships (#882) so the residual "never
  dead-end" work is the persistent Back/Disconnect chrome; the explorer
  empty-state's activity-rail reveal path noted. All other code-fact claims
  verified true against source.
