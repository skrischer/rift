# Spec: Remote exec wrapper (container / WSL / jump target)

> Status: DRAFT
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
  once at connect from `RIFT_REMOTE_EXEC_WRAPPER` (runtime) with an optional
  `RIFT_DEFAULT_REMOTE_EXEC_WRAPPER` compile-time bake — **OPEN, resolved at the
  spec-acceptance gate** (see Prior decisions).
- Applying the wrapper at exactly the three non-PTY exec methods that carry
  daemon work: `exec_capture`, `open_daemon_channel`, `upload_executable`
  (`crates/ssh/src/connection.rs`). When a wrapper is set, the command string
  `C` becomes `<wrapper> sh -c <shell_single_quote(C)>`; when unset, `C` is
  passed through unchanged.
- Reusing the existing `shell_single_quote` helper for the added nesting layer —
  the same double-escaping idiom `launch.rs` already uses for `setsid sh -c`
  (tested there), so composed commands (`>`, `&&`, `if/fi`, `$HOME` expansion)
  resolve inside the container's shell, not the host's.
- Unit tests on the wrap transform: passthrough when unset; correct
  `<wrapper> sh -c '…'` shape when set; nested single-quote escaping; shell
  metacharacter / injection neutralization (mirroring the `launch.rs` guards).
- Documentation of the container-target usage (env vars, `RIFT_PROJECT_ROOT`
  pointing at the in-container root) in `CLAUDE.md` / justfile comments.

### Out of scope

- **The legacy `tmux -CC` path (`open_pty_exec`).** It requests a PTY (would need
  `-t`), the reactive layer is dead on it anyway, and it is already slated for
  removal (#285). The wrapper is not applied there; the container target is a
  daemon-path feature.
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

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Client wraps `docker exec` over the existing `ssh -> docker exec` path; the daemon runs in the container | Industry standard for editor-to-container (VS Code Dev Containers over SSH, DevPod, JetBrains Gateway all `docker exec`). Keeps the homelab's single-entry / tailnet-only / port-less invariant intact; zero server change | 2026-07-09 |
| AVOID sshd-in-container and host-side ForceCommand | sshd-in-container is a documented anti-pattern (attack surface, per-container key/user mgmt, broken process supervision — jpetazzo, HN); ForceCommand solves a client-fixable gap server-side. russh dials a TCP endpoint and does not shell out to system `ssh`, so no `~/.ssh/config` ProxyCommand "zero-code" path exists without sshd anyway | 2026-07-09 |
| Opaque single wrapper string, not a Docker-aware feature | Zed's `RemoteConnection` puts SSH/WSL/Docker behind one transport seam (prior-art Category 8); one string serves docker/podman/wsl/nsenter/sudo. Matches constitution "no premature abstraction" + "generality is a non-goal" — smaller than a container feature | 2026-07-09 |
| `docker exec -i`, never `-t` | The daemon transport is PTY-less binary framing; a TTY corrupts the frames | 2026-07-09 |
| Wrapper applied at the `SshConnection` exec chokepoint via `<wrapper> sh -c <quoted>` reusing `shell_single_quote` | The nesting idiom is already proven in `launch.rs`'s `setsid sh -c` (double-escape tested); one seam covers all daemon commands | 2026-07-09 |
| In-container root via existing `RIFT_PROJECT_ROOT`; legacy tmux path excluded | No new `--root` plumbing; `open_pty_exec` needs a PTY and is being removed (#285) | 2026-07-09 |
| **OPEN — resolved at the spec-acceptance gate:** env-only (`RIFT_REMOTE_EXEC_WRAPPER`) vs env + compile-time bake (`RIFT_DEFAULT_REMOTE_EXEC_WRAPPER`) | The bake mirrors the `RIFT_SSH_KEY` / `RIFT_DEFAULT_SSH_KEY` split and lets the dogfooding **stable** channel target the container; env-only is the smaller cut. Recommendation: include the bake | 2026-07-09 |

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
