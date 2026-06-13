# Spec: Daemon project root (watch the project, not the SSH login dir)

> Status: READY
> Created: 2026-06-13

Give the remote daemon an explicit, configurable project root so it watches the
actual project checkout instead of the SSH login directory (`$HOME`), making the
Phase 3 data layer (file tree, git status, soon LSP) useful on the live
dogfooding channels. Closes the follow-up flagged by the file-tree and
git-status specs (issue #242).

## Outcome

- [ ] When a project root is configured, the daemon scans and watches that
      directory; on the dev channel `rift-daemon listening …, worktree <repo>`
      names the project checkout, and git status streams (branch + entries) —
      the `worktree /home/developer … worktree-only` symptom from #242 is gone.
- [ ] When no project root is configured, the daemon falls back to its launch
      directory (current behavior) — the daemon never fails to start over a
      missing or bad root; failures degrade and log, the tmux flow is untouched.
- [ ] Both dogfooding channels watch the project: the dev channel via its run
      configuration and the stable channel via `just promote`'s baked default,
      so file tree and git status are populated live on each.
- [ ] The configured root is passed injection-safe (single-quoted) through the
      detached launch command, consistent with the existing socket/log/binary
      path handling.

## Scope

### In scope

- A remote project-root knob on the app side (`RIFT_PROJECT_ROOT`), read next to
  the existing `RIFT_SSH_*` / `RIFT_SESSION` config and threaded through
  `provision_daemon` → `connect_or_spawn_daemon` → the launch command.
- `crates/ssh/src/launch.rs`: the detached `--serve-uds` launch command appends
  a single-quoted `--root <path>` to the inner daemon invocation when a root is
  configured (positioned before the `</dev/null >> log 2>&1` redirections), and
  omits the flag entirely when unset.
- `crates/daemon/src/main.rs`: grow the argument parser — after
  `--serve-uds <sock>`, parse an optional `--root <path>` named flag; source the
  watched root from it, falling back to `current_dir()` when the flag is absent
  (same parity in the stdio `None` arm). The `serve`/`serve_uds` signatures
  already take the watched root as `Option<PathBuf>` (#110); this changes only
  the binary's arg parsing and where the value comes from, not those signatures.
- Dogfooding wiring so the symptom is actually gone on both channels: the dev
  recipes (`dev` / `dev-watch` / `_launch-windows`) set `RIFT_PROJECT_ROOT` and
  add it to the `_launch-windows` `WSLENV` export list so it crosses to the
  native exe; `just promote` exports a `RIFT_DEFAULT_PROJECT_ROOT` bake var read
  via `option_env!` for the stable default (runtime `RIFT_PROJECT_ROOT` still
  wins), mirroring exactly how `RIFT_DEFAULT_SSH_KEY` is handled today.
- The socket-identity behavior under reattach (see Prior decisions — the one
  point resolved at the gate).

### Out of scope

- The tmux session's working directory (the agent's cwd). #242 is specifically
  the daemon's watched root; the agent pane cwd is a separate concern.
- Multi-root / per-worktree explorer contexts (`vision.md` Scenario 2) — already
  deferred by the file-tree and git-status specs; this stays single-root.
- A GUI affordance to pick or switch the project at runtime — configuration via
  env / baked default only for now.
- LSP root selection (Phase 3.4 is not built yet). The project root chosen here
  is the value LSP will later consume; no LSP code is touched.

## Constraints

- The daemon is best-effort: `provision_daemon` swallows every error so the tmux
  flow keeps working without the daemon (`crates/app/src/main.rs`). A missing or
  invalid root must degrade and log, never abort startup. Per
  `docs/constitution.md` (binaries use `anyhow`; libs degrade and log).
- An invalid root already degrades gracefully downstream: a non-repo root →
  worktree-only (tree, no git status); a scan/watch failure → "no worktree"
  (`crates/daemon/src/lib.rs`). Reuse this; do not add a refuse-to-start path.
- Remote paths are single-quoted via `shell_single_quote` before entering a
  command line (`crates/ssh/src/launch.rs`); the root path follows the same
  injection-safe handling, with a matching unit test.
- The reattachable single-instance daemon (one detached process per host+version,
  shared socket, #62) is the transport contract; the dogfooding setup runs one
  daemon for both the stable and dev views (`docs/spec-dogfooding-channels.md`).
- Single watched root; multi-root is deferred (inherited from the file-tree and
  git-status specs; `docs/constitution.md`: no premature abstraction).

## Human prerequisites

- [x] Remote project-root path confirmed: `/home/developer/CascadeProjects/rift`
      (the rift checkout on the SSH host `127.0.0.1` / `developer`). Backs both
      `RIFT_PROJECT_ROOT` (dev) and the `RIFT_DEFAULT_PROJECT_ROOT` bake (stable).
- [ ] After the first deploy of this change, kill any stale daemon already
      running on the SSH host (rooted at `$HOME`) once, so the next launch
      re-roots to the project. One-time migration — the shared daemon binds its
      root at first spawn (see Prior decisions). A QA-gate step covers this.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Transport the root via an explicit `--root <path>` named flag on the launch command (positioned before the shell redirections), not a `cd` in the shell or a third positional argument | `serve`/`serve_uds` already take the root as an explicit `Option<PathBuf>` (#110); `launch.rs` is unit-tested at the command-construction level; a named flag is unambiguous to parse, more testable than relying on shell cwd, and avoids a second `$HOME`/relative-path resolution. A flag placed after a redirection would be swallowed by the shell — hence the ordering. | 2026-06-13 |
| Knob name `RIFT_PROJECT_ROOT` (runtime), `RIFT_DEFAULT_PROJECT_ROOT` (compile-time bake) | Follows the established `RIFT_*` / `RIFT_DEFAULT_*` split already used for the SSH key (`RIFT_SSH_KEY` / `RIFT_DEFAULT_SSH_KEY`); the app centralizes its remote config beside `RIFT_SSH_*` / `RIFT_SESSION`. | 2026-06-13 |
| Absent knob → fall back to the daemon launch directory (today's behavior) | Preserves the best-effort contract: the daemon must start and serve even with no project configured; absent ≠ error. | 2026-06-13 |
| Invalid/nonexistent root → degrade + log (worktree-only or empty), never refuse to start | Reuses the existing graceful-degradation path (`lib.rs`); consistent with the daemon being a best-effort side channel. | 2026-06-13 |
| Stable channel gets the root from a `just promote` compile-time bake (runtime `RIFT_PROJECT_ROOT` overrides) | Mirrors the existing `RIFT_SSH_KEY` bake-via-promote pattern (`docs/spec-dogfooding-channels.md`); stable has no console/env to set a runtime var. | 2026-06-13 |
| Socket identity under reattach: keep the shared version-keyed socket (`rift-daemon-<version>.sock`, unchanged) and bind the watched root at first spawn; `provision_daemon`'s socket-path formula is untouched | Minimal, consistent with the constitution (no premature abstraction) and the deferred multi-root scope. The reattach-ignores-new-root limitation only bites with multiple projects on one host (Scenario 2, deferred); for the single-project dogfooding setup it is a one-time migration. Project-keyed sockets were considered and rejected as premature for single-root. *Resolved at the spec-acceptance gate, 2026-06-13.* | 2026-06-13 |
| Dogfooding project root = `/home/developer/CascadeProjects/rift` for both channels | The SSH host is `127.0.0.1` / `developer` (localhost); the rift checkout lives there. Backs `RIFT_PROJECT_ROOT` (dev) and the `RIFT_DEFAULT_PROJECT_ROOT` bake (stable). *Confirmed at the spec-acceptance gate, 2026-06-13.* | 2026-06-13 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file — one
issue per step, grouped under a milestone. This spec owns the design; the issues
own progress.

- Milestone: created once this spec is `READY`
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path in its body.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`).
- [ ] Unit (`launch.rs`): when a root is configured the inner daemon command
      contains `--root '<path>'` and the flag appears before the `</dev/null`
      redirection (a flag after a redirection would be swallowed); when no root
      is configured the flag is absent entirely. A root path containing shell
      metacharacters stays single-quoted inside the `setsid sh -c` inner command
      (and is therefore doubly-escaped in the final string), mirroring
      `test_launch_command_neutralizes_injection`.
- [ ] Unit (`crates/daemon`): `--serve-uds <sock> --root <path>` resolves the
      watched root to the flag argument; with no `--root`, the root falls back to
      `current_dir()`.
- [ ] Behavioral, live dev channel (human QA gate): with the knob set to the
      rift checkout, the daemon log names `worktree <repo>` and git status
      streams (branch + entries); the #242 `$HOME` / `worktree-only` symptom is
      gone.
- [ ] Behavioral, knob unset (human QA gate): daemon behavior is unchanged —
      watches the launch dir, no regression to the existing flow.
- [ ] Behavioral, stable channel (human QA gate): after `just promote`, the
      stable instance watches the rift checkout — file tree and git status are
      populated.
- [ ] Migration (human QA gate): the stale `$HOME`-rooted daemon was killed once
      after the first deploy; the next launch re-rooted to the project (verifying
      the bind-at-first-spawn behavior).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Reattach binds the root at first spawn (chosen design): a stale `$HOME`-rooted daemon keeps serving `$HOME` after the knob is added, until killed | One-time QA migration step kills the stale daemon after the first deploy (Verification + Human prerequisites); the bind-at-spawn behavior is documented in Prior decisions. |
| Typo'd or nonexistent root path | Daemon degrades to worktree-only/empty and logs it (existing path); diagnosable in the daemon log — no crash. |
| Path with spaces / non-ASCII / shell metacharacters | `shell_single_quote` already neutralizes these; covered by a dedicated injection unit test mirroring the existing ones. |
| Two app instances pointed at different projects on one host | Out of scope (multi-root deferred, Scenario 2); the shared-daemon limitation is documented in Prior decisions. A future multi-root phase would project-key the socket. |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work
progresses.

- 2026-06-13: Spec drafted from issue #242, the follow-up deferred by the
  file-tree (`archive/spec-daemon-filetree.md`) and git-status
  (`archive/spec-daemon-git-status.md`) specs. One genuinely-open decision
  (socket identity under reattach) carried to the spec-acceptance gate.
- 2026-06-13: Agent spec review (VERDICT NEEDS CHANGES → addressed): made the
  `main.rs` arg-parser change explicit (named `--root <path>` flag, parsed after
  `--serve-uds <sock>`, before redirections); sharpened the `launch.rs` injection
  test requirement (root single-quoted inside the `setsid sh -c` inner command);
  named the bake var `RIFT_DEFAULT_PROJECT_ROOT` and the dev `WSLENV` addition.
- 2026-06-13: Spec-acceptance gate. Resolved the open decision — **shared
  version-keyed socket, watched root bound at first spawn** (option i; minimal,
  multi-root deferred); the stale-`$HOME`-daemon kill is a one-time migration
  step. Confirmed the dogfooding project root
  `/home/developer/CascadeProjects/rift` for both channels. Flipped
  `DRAFT` → `READY` in the same PR.
