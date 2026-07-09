# Spec: Remote exec wrapper (container / WSL / jump target)

> Status: READY
> Created: 2026-07-09
> Completed: —

A single opaque command-wrapper (`RIFT_REMOTE_EXEC_WRAPPER`, e.g.
`docker exec -i devenv`) applied at the SSH exec chokepoint so rift runs its
daemon + tmux one hop deeper than the SSH login — inside a remote dev container,
a WSL distro, or under a jump user — over the already-blessed
`ssh host` -> `docker exec` path, with zero server-side changes and no Docker
knowledge baked into rift.

## Outcome

- [ ] With `RIFT_REMOTE_EXEC_WRAPPER="docker exec -i <container>"` and
      `RIFT_PROJECT_ROOT=/workspace` set, connecting deploys the daemon binary
      *into the container*, spawns it there, and the reactive layer (file tree /
      git / diagnostics) watches the container's `/workspace` — a file edited
      inside the container lights up the explorer.
- [ ] The terminal attaches its tmux session inside the container (tmux lives in
      the container, not on the host).
- [ ] With the wrapper unset, behavior is byte-for-byte today's: every remote
      command runs directly in the host login shell, no `sh -c` added.
- [ ] rift contains no Docker-specific code path — the same wrapper string drives
      `docker exec`, `podman exec`, `wsl -d <distro>`, `nsenter`, or `sudo -u`
      unchanged (constitution: no target detection/special-casing).
- [ ] The daemon binary transport stays a PTY-less pipe: the wrapper never forces
      a TTY (`docker exec -i`, never `-t`); a wrapper carrying `-t` is the user's
      error, documented, not rift's to guard.

## Scope

### In scope

- A `remote_exec_wrapper: Option<String>` carried on `SshConnection`, resolved
  once at connect from `RIFT_REMOTE_EXEC_WRAPPER` (runtime) with a
  `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER` compile-time bake — runtime wins over the
  bake, mirroring the `RIFT_SSH_KEY` / `RIFT_DEFAULT_SSH_KEY` and
  `RIFT_PROJECT_ROOT` / `RIFT_DEFAULT_PROJECT_ROOT` splits, so the dogfooding
  stable channel (`just promote`) can target the container without runtime env.
- A **single shared `wrap(command)` helper** (on `SshConnection` / in the `exec`
  module) that all three non-PTY exec methods — `exec_capture`,
  `open_daemon_channel`, `upload_executable` (`crates/ssh/src/connection.rs`) —
  call, so "wrapped or not" is atomic across every daemon command. When a wrapper
  is set, the command string `C` becomes `<wrapper> sh -c <shell_single_quote(C)>`;
  when unset, `C` is passed through unchanged. One shared helper (not three
  independent edits) is a hard requirement: a method that forgets to wrap would
  split the deploy — probing in-container while `cat`-ing the binary to the host
  FS (or vice-versa) — silently.
- The wrapper is threaded onto `SshConnection` via a **setter/builder**
  (e.g. `with_remote_exec_wrapper`), NOT as a new `connect()` parameter — so the
  `ssh` crate keeps its independent green build and issue 1 (ssh) does not force
  the single `connect()` call site (`crates/app/src/main.rs:1458`) into the same
  PR. The `<wrapper>` tokens are spliced **raw** (word-split by the host shell
  into argv, e.g. `docker exec -i devenv`); only `C` is quoted. Same trust model
  as `RIFT_SSH_KEY`.
- Reusing the existing `shell_single_quote` helper for the added nesting layer —
  the same double-escaping idiom `launch.rs` already uses for `setsid sh -c`
  (tested there), so composed commands (`>`, `&&`, `if/fi`, `$HOME` expansion)
  resolve inside the container's shell, not the host's.
- Unit tests on the wrap transform: passthrough when unset; correct
  `<wrapper> sh -c '…'` shape when set; nested single-quote escaping; shell
  metacharacter / injection neutralization (mirroring the `launch.rs` guards).
- Documentation of the container-target usage in `CLAUDE.md` / justfile
  comments: `RIFT_PROJECT_ROOT` must be an **absolute in-container path**
  (`/workspace`), never `$HOME`-relative (`launch_command` single-quotes
  `--root` literally, no `$HOME` expansion); and `RIFT_DAEMON_REMOTE_DIR` should
  be set to an absolute in-container dir when the image does not guarantee
  `$HOME` (`docker exec` runs no login shell — an unset `$HOME` makes the deploy
  land in `/.rift/bin`).

### Out of scope

