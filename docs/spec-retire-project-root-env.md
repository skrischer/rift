# Spec: Retire the RIFT_PROJECT_ROOT env root — daemon follows the session @root

> Status: DRAFT
> Created: 2026-07-10
> Completed: —

Remove the single baked project root (`RIFT_PROJECT_ROOT` runtime /
`RIFT_DEFAULT_PROJECT_ROOT` compile-time bake) that seeds the daemon's watched
directory at spawn, now that the per-session `@root` substrate (Phases 34–36,
merged) makes the reactive layer follow the active session. The daemon starts
**root-less** and derives its watched root only from the session it attaches —
`@root` for rift-created sessions, the tmux `session_path` for sessions created
outside rift. Mirrors Phase 38 (retire the fixed `RIFT_SESSION` default): remove
the last baked global launch default that bypasses the connect-and-list / remote
root-picker model.

## Outcome

- [ ] No launch path sets `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT`: the
      justfile drops the `RIFT_PROJECT_ROOT := env(…, "/home/developer/CascadeProjects/rift")`
      default (`justfile:293`), its three exports (`:348/360/391`), the `WSLENV`
      entry (`:384`), and the `promote` bake (`:448-462`), so no WSL/host path can
      leak into a remote session's root.
- [ ] The daemon starts **root-less**: `--serve-uds` no longer requires `--root`;
      a daemon spawned without a root serves with no initial watched directory and
      the reactive layer (file tree / git / diagnostics) stays empty until the
      first session attach re-roots it via `@root` / `session_path` (Phases 34–35,
      shipped).
- [ ] The app stops resolving and passing a project root: the
      `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT` resolution
      (`crates/app/src/main.rs:2714`) is removed and the `root` parameter is
      dropped from `connect_or_spawn_daemon` / `launch_command`
      (compiler-enforced — no caller silently keeps passing one).
- [ ] Connecting to a host/container with no matching session opens the remote
      root picker (Phase 36) and the picked root is the only thing that binds the
      daemon's watched dir — a path that does not exist on the target can no
      longer be stamped into `@root` from a stale launch env.
- [ ] A session created outside rift (`tmux new -s main`, no `-c`) is watched at
      its own `session_path`, not a baked global root; `resolve_session_root`
      already provides this fallback.
- [ ] No protocol / daemon-message change; `PROTOCOL_VERSION` unchanged (the
      `@root` substrate is already in the protocol).

## Scope

### In scope

- **justfile**: drop the `RIFT_PROJECT_ROOT` default variable (`:293`), its three
  exports into the launched app (`:348/360/391`), the
  `WSLENV` `RIFT_PROJECT_ROOT/p` entry (`:384`), and the
  `RIFT_DEFAULT_PROJECT_ROOT` compile-time bake in `promote` (`:448-462`).
- **`crates/daemon` (`main.rs`)**: `--serve-uds` accepts an absent `--root`; pass
  the `Option` root straight to `serve_uds` (which already takes
  `Option<PathBuf>`, `lib.rs:1955`) instead of `watched_root(root_flag)?`. Log
  "no initial root — awaiting first attach" when `None`. `serve` / `serve_uds` /
  the internal `self_root` already model a `None` root end-to-end.
- **`crates/ssh` (`launch.rs`)**: drop the `root: Option<&str>` parameter from
  `connect_or_spawn_daemon` (`:156`) and `launch_command` (`:61`), the `--root`
  wrapping (`:70`), and its unit tests (`:237-282`) — the daemon is always
  spawned without `--root` from the app.
- **`crates/app` (`main.rs`)**: remove the `RIFT_PROJECT_ROOT` /
  `RIFT_DEFAULT_PROJECT_ROOT` resolution (`:2714`) and its doc comment; stop
  threading a root through `connect_or_spawn_daemon`; drop `DaemonEndpoint`'s
  `project_root` field if it has no remaining reader.
