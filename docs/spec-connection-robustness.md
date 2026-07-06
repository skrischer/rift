# Spec: protocol & connection robustness

> Status: READY
> Created: 2026-07-05
> Completed: —

No silent deaths: app/daemon protocol skew is negotiated away at the handshake
(with a client-driven restart of a stale running daemon), a dead daemon stream
reconnects and resyncs instead of leaving a frozen IDE, an SSH drop enters a
visible reconnect loop instead of quitting the app, and a Connection screen
(per the Paper "Connection — Startup" artboard) owns the not-connected state.

Root cause evidence (wave-1 live QA, stable channel log 2026-07-05 09:08): the
stable app (protocol without `key_table_reply`) spawned the newer daemon; the
first unknown `DaemonMessage` variant produced "malformed daemon frame, closing
stream" 37 ms after startup — worktree, git, diagnostics, and activity streams
all died silently while the app kept running. This is the shape behind
"explorer/indicators partly non-functional".

## Outcome

- [ ] A protocol message-set change without a `PROTOCOL_VERSION` bump cannot
      pass CI (a fingerprint test in `crates/protocol` pins the message set).
- [ ] Client and daemon enforce version equality at Hello/Welcome; on mismatch
      with a RUNNING daemon the client stops it via the pidfile mechanism,
      re-deploys the matching binary, respawns, and reconnects — no manual
      intervention, no silent feature death.
- [ ] A daemon-channel death while SSH is up (EOF, malformed frame, channel
      error) triggers automatic reconnect + full state resync (worktree, git,
      diagnostics via the Welcome snapshot replay; terminal via re-Attach and
      the fresh-LayoutSnapshot reset contract) with a visible "reconnecting"
      state — never a permanently frozen reactive layer.
