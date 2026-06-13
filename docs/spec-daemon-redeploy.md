# Spec: Daemon re-deploy on a same-version binary change

> Status: DRAFT
> Created: 2026-06-13
> Completed: —

Make a rebuilt, same-version daemon binary actually take effect on the dev
channel after an app relaunch — without any manual step — by fixing the three
seams that today silently keep the stale daemon. The graduated successor to the
reverted papercut #268 (PR #271, reverted by #276), which addressed only the
deploy *decision* and broke on the upload itself.

## Background — why #268 was reverted

`#268` made the deploy decision content-aware (re-upload when a fingerprint
differs) but left two seams unaddressed, so it failed live and was reverted:

1. **Decision** (was fixed): `ensure_daemon_deployed` keyed purely on the
   versioned filename `rift-daemon-<version>`, so a same-version rebuild was
   never re-uploaded.
2. **Upload** (broke): `upload_executable` writes in place
   (`cat > '<path>' && chmod +x`). A running daemon holds that exact binary, so
   the re-upload fails with `ETXTBSY` ("Text file busy") — the open() is rejected
   before truncation (no corruption), `ensure_daemon_deployed` errors, and
   `provision_daemon` aborts. Confirmed live: `daemon auto-deploy failed …
   remote command exited with status 1`.
3. **Restart** (never addressed): even after a successful replace, the running
   daemon keeps executing the old inode, and the reattach contract (#62, shared
   version-keyed socket) reattaches to it instead of spawning the new binary — so
   the new code never runs until the daemon is killed.

All three must be handled for the dev-loop goal to hold end-to-end.

## Outcome

Design-neutral (the mechanism is the open decision below); these state the end
behaviour, verified live on the dev channel:

- [ ] Editing daemon code, rebuilding, and relaunching the dev channel makes the
      remote daemon run the **new** code — no manual binary removal, no manual
      daemon kill. The startup log / behaviour reflects the change.
- [ ] The re-deploy never fails when a daemon is already running the binary (no
      `ETXTBSY`); it degrades-and-logs on real errors per the best-effort
      `provision_daemon` contract, never aborts the tmux flow.
- [ ] An unchanged relaunch does not redundantly re-upload or needlessly bounce a
      healthy daemon.
- [ ] The released path is unaffected: `just promote` / a version bump still
      deploys and runs, and the stable channel keeps working.
- [ ] No unbounded accumulation of stale daemons/sockets/binaries on the host
      (whatever the chosen design leaves behind is bounded and cleaned up).

## Scope

### In scope

- `crates/ssh` deploy + launch path (`deploy.rs`, `launch.rs`, the
  `upload_executable`/`cat_to_executable_command` seam in `connection.rs`) and
  `crates/app` `provision_daemon`, so a changed same-version daemon is deployed
  **and** run.