- **The legacy `tmux -CC` path (`open_pty_exec`).** It requests a PTY (would need
  `-t`), the reactive layer is dead on it anyway, and it is already slated for
  removal (#285). The wrapper is not applied there; the container target is a
  daemon-path feature. **Split-brain caveat:** with the wrapper set AND
  `RIFT_TERMINAL_LEGACY` selected, the daemon still provisions wrapped (watches
  the container's `/workspace`) while `run_legacy_terminal` runs host tmux
  unwrapped (`main.rs:1519-1538`, `:2006`) — explorer/git on the container,
  terminal on the host. The wrapper is only coherent on the **daemon terminal
  path** (the default); document that the two must not be combined. Not guarded
  in code — the legacy hatch is being removed.
- A connection-screen UI field for the wrapper — env/bake only for this cut
  (proportional; a UI field is a separate later issue if wanted).
- Any change to the daemon, protocol, or `--root` mechanism: the in-container
  root rides the existing `RIFT_PROJECT_ROOT` -> `connect_or_spawn_daemon(root)`
  path unchanged (set it to `/workspace`).
- Server-side changes of any kind (no sshd-in-container, no host ForceCommand) —
  explicitly rejected, see Prior decisions.
- Building a distinct daemon target for the container: the existing
  `x86_64-unknown-linux-musl` static binary runs in the container as-is.

## Constraints

- The daemon transport is a length-prefixed binary protocol over a **PTY-less**
  exec channel (`open_daemon_channel`: `channel.exec` with no `request_pty` —
  `crates/ssh/src/connection.rs:140`). A PTY line discipline (ONLCR / echo) would
  corrupt the frames, so the wrapper must not allocate a TTY. Same for the raw
  binary upload (`upload_executable`) and the probes.
- Every daemon remote command today runs in the **remote host login shell**
  (`channel.exec(true, cmd)`). A naive prefix (`docker exec -i c <cmd>`) would
  let the host shell parse `>`, `&&`, `if`, and expand `$HOME` — deploying the
  binary onto the host FS and resolving `$HOME` to the host home, silently. The
  whole command must be relocated into the container as an argument to the
  container's `sh -c`.
- The daemon lifecycle is self-contained in the daemon **binary** (`--ping` /
  `--serve-uds` / `--connect` — `crates/ssh/src/launch.rs`): no `socat`/`nc`
  dependency. The container needs only the deployed binary, `tmux` (already
  present), a POSIX `sh`, and `setsid` (util-linux) for the detached spawn.
- All ten daemon remote-command sites funnel through the three `SshConnection`
  exec methods, so the wrapper is one chokepoint, not ten call-site edits.
- Constitution: no agent/target detection. The wrapper is an opaque string; rift
  learns nothing about Docker.
- The wrapper must be a transparent transport: pass stdin through to the inner
  command, forward EOF, and propagate the inner exit status (docker/podman/wsl
  `-d`/nsenter/sudo all do). A wrapper that swallows the exit status would break
  `drain_channel`'s zero-exit contract (`connection.rs:259`). Stated as a
  requirement on the wrapper, not something rift can enforce.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Client wraps `docker exec` over the existing `ssh -> docker exec` path; the daemon runs in the container | Industry standard for editor-to-container (VS Code Dev Containers over SSH, DevPod, JetBrains Gateway all `docker exec`). Keeps the homelab's single-entry / tailnet-only / port-less invariant intact; zero server change | 2026-07-09 |
| AVOID sshd-in-container and host-side ForceCommand | sshd-in-container is a documented anti-pattern (attack surface, per-container key/user mgmt, broken process supervision — jpetazzo, HN); ForceCommand solves a client-fixable gap server-side. russh dials a TCP endpoint and does not shell out to system `ssh`, so no `~/.ssh/config` ProxyCommand "zero-code" path exists without sshd anyway | 2026-07-09 |
| Opaque single wrapper string, not a Docker-aware feature | Zed's `RemoteConnection` puts SSH/WSL/Docker behind one transport seam (prior-art Category 8); one string serves docker/podman/wsl/nsenter/sudo. Matches constitution "no premature abstraction" + "generality is a non-goal" — smaller than a container feature | 2026-07-09 |
| `docker exec -i`, never `-t` | The daemon transport is PTY-less binary framing; a TTY corrupts the frames | 2026-07-09 |
| Wrapper applied at the `SshConnection` exec chokepoint via `<wrapper> sh -c <quoted>` reusing `shell_single_quote` | The nesting idiom is already proven in `launch.rs`'s `setsid sh -c` (double-escape tested); one seam covers all daemon commands | 2026-07-09 |
| In-container root via existing `RIFT_PROJECT_ROOT`; legacy tmux path excluded | No new `--root` plumbing; `open_pty_exec` needs a PTY and is being removed (#285) | 2026-07-09 |
| Config surface = env + compile-time bake: `RIFT_REMOTE_EXEC_WRAPPER` (runtime) wins over `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER` (bake) | Mirrors the `RIFT_SSH_KEY` / `RIFT_DEFAULT_SSH_KEY` split so the dogfooding **stable** channel can target the container without runtime env — the daily-driver use case. Resolved at the spec-acceptance gate | 2026-07-09 |

## Prior art

- `docs/prior-art.md` Category 8 (Remote Development & SSH), Zed
  `crates/remote`: the `RemoteConnection` trait abstracts **SSH / WSL / Docker**
  transports behind one seam (`transport/ssh.rs`, `wsl.rs`, `docker.rs`) and
  auto-deploys a versioned daemon from `uname -sm` — the direct precedent for an
  opaque transport wrapper plus the deploy-into-target pattern rift mirrors.
- External decision sources (recorded, not clonable): VS Code "Develop on a
  remote Docker host" + "Dev Containers over Remote-SSH"; DevPod; JetBrains
  Gateway (all `docker exec` over SSH); jpetazzo "If you run SSHD in your Docker
  containers, you're doing it wrong"; HN "Docker containers should not run an SSH
  server" (the sshd-in-container anti-pattern).

## Tracking

- Milestone: [Remote exec wrapper](<milestone-url>)
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path in its body.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Unit test: wrap transform returns the input unchanged when the wrapper is
      `None`/empty.
- [ ] Unit test: with a wrapper set, a composed command
      (`cat > 'x' && mv 'x' 'y'`) becomes `<wrapper> sh -c '<doubly-escaped>'`,
      and a path carrying `$(…)` / backticks stays inert inside the quoting.
- [ ] Unit test at shipping depth: feed the real `launch_command(...)` output
      (already `setsid sh -c '…'`, 2-deep) and the `cat_to_executable_command`
      upload body through `wrap`, asserting the triple-nested shape round-trips
      and injection stays inert — proves the composition at the depth that ships,
      not one level shallower.
- [ ] Behavioral (dev channel, manual QA): with
      `RIFT_REMOTE_EXEC_WRAPPER="docker exec -i <container>"` +
      `RIFT_PROJECT_ROOT=/workspace`, connecting to the homelab host deploys the
      daemon into the container (`$HOME/.rift/bin` resolves to the container
      home), spawns it, the terminal runs tmux in the container, and editing a
      file inside the container updates the explorer / git / diagnostics.
- [ ] Behavioral: with the wrapper unset, a normal host connection is unchanged.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Nested single-quote escaping breaks composed commands | Reuse `shell_single_quote` (proven in `launch.rs` double-escape tests); add wrap-transform unit tests incl. injection cases |
| A wrapper carrying `-t` corrupts the binary transport | Document `-i` only; the daemon handshake fails loudly (not silently) if frames are mangled — surfaced as a connect error, not degraded |
| Container lacks `setsid` / `sh` | Full dev containers ship both; note the prerequisite in docs. A minimal container failing the detached spawn surfaces as `RIFT_DAEMON_TIMEOUT` -> visible launch error |
| Deploy silently lands on the host if a seam is missed | Applying at the single `SshConnection` chokepoint (not per-call-site) makes "wrapped or not" total across daemon commands; the QA step verifies the in-container `$HOME` deploy path |

## Decision log

- 2026-07-09: Scope narrowed from "wrap the tmux command" to "wrap the daemon
  path at the `SshConnection` exec chokepoint" after reading `connection.rs` /
  `deploy.rs` / `launch.rs`: the daemon is the default terminal source (#205),
  the legacy `tmux -CC` path is being removed (#285), and the daemon lifecycle is
  self-contained in the binary — so one wrapper at three methods covers deploy,
  probe, launch, relay, and stop.
- 2026-07-09 (spec-acceptance gate): config surface resolved to **env + bake**
  (`RIFT_REMOTE_EXEC_WRAPPER` runtime wins over `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER`
  bake), so the stable dogfooding channel can target the container. Review
  findings folded in pre-merge: single shared `wrap()` helper (atomic across the
  three methods), builder/setter threading (not a `connect()` param), legacy
  split-brain caveat, `$HOME`/`RIFT_DAEMON_REMOTE_DIR` container edge, wrapper
  pass-through requirement, and a shipping-depth (triple-nested) wrap test.
- 2026-07-09 (issue #763): wrap transform landed as a free `pub(crate) fn
  wrap_command(wrapper: Option<&str>, command: &str) -> String` in
  `connection.rs`'s `exec` module (not a `&self` method), so it is
  unit-testable without an `SshConnection`; `open_daemon_channel`,
  `exec_capture`, and `upload_executable` all route through it via a new
  `remote_exec_wrapper` field set by `with_remote_exec_wrapper`.
  `open_pty`/`open_pty_exec` are untouched per scope.