- **Docs**: `CLAUDE.md` / `AGENTS.md` (the dogfooding-channels and remote-exec-
  wrapper notes that instruct setting `RIFT_PROJECT_ROOT=/workspace`) updated to
  "pick the project root in the root picker on connect"; `docs/roadmap.md`'s
  Phase 34–36 narrative note about the baked single root (~`L211-214`) updated.
  `docs/archive/spec-remote-exec-wrapper.md` is a historical record —
  decision-log-superseded here, not edited.

### Out of scope

- The per-session `@root` mechanism, the remote root picker, and re-root-on-
  attach — already shipped (Phases 34–36); this only removes the baked global
  seed that bypassed them.
- `RIFT_SESSION` — retired separately in Phase 38.
- The other launch env knobs (SSH host / user / port / key, daemon binary, exec
  wrapper) — they stay env-configured with working defaults.
- Any protocol / daemon-message change; any new UI surface.
- The daemon's bare stdio mode (`serve` without `--serve-uds`): unused by every
  launch path and deliberately still erroring on a missing root
  (`main.rs:68`) — left as-is.

## Constraints

- **The per-session root substrate is shipped and is the replacement.** `@root`
  is stamped (`crates/daemon/src/terminal.rs:452`) and queried (`ROOT_QUERY`,
  `terminal.rs:104`), `resolve_session_root` prefers `@root` and falls back to
  `session_path` (`terminal.rs:1175`), and each connection re-roots on attach
  (`reroot_connection`, `crates/daemon/src/lib.rs:1331`). This phase removes the
  now-redundant global seed; it does not build root resolution.
- The daemon already accepts `Option<PathBuf>` for its watched root end-to-end
  (`serve` / `serve_uds`, `self_root` starts `None`); only `watched_root`
  (`main.rs:158`) forces a root, and only on the launch path this phase changes.
- **#502 (no launch-dir fallback) must hold**: an absent root means "watch
  nothing until an attach re-roots," never "watch `$HOME`." Root-less start
  satisfies this — the watcher binds nothing until a session supplies a root, so
  the "silently watch `$HOME` over SSH" failure #502 fixed cannot reappear.
- **Agnostic direction (`docs/vision.md`)**: a baked project-root default is a
  personal-tool artifact; v1 is host-agnostic — the root is a property of the
  session / target, chosen on connect, never a client-side default.
- **Coupled change**: removing the justfile default alone would make the app pass
  no root and the daemon refuse to start (today's `watched_root(None)` →
  tmux-only fallback). The daemon must become root-less in the same phase — the
  justfile, app, ssh, and daemon changes land together.

## Prior art

- **Session ↔ project root coupling — prior-art index (Phases 34–36)** in
  `docs/prior-art.md` — the `@root` substrate + remote root picker this makes the
  sole root source.