- A content-aware deploy decision (re-introduce the fingerprint approach from the
  reverted #268 — see Prior decisions).
- Whatever daemon-lifecycle handling the chosen design needs (see the open
  decision): atomic replacement and/or content-keyed identity and/or a
  stop/respawn step, plus bounded cleanup of anything orphaned.

### Out of scope

- The daemon's *watched root* and any feature behaviour — covered by
  `archive/spec-daemon-project-root.md`; this spec only changes how a new daemon
  binary reaches and runs on the host.
- Multi-project / per-worktree daemons (deferred, Scenario 2).
- A general remote process manager. Only the rift-daemon lifecycle is in scope.
- Hot-reloading a running daemon in place (the daemon is a black-box restart, not
  a reload).

## Constraints

- Best-effort: `provision_daemon` swallows every error so the tmux flow survives
  without the daemon (`crates/app/src/main.rs`); a deploy/restart failure must
  degrade and log, never abort startup. Binaries use `anyhow`; libs degrade and
  log (`docs/constitution.md`).
- A running executable cannot be written in place on Linux (`ETXTBSY`); any
  replacement of a possibly-running binary must avoid an in-place `cat >` of the
  live path (e.g. rename-into-place, or a distinct path). This is the seam that
  broke #268.
- The reattachable single-instance daemon (one detached process per host+version,
  shared version-keyed socket, #62) is the transport contract. The dogfooding
  setup runs **one** daemon for both the stable and dev views
  (`docs/spec-dogfooding-channels.md`) — so any "restart the daemon" step bounces
  *both* channels; the chosen design must state how it handles that.
- Remote paths/values entering a shell command are single-quoted via
  `shell_single_quote`, with a matching unit test, exactly as the existing
  deploy/launch commands are.
- `cargo deny check licenses` must pass; prefer a dependency-free implementation
  (the reverted fingerprint used a hand-rolled FNV-1a — no new crate).

## Open decisions — resolved at the spec-acceptance gate

| Decision | Options | Status |
|---|---|---|
| **Re-deploy + restart strategy** — how a changed same-version binary is replaced and made to run, given the shared running daemon | **Family A — replace + restart:** content-fingerprint decision + atomic upload (write `<path>.tmp`, `chmod +x`, `mv -f` over the live path — rename succeeds while the old inode keeps running) + an explicit daemon stop/respawn when the fingerprint changed (needs a way to stop the remote daemon: pidfile, `pkill -f <socket>`, or a protocol shutdown message). **Family B — content-keyed identity:** name the binary *and* socket by `<version>-<fingerprint>`, so a changed binary is simply a new, absent path → the existing deploy+spawn path runs unchanged (no in-place write, no kill), with bounded cleanup of stale same-version daemons/sockets on launch. **Family C — dev-only:** apply A or B only on the dev channel (needs a reliable dev signal — `debug_assertions` is true for stable too, so this requires an explicit env flag from the dev recipes), leaving stable on today's exact path. | OPEN |

## Human prerequisites

- [ ] Confirm the acceptable behaviour for the **shared dogfooding daemon** when a
      dev rebuild triggers a restart: a brief bounce of the stable channel's view
      is acceptable, or the restart must be scoped to the dev channel (drives the
      A/B-vs-C part of the open decision). No secrets or external provisioning are
      needed; the dev/stable host is already configured.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Re-introduce the content-aware deploy *decision* from #268 (compare a dependency-free FNV-1a fingerprint of the local binary against a remote marker) | The decision logic was sound and approved in #271's review; only the upload mechanism (in-place `cat >`) and the missing restart broke it. The fingerprint is hand-rolled (no dependency) and deterministic across toolchains. | 2026-06-13 |
| Avoid any in-place overwrite of the live binary path | Linux `ETXTBSY` — the concrete failure that reverted #268. Both candidate families honour this (A via rename-into-place, B via a distinct content-keyed path). | 2026-06-13 |
| Keep the change in `crates/ssh` + `provision_daemon`; no new crate, no protocol change unless the chosen family needs a daemon-shutdown message (then it is a deliberate `protocol` addition) | Respect crate boundaries; `protocol` is a deliberate API change (`CLAUDE.md`). | 2026-06-13 |

## Tracking

The decomposition into steps lives as GitHub issues, one per step, grouped under
a milestone. This spec owns the design; the issues own progress. Do not duplicate
the step list here.

- Milestone: created once this spec is `READY`
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path. A `feat:`/`fix:` PR may only merge if it
closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes (`just ci`, workspace excluding `rift-app`; `app-check` covers the app
      in CI).
- [ ] Unit: the deploy decision re-uploads on fingerprint mismatch and skips on
      match; the replace/identity command is injection-safe (single-quoted),
      mirroring the existing deploy/launch command tests.
- [ ] **Behavioural, the exact #268 failure (human QA gate, dev channel):** edit
      daemon code (e.g. a distinctive startup-log string), `just dev-windows`, and
      confirm the daemon log shows the new string and the app applies the new
      daemon's stream — with **no** manual binary removal and **no** `ETXTBSY` /
      `auto-deploy failed` in the app log.
- [ ] Behavioural: an unchanged relaunch neither re-uploads nor bounces the daemon
      (log shows skip).
- [ ] Behavioural, released path (human QA gate): `just promote` still deploys and
      the stable instance runs; the shared-daemon behaviour matches the resolved
      open decision.
- [ ] No stale daemon/socket/binary accumulation after several dev rebuilds
      (bounded / cleaned up).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Restarting the shared daemon bounces the other dogfooding channel | Resolved by the open decision (scope of restart); the QA gate checks both channels. |
| Family B orphans a daemon per dev rebuild | Bounded cleanup of stale `rift-daemon-<version>-*` daemons/sockets on launch is part of the chosen design and a verification item. |
| A daemon-shutdown protocol message widens the protocol surface | Only added if Family A is chosen and a pidfile/`pkill` is rejected; it is then a deliberate, tested `protocol` addition. |
| Killing a daemon mid-stream drops an in-flight client | The client already tolerates daemon loss (best-effort reattach, #62); the restart path reconnects after respawn. |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work
progresses.

- 2026-06-13: Spec drafted after live QA of #268 (PR #271) revealed the `ETXTBSY`
  upload failure and the unaddressed restart, and #268 was reverted (#276). The
  re-deploy+restart strategy (one of three families) is the single genuinely-open
  decision, carried to the spec-acceptance gate.
