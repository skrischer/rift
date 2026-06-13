# Spec: Daemon project root (watch the project, not the SSH login dir)

> Status: DRAFT
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
- `crates/ssh/src/launch.rs`: the detached `--serve-uds` launch command carries
  the configured root to the daemon (single-quoted), and omits it when unset.
- `crates/daemon`: parse the root argument in `--serve-uds` (and the stdio
  `None` arm for parity); source the watched root from it, falling back to
  `current_dir()` when absent. The watched root is already an explicit
  `Option<PathBuf>` on `serve`/`serve_uds` (#110) — this only changes where the
  value comes from, not the signatures' contract.
- Dogfooding wiring so the symptom is actually gone on both channels: the dev
  run configuration sets the knob; `just promote` bakes a compile-time default
  for stable (runtime override still wins), mirroring how `RIFT_SSH_KEY` is
  handled today.
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

- [ ] Confirm the remote project-root path for the dogfooding channels — the
      rift checkout on the SSH host (host `127.0.0.1`, user `developer`):
      `/home/developer/CascadeProjects/rift`. This value backs both the dev run
      configuration and the stable `just promote` baked default.
- [ ] Accept the reattach behavior chosen at the gate: with a single shared
      daemon, changing the watched project requires killing the running daemon
      once so the next launch re-roots (see Prior decisions). If the
      project-keyed-socket option is chosen instead, this prerequisite drops.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Transport the root via an explicit `--root <path>` argument on the launch command, not a `cd` in the shell | `serve`/`serve_uds` already take the root as an explicit `Option<PathBuf>` (#110); `launch.rs` is unit-tested at the command-construction level; an explicit, quoted argument is more testable than relying on shell cwd, and avoids a second `$HOME`/relative-path resolution. | 2026-06-13 |
| Knob name `RIFT_PROJECT_ROOT`, read on the app side beside `RIFT_SSH_*` / `RIFT_SESSION` | Follows the established `RIFT_*` env convention; the app already centralizes its remote config there. | 2026-06-13 |
| Absent knob → fall back to the daemon launch directory (today's behavior) | Preserves the best-effort contract: the daemon must start and serve even with no project configured; absent ≠ error. | 2026-06-13 |
| Invalid/nonexistent root → degrade + log (worktree-only or empty), never refuse to start | Reuses the existing graceful-degradation path (`lib.rs`); consistent with the daemon being a best-effort side channel. | 2026-06-13 |
| Stable channel gets the root from a `just promote` compile-time bake (runtime `RIFT_PROJECT_ROOT` overrides) | Mirrors the existing `RIFT_SSH_KEY` bake-via-promote pattern (`docs/spec-dogfooding-channels.md`); stable has no console/env to set a runtime var. | 2026-06-13 |
| OPEN — socket identity under reattach: (i) keep the shared version-keyed socket and bind the watched root at first spawn (minimal; reattach ignores a newly-configured root until the daemon is killed), or (ii) project-key the socket (`rift-daemon-<version>-<hash(root)>.sock`) so each distinct root gets its own daemon (handles project switching + forward-compatible with multi-root, slightly more code) | resolved at the spec-acceptance gate | — |

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
- [ ] Unit (`launch.rs`): the launch command includes `--root '<path>'` when a
      root is configured and omits it when not; a root path containing shell
      metacharacters stays single-quoted (mirrors the existing injection tests).
- [ ] Unit (`crates/daemon`): `--serve-uds <sock> --root <path>` resolves the
      watched root to the argument; with no `--root`, the root falls back to
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

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Reattach binds the root at first spawn (option i): a stale `$HOME`-rooted daemon keeps serving `$HOME` after the knob is added, until killed | QA step kills any stale daemon once after the first deploy; document the bind-at-spawn behavior in the human prerequisites. Dissolves entirely if option (ii) is chosen at the gate. |
| Typo'd or nonexistent root path | Daemon degrades to worktree-only/empty and logs it (existing path); diagnosable in the daemon log — no crash. |
| Path with spaces / non-ASCII / shell metacharacters | `shell_single_quote` already neutralizes these; covered by a dedicated injection unit test mirroring the existing ones. |
| Two app instances pointed at different projects on one host (option i) | Out of scope (multi-root deferred, Scenario 2); the shared-daemon limitation is documented. Option (ii) would remove it. |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work
progresses.

- 2026-06-13: Spec drafted from issue #242, the follow-up deferred by the
  file-tree (`archive/spec-daemon-filetree.md`) and git-status
  (`archive/spec-daemon-git-status.md`) specs. One genuinely-open decision
  (socket identity under reattach) carried to the spec-acceptance gate.