- Phase 38 [spec-retire-fixed-session.md](spec-retire-fixed-session.md) — the
  direct in-repo precedent: retire a baked launch default (`RIFT_SESSION`) in
  favour of the shipped connect-and-list model. This phase does the same for the
  project root — the sibling knob Phase 38 explicitly scoped out ("`RIFT_PROJECT_ROOT`
  / `RIFT_DEFAULT_PROJECT_ROOT` — the daemon's fallback watched root … orthogonal
  to the session name, unchanged here").
- `docs/prior-art.md` Category 8 (Remote Development & SSH, Zed `crates/remote`) —
  the daemon owns the target filesystem and derives its context from the
  connection / session, not a client-baked path.

## Human prerequisites

- none — code + justfile + docs only; no secret, provisioning, or account is
  required to build or test this.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The daemon starts **root-less**; its watched root comes only from the session on attach (`@root`, or `session_path` for externally-created sessions) | The per-session substrate (Phases 34–35) already re-roots every connection on attach, so a baked spawn root is redundant — and it is exactly what leaked a WSL path into the container. `serve` / `serve_uds` already accept `Option`. | 2026-07-10 |
| The change is **coupled** across justfile + app + ssh + daemon, landing in one milestone | Removing the justfile default without the root-less daemon would break the daemon start (no root → refuse to start → tmux-only). | 2026-07-10 |
| #502's no-`$HOME`-fallback guard is **kept**: absent root = watch nothing, not the launch dir | Root-less start binds no watcher until a session supplies a root; the "silently watch `$HOME`" failure #502 fixed cannot reappear. | 2026-07-10 |
| Externally-created sessions (no `@root`) are watched at their `session_path` | `resolve_session_root` already falls back to `session_path`; the session's own cwd is more correct than a baked global path and strands nothing. | 2026-07-10 |
| OPEN — remove `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT` **entirely** vs keep them as an **unset optional override** for no-`@root` sessions | resolved at the spec-acceptance gate | — |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file — one
issue per implementable step, grouped under the milestone. This spec owns the
design; the issues own progress.

- Milestone: created at the spec-acceptance gate.
- Issues: created from this spec after merge (one per implementable step).

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`.
- [ ] Recipe inspection: `just dev-windows`, `just promote`, `just stable` no
      longer reference or export `RIFT_PROJECT_ROOT`, and `promote` bakes no
      `RIFT_DEFAULT_PROJECT_ROOT`.
- [ ] Unit / build: `connect_or_spawn_daemon` / `launch_command` no longer take a
      `root` parameter (compile-checked); the `--root`-wrapping launch tests are
      removed; `parse_serve_uds_args` still parses `--root` (manual affordance)
      but `--serve-uds` no longer errors when it is absent (new test: absent root
      → daemon serves `None`).
- [ ] Behavioural (dev-channel QA): connecting to the empty devenv container with
      **no** `RIFT_PROJECT_ROOT` set deploys the daemon, opens the remote root
      picker (no session), and picking `/workspace` watches `/workspace` — the
      WSL-path leak that produced a blank terminal + dead explorer no longer
      occurs.
- [ ] Behavioural: connecting to a host that has a rift-created session re-roots
      the reactive layer to that session's `@root` (unchanged from Phase 35); a
      session created via `tmux new -s x` outside rift is watched at its
      `session_path`.
- [ ] Docs: no `CLAUDE.md` / `AGENTS.md` instruction tells the operator to set
      `RIFT_PROJECT_ROOT=/workspace`; the container workflow says "pick the root
      on connect".

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The daemon watches nothing right after connect (no session yet) | Correct end state — the reactive layer is empty until a session / root is picked (Phase 36 opens the picker on zero sessions); the first attach re-roots it. |
| The stable channel baked its root via `RIFT_DEFAULT_PROJECT_ROOT` | With the bake removed, stable picks its root on first connect (Phase 36) and reattaches via recents (Phase 38) — consistent with the connect-and-list direction; no baked root to go stale. |
| A muscle-memory `RIFT_PROJECT_ROOT` in a shell profile | Removed entirely → the env var is ignored, so it cannot re-leak; kept as override → it is unset by default and never defaults to the WSL path again. Resolved at the gate. |
| An externally-created session has an unhelpful `session_path` (e.g. `$HOME`) | The operator picks / creates a rift session at the intended root (stamps `@root`); or the gate keeps `RIFT_PROJECT_ROOT` as an explicit override for that case. |

## Decision log

- 2026-07-10: Spec drafted. Retires the baked `RIFT_PROJECT_ROOT` /
  `RIFT_DEFAULT_PROJECT_ROOT` project-root seed in favour of the shipped
  per-session `@root` substrate (Phases 34–36); the daemon starts root-less and
  follows the session on attach. Motivated by a live failure: connecting to the
  empty devenv container leaked the justfile's WSL-path default (`justfile:293`)
  into the new session's `@root`, so the daemon worktree-scan failed (path not
  found) and the terminal was blank. Mirrors Phase 38 (retire `RIFT_SESSION`).
  One open decision (remove entirely vs keep an unset override) carried to the
  acceptance gate.