- [ ] An SSH drop never quits the app: the UI shows a danger banner ("SSH
      connection lost … retrying") + the `Reconnecting` status dot, retries
      with capped backoff, and fully re-initializes on success (daemon channel,
      attach, resync).
- [ ] The Connection screen is the startup state on every launch (prefilled;
      one click connects) and owns the not-connected state: connect card
      (host/user/port/key/session), recent connections, visible status.
- [ ] `ConnectionStatus::Reconnecting` (today a dead variant,
      crates/terminal/src/lib.rs:94) is reachable and rendered.

## Scope

### In scope

- `protocol`: bump `PROTOCOL_VERSION` to 2; add a message-set fingerprint test
  (stable hash over both enums' variant names + field names + field TYPES, so
  a wire-breaking type change also trips it) pinned beside the version
  constant — the test's failure message instructs "bump PROTOCOL_VERSION and
  re-pin"; document the policy in `docs/protocol.md`.
- `daemon`: enforce Hello version equality (today any version is accepted —
  crates/daemon/src/lib.rs:1273-1276 marks this as deferred); on mismatch reply
  `Welcome { version: <own> }` and close cleanly WITHOUT streaming (the old
  client then sees an orderly version signal, not a mid-stream codec death).
  Note: today the Welcome rides the shared broadcast bus (lib.rs:1286) — the
  mismatch reply must be per-connection so one mismatched client cannot
  interfere with a healthy client's stream (shared stable+dev daemon).
- `ssh`/`app`: client checks `Welcome.version` for equality (today it only
  logs — crates/app/src/main.rs:992-993); on mismatch: stop the running daemon
  via the existing pidfile stop (`stop_daemon`, crates/ssh/src/launch.rs:139, #281),
  re-run the existing versioned deploy (content-fingerprinted, deploy.rs),
  respawn, re-handshake (bounded by the #441 timeout). One retry; a second
  mismatch surfaces as a connection error (screen/banner). While touching this
  path, fix the adjacent stale comment (main.rs ~:976-978 still describes the
  pre-#227 "re-broadcast on every Hello" behavior).
- `app`: daemon-stream death recovery — on `daemon message stream ended` /
  malformed frame while SSH is alive: bounded auto-reconnect to the socket,
  fresh Hello/Welcome (snapshot replay restores worktree/git/diagnostics —
  the #227/#425/#426 mechanisms), re-Attach the terminal (fresh LayoutSnapshot
  reset is the existing reconnect contract, protocol lib.rs docs).
- `app`/`ssh`/`terminal`: SSH-drop handling — replace quit-on-disconnect
  (run_ssh_session error path, crates/app/src/main.rs:466-476, and the
  `Disconnected → cx.quit()` handler in crates/terminal/src/session_view.rs:350)
  with a reconnect loop (capped backoff) driving
  `ConnectionStatus::Reconnecting` (enum + status-dot rendering live in
  crates/terminal; the dot colors become theme tokens while touched); danger
  banner per design §7 with retry counter; success re-runs the full connect
  pipeline. Policy (gate decision 2026-07-05): unlimited retries with jittered
  capped backoff (30s cap) and a Cancel action leading to the Connection
  screen; auth/config failures skip retrying and go straight to the screen
  with the error surfaced.
- `app`: Connection screen per design §6 (UI contract below): connect card
  with Host / User / Port / SSH key / Session fields prefilled from env and
  baked defaults, `Connect` primary button, `tmux -CC -A` caption, RECENT list
  (small local store beside the window-state store), "not connected" titlebar
  state. The screen is the app's startup state on EVERY launch (gate decision
  2026-07-05): prefilled from config, one click (or Enter) connects; it also
  owns connect failures and a canceled reconnect. Auto-connect-on-launch is
  explicitly not wanted.
- `ssh`: passphrase-protected keys (today unsupported and failing log-only —
  `load_secret_key(&path, None)`, crates/ssh/src/connection.rs:52): decrypt
  with a passphrase entered in the Connection screen (never persisted; a wrong
  passphrase surfaces as a field-level error). In scope per gate decision
  2026-07-05.

### Out of scope

- Multi-version protocol compatibility / message translation — the policy is
  strict equality + client-driven daemon replacement (personal-tool scale;
  both binaries build from one repo).
- Daemon self-update or daemon-initiated upgrades (client owns the version).
- Custom title bar chrome (phase 21) — the "not connected" state reuses the
  existing title string until then.
- Multi-host connection management (one host per instance; recents are
  convenience prefills, not a session manager).
- The tmux session-list UI (phase 19; its protocol issue depends on this
  phase's negotiation issue via a cross-milestone edge).

## Constraints

- Constitution: no `.unwrap()` in libs; `thiserror` in libs / `anyhow` in
  binaries; state flows through channels; crate boundaries via `lib.rs`;
  protocol additions deliberate and documented; no emojis; English.
- The reconnect loop must not spin: capped exponential backoff, and the
  keepalive detection (#438) plus the handshake timeout (#441) bound every
  wait. No unbounded buffering while disconnected (drop and resync on
  reconnect — the daemon replays state, tmux replays the terminal).
- The Welcome-first ordering (#425) is load-bearing for resync: the snapshot
  replay after Welcome is the resync mechanism; do not introduce a second
  sync path.
- UI contract (design §6/§7, all colors as THEME TOKENS — reference values
  Catppuccin Mocha): Connection screen = centered column on empty background:
  logo 60px + wordmark "rift" (mono, bold) + tagline (muted); card ~470px
  (popover bg ref #181825, border ref #45475a, radius 12, 24px padding),
  title "Connect to host" + `SSH` pill; labeled inputs 38px (bg ref #1e1e2e,
  border ref #45475a, radius 6, mono values, leading icons; focus = primary
  border): Host · User+Port row · SSH key (+ passphrase row when the key is
  encrypted) · Session (trailing muted "attach or create", caption right
  `tmux -CC -A` mono); full-width primary button "Connect →" 40px; below:
  "RECENT" eyebrow + rows (radius 8: host icon tile, host mono 13px, caption
  "user · session <name>" muted; right: `● live` success pill / relative time
  muted). Banners §7: danger banner (danger-tint bg, icon, 13/600 title +
  12 muted body) "SSH connection lost — reconnecting to <user@host> — retry N";
  info banner for "Daemon updated/restarted". Status dots: connected =
  success, reconnecting = warning, not connected = muted.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Version token = integer `PROTOCOL_VERSION`, strict equality, enforced by a pinned message-set fingerprint test | Simplest mechanism that makes skew impossible to ship silently; a build-hash adds machinery without covering more cases (both ends build from one repo). The daemon-scaffolding spec already deferred exactly this ("for now any version is accepted", lib.rs:1273-1276) | 2026-07-05 |
| Mismatch resolution is client-driven: pidfile stop → fingerprinted redeploy → respawn | All three halves already exist (#281 stop command, deploy.rs fnv marker, spawn path); the daemon never self-updates — one owner, no races. Zed's client-owned versioned server is the precedent (prior-art Phase 20 row) | 2026-07-05 |
| Daemon replies `Welcome{own version}` then closes on mismatch, instead of streaming or erroring mid-stream | Gives OLD clients (already shipped, e.g. the Jun-14 stable exe) an orderly early signal instead of the mid-stream codec death — the best achievable behavior toward binaries we cannot retrofit | 2026-07-05 |
| Resync = the existing Welcome snapshot replay + terminal re-Attach; no new sync protocol | The per-connection replay (#227, ordered by #425, lag-hardened by #426) and the fresh-LayoutSnapshot reset contract are exactly the resync semantics; a second path would violate the no-duplicate-mechanism rule | 2026-07-05 |
| Reconnect state surfaces through the existing `ConnectionStatus` channel | The enum + statusbar rendering exist (`Reconnecting` is a dead variant today); no parallel status mechanism | 2026-07-05 |
| Recents store lives beside the window-state store (same JSON pattern, per-channel) | Window-state persistence (phase 9) is the established local-store pattern; no new dependency | 2026-07-05 |
| The Connection screen is the startup state on every launch (no auto-connect) | Gate decision: explicit, visible connect step preferred over blind auto-connect; the design's §6 artboard is the startup state | 2026-07-05 |
| Reconnect: unlimited jittered capped backoff (30s cap) + Cancel → Connection screen; auth/config errors skip retries | Gate decision: a long outage self-heals; the user can always bail out; misconfiguration must not hide behind retries | 2026-07-05 |
| Passphrase-protected SSH keys are in scope (screen-prompted, never persisted) | Gate decision: the screen provides the natural prompt surface; closes the wave-1 log-only failure | 2026-07-05 |

## Prior art

- `docs/prior-art.md` → "v1.0 polish + robustness phases — prior-art index
  (Phases 19–26)", Phase 20 rows: Zed `crates/remote`/`remote_server`
  (client-owned versioned server binary, reconnect UX) — reference; design
  artboards Connection — Startup / alert banners as the UI contract.

## Human prerequisites

None — no new secrets or provisioning. (Passphrase-key support, if accepted
into scope, uses a key the developer already owns; nothing to deliver.)

## Tracking

- Milestone: created after this spec merges (phase 20).
- Issues: one per implementable step, each referencing this spec path.
  Phase 19's protocol issue (#465) gains a cross-milestone `Depends on:` edge
  to this phase's negotiation issue once it exists.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Fingerprint test: changing any protocol enum variant/field without
      bumping `PROTOCOL_VERSION` fails `cargo test -p rift-protocol`
- [ ] Handshake test: daemon rejects a mismatched Hello with `Welcome{own}` +
      clean close (transport roundtrip test)
- [ ] Behavioral (dev channel): start an OLD-protocol daemon (previous binary)
      → launching the app replaces and restarts it automatically; the reactive
      layer works immediately (no silent stream death)
- [ ] Behavioral: kill the daemon process mid-session → banner/status shows
      reconnecting; within seconds the stream is back and explorer/git/
      diagnostics converge without restart; terminal content intact (tmux
      persistence)
- [ ] Behavioral: drop SSH (kill sshd connection) → app does NOT quit; danger
      banner + Reconnecting dot; restoring the network restores the full
      cockpit
- [ ] Behavioral: every launch lands on the Connection screen with prefilled
      values; Enter/Connect reaches the cockpit; the recents list records the
      connection; canceling a reconnect returns to the screen
- [ ] Encrypted-key flow: connecting with a passphrase-protected key prompts
      once and succeeds; a wrong passphrase surfaces a field-level error, not
      a log line

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Restarting the shared daemon kills the OTHER channel's live instance (stable vs dev — wave-1 low finding) | Restart only on version mismatch (equal-version reattach never restarts); both channels rebuilt from develop converge on the same version; the orderly Welcome-close gives the older instance a clean early failure instead of a mid-session death |
| Reconnect loop masks a genuinely broken config (wrong key, host gone) | Distinguish transport drops (retry) from auth/config failures (straight to the Connection screen with the error surfaced) |
| Fingerprint test too brittle (false bumps) or too loose (misses changes) | Hash exactly the serde-visible surface: enum variant names + field names per message enum; unit tests cover both a variant addition and a field rename |
| Backoff interacting with keepalive/timeout constants causing thundering retries | Single reconnect engine owns all retries; constants documented next to it; jittered capped backoff |

## Decision log

- 2026-07-05: Spec drafted from wave-1 verified findings (stable-log smoking
  gun; dead `Reconnecting` variant; quit-on-disconnect paths) and the
  post-wave code state (#425 Welcome ordering, #426 lag resync, #438
  keepalive, #441 handshake timeout already merged — this spec builds on
  them instead of re-scoping them).
- 2026-07-05: Fresh-context review (PR #470): APPROVE; adopted the
  non-blocking findings — per-connection Welcome note, field types in the
  fingerprint, `terminal` crate in the SSH-drop scope, corrected
  `stop_daemon` citation, stale-comment cleanup folded into the client issue,
  architecture.md robustness contract authored in this PR.
- 2026-07-05: Spec-acceptance gate resolved the three open decisions —
  Connection screen on every launch (no auto-connect), unlimited capped
  reconnect with Cancel, passphrase keys in scope — and accepted the spec.
- 2026-07-05 (#475): The daemon-stream recovery engine bounds itself at 10
  attempts (~2 min under the capped schedule) and then fails the session
  visibly — the unlimited-retry + Cancel policy applies to the SSH-level loop
  (#476), which owns outages where SSH itself is down; a daemon that cannot be
  respawned within the window is a structural failure retrying cannot fix.
- 2026-07-05 (#475): A protocol version mismatch during mid-session reconnect
  aborts recovery with a session error instead of re-running the
  stop/redeploy/respawn replacement: mid-session, a mismatched daemon means a
  newer client replaced it, and replacing it back would kill that client's
  live session (stable/dev tug-of-war).
- 2026-07-05 (#475): The reverse-path bridges resolve the current client
  through a `tokio::sync::watch` handle per message (the recovery engine swaps
  the reconnected client in under them); sends during the gap are dropped, per
  the no-buffering constraint — the Welcome replay + re-Attach resync replaces
  the state they would have touched. The jittered capped backoff lives in
  `rift_ssh::ReconnectBackoff`, ready for reuse by the #476 engine.
- 2026-07-05 (#475, post-#509 rebase): The recovery's re-Attach targets the
  currently attached session, not the startup one — a cockpit switch (#509)
  moves the client mid-session, and re-attaching the original would be a
  silent regression. The session-switch bridge records each sent switch on a
  `watch` channel the recovery resolves per attempt; a switch dropped during
  an outage stays untracked (dropped, never buffered). The re-Attach is also
  followed by a `ResizePane` re-asserting the last known client grid (cached
  by the resize bridge), mirroring the switch's viewport re-assert — the
  fresh tmux child spawns unsized and the render layer only re-sends on a
  size change, so without it the terminal would stay reflowed to the 80x24
  default until a manual resize.
- 2026-07-05 (#476): The SSH-level engine classifies failures by allowlist:
  only an error chain carrying a retryable `SshError` (transport-shaped)
  re-enters the loop; auth/key/host-key failures and typeless session errors
  — notably the mid-session protocol mismatch, whose automatic re-run would
  re-enter the replacement tug-of-war #475 cut off — end in the visible
  `Disconnected` state. An orderly tmux exit (`TerminalExit`) also ends
  without retrying: the session is gone on purpose, not lost. The daemon
  recovery's give-up carries its last transport error in the chain so the
  engine can classify it.
- 2026-07-05 (#476): Each connect attempt runs on a fresh tokio runtime —
  dropping it cancels the dead session's bridge tasks, which would otherwise
  compete with the fresh session's bridges for the shared flume receivers
  (MPMC). The daemon recovery aborts early when the SSH transport is closed
  (`SshConnection::is_closed`), handing an SSH drop to the SSH-level loop
  (and its banner) within the keepalive detection bound instead of burning
  its ~2-minute attempt window first.
- 2026-07-05 (#476): The danger banner renders across the top of the
  terminal panel (`SessionView`), where the connection state, `user@host`
  label, and the Cancel channel already live; the status dot colors moved to
  theme tokens (connected = success, connecting/reconnecting = warning, not
  connected = muted). Until the Connection screen (#477) lands, a canceled
  reconnect, an orderly exit, and non-retryable failures all surface as the
  muted `disconnected` statusbar state; the legacy tmux escape hatch keeps
  its pre-existing behavior (no reconnect loop) pending its removal (#285).
- 2026-07-05 (#476, review): The engine drops the render-side backlog
  immediately before every connect attempt — its per-attempt runtime drop
  cancels the dead session's bridge consumers, so the shared flume queues
  would otherwise buffer the whole outage and replay it into the fresh attach
  (stale keystrokes, pane commands, editor requests landing minutes later),
  violating the no-buffering constraint. The resync replaces the dropped
  state; the one latest-value signal kept is the client grid, folded into an
  engine-scope viewport watch that every fresh attach re-asserts (the render
  layer only re-sends on a size change, which a reconnect is not).
  Current-session tracking likewise moved to engine scope, for the same
  reason the daemon recovery already tracks it (post-#509 decision above):
  the watch is seeded from `RIFT_SESSION` once and updated by the switch
  bridge across attempts, so an SSH-level reconnect re-attaches the session
  the user is actually on, never silently the startup one.
