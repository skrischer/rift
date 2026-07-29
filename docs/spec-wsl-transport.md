# Spec: WSL transport

> Created: 2026-07-29

A first-class WSL connection type alongside SSH — `wsl.exe -d <distro>` as a local
transport peer of the russh SSH connection, selected by a connection-kind chooser
on the connect card. Roadmap Phase 52.

## Outcome

- [ ] The connect card offers a connection-kind choice (SSH | WSL); choosing WSL replaces the host/user/port/key/passphrase fields with a distro chooser populated from the installed distros, and connects with no network parameters.
- [ ] A WSL connection provisions and drives the daemon exactly as SSH does — the reactive terminal, file explorer, git status, and LSP all work against a distro with zero feature-specific branching, because both transports satisfy one shared operation contract.
- [ ] `crates/ssh`'s SSH-only assumption is generalised into a transport seam (SSH | WSL) at the four operations the daemon lifecycle needs (`exec_capture`, `upload_executable`, `open_daemon_channel`, `is_closed`); the daemon-provisioning and reconnect code is transport-agnostic.
- [ ] A WSL target is stored in and reconnected from recents (per-channel), distinct from an SSH target, and survives a tolerant load of an older recents file.
- [ ] The WSL transport is distinct from `RIFT_REMOTE_EXEC_WRAPPER` (which nests one hop deeper *over* an SSH connection); the two are not conflated in code or UI.

## Scope

### In scope

- A transport seam in `crates/ssh` (or a transport module it owns) covering the four verbs the daemon lifecycle calls — `exec_capture`, `upload_executable`, `open_daemon_channel`, `is_closed` — with two implementations: the existing `SshConnection` (russh) and a new `WslConnection` that runs `wsl.exe -d <distro> -- <cmd>` as a local child process with stdio pipes.
- A connection-kind discriminator threaded through `SshConfig` / `ConnectRequest` / `RecentConnection` (additive, serde-tolerant), and `recents.rs`'s `identity()` chokepoint extended to key a WSL target by distro rather than the SSH 5-tuple.
- A connect-card kind chooser (SSH | WSL) and a WSL distro chooser populated from `wsl.exe -l -q`; the SSH fields hidden when WSL is selected.
- WSL-specific daemon deployment: reuse the existing `uname -sm` → musl-triple probe and the `cat`-stream `upload_executable` path (both run unchanged inside the distro), with the daemon binary and UDS socket placed on the distro's Linux filesystem.
- A WSL-specific liveness / retryable mapping: `is_closed()` means "the `wsl.exe` child exited"; distro-not-running / `wsl.exe`-missing map to a non-retryable connect error.

### Out of scope

- The full Zed-style "Add Project" connect-screen redesign (fuzzy folder picker, compact start screen — QA finding #13's UI half). Phase 52 adds only the minimal transport chooser to the existing card; the redesign is a separate concern.
- A **Docker** transport. The seam is shaped so a third variant is a later addition, but only SSH and WSL are built here (Zed's split is SSH/WSL/Docker; rift builds two).
- The dead terminal-PTY path (`open_pty` / `open_pty_exec` / `PtyStream` in `crates/ssh`). It has zero consumers since #285 removed the tmux-`-CC`-over-SSH-PTY escape hatch; the daemon protocol channel is the only live terminal source, so the WSL transport reproduces that one path and does not implement a PTY transport.
- Reusing `RIFT_REMOTE_EXEC_WRAPPER` to reach WSL. The wrapper runs one hop deeper over a live SSH transport (`russh → host sh → docker/wsl exec`); a first-class WSL transport has no host and no russh. WSL is a transport peer, not a wrapper value.
- Running the daemon from a Windows-side `/mnt/c/...` path (skip-the-copy optimisation). The daemon is copied into the distro like the SSH path; see Prior decisions.

## Constraints

- **One live data path.** The app never consumes the SSH PTY stream; the terminal rides the daemon protocol (`run_daemon_terminal`, `main.rs`). So the transport contract is exactly the four verbs the daemon lifecycle uses (`exec_capture`, `upload_executable`, `open_daemon_channel`, `is_closed`) plus a per-variant constructor — not a general SSH surface.
- **The daemon lifecycle is already transport-neutral POSIX `sh`.** `launch.rs` / `deploy.rs` build `sh` command strings (`setsid`, `mkdir -p`, `test -x`, `uname -sm`, `--ping`/`--connect`/`--serve-uds`) and run them through the four verbs. WSL runs a real Linux distro, so those strings work unchanged inside it; only *how the command string is executed* changes (russh channel vs. `wsl.exe` subprocess stdio). `deploy.rs`/`launch.rs` take `&mut SshConnection` concretely today and flip to the seam.
- **The daemon binary is already the WSL target.** It builds `x86_64-unknown-linux-musl`; `uname -sm` inside WSL returns `Linux x86_64`, so `target_triple_from_uname` matches as-is with no new build target.
- **AF_UNIX on DrvFs is unreliable.** The daemon's UDS socket must live on the distro's Linux filesystem (e.g. `$HOME/.rift`), never a `/mnt/c` DrvFs path. The remote-dir / socket-path default needs a WSL-appropriate value.
- **Recents identity is SSH-shaped.** `identity()` keys on the `(host, user, port, key, remote_exec_wrapper)` 5-tuple; a WSL target has only a distro name. The additive-field precedent (`remote_exec_wrapper`, `recent_roots` both `#[serde(default)]`) makes a `kind`/`distro` field a low-risk tolerant-load extension. `identity()` is the single dedup chokepoint to change.
- **`wsl.exe` is a Windows-only binary.** The WSL transport compiles and is offered only on the Windows target; the seam abstraction is cross-platform but the WSL variant is Windows-gated (matches the app's Windows-primary target; Linux/X11 builds offer SSH only).
- Agent-agnostic; no new heavyweight dependency (spawn `wsl.exe` via the existing async process facility / `tokio::process`); daemon protocol and `crates/protocol` unchanged (a transport swap is below the protocol).

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 52 row: generalise rift's SSH-only `crates/ssh` into a transport seam with a WSL variant (`wsl.exe -d <distro> …` exec) and a connect-card kind chooser; distinct from `RIFT_REMOTE_EXEC_WRAPPER`.
- [Category 8: Remote Development & SSH #1 — Zed `crates/remote`](prior-art.md#category-8-remote-development--ssh) — the `RemoteConnection` trait abstracting SSH / WSL / Docker (`crates/remote/src/transport/{ssh,wsl,docker}.rs`); WSL auto-deploy via `parse_platform` from `uname -sm` and versioned server directory (`wsl.rs`). The reference for the seam shape and the WSL-specific deploy path.

## Human prerequisites

- The dev/QA station (Windows GPU station) must have WSL2 installed with at least one Linux distro registered (`wsl.exe -l -q` lists it), so the WSL transport can be exercised at the milestone QA gate. No secrets or accounts are needed — WSL is local.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The transport contract is exactly four verbs (`exec_capture`, `upload_executable`, `open_daemon_channel`, `is_closed`) plus a per-variant constructor | The app consumes only the daemon channel (the SSH PTY path is dead code); the daemon lifecycle needs nothing more | 2026-07-29 |
| The seam lives at the `launch.rs`/`deploy.rs` boundary; `provision_daemon` and the reconnect engine stay unchanged above it | Those builders already need only the four verbs and already speak transport-neutral `sh`; the abstraction slots in with the smallest blast radius | 2026-07-29 |
| WSL daemon deploy reuses the existing `uname -sm` probe and `cat`-stream `upload_executable`, copying the musl binary INTO the distro | The musl binary runs natively in the distro; reusing the SSH deploy path unchanged beats a WSL-special `/mnt/c` run-in-place, and keeps one deploy code path | 2026-07-29 |
| The UDS socket + daemon dir live on the distro's Linux filesystem, not `/mnt/c` | AF_UNIX on DrvFs is unreliable; the socket must be native-FS | 2026-07-29 |
| The connection kind is an additive `#[serde(default)]` discriminator on `SshConfig`/`ConnectRequest`/`RecentConnection`; `identity()` keys a WSL target by distro | Tolerant-load precedent (`remote_exec_wrapper`, `recent_roots`); one chokepoint (`identity()`) to change; an older recents file still loads | 2026-07-29 |
| The WSL variant is Windows-gated; Linux/X11 builds offer SSH only | `wsl.exe` is Windows-only; matches the app's Windows-primary target | 2026-07-29 |
| Phase 52 adds only a transport chooser to the existing connect card — NOT the full Zed-style "Add Project" redesign | The roadmap row scopes Phase 52 to the transport + kind chooser; the redesign (#13 UI) is a separate concern | 2026-07-29 |
| **OPEN — resolved at the spec-acceptance gate:** the transport seam shape — `enum Connection { Ssh, Wsl }` vs. a `Transport` trait | Architecture-impacting (the foundation-impact line mandates ratification here). Enum sidesteps `async_trait`/`&mut self` object-safety friction for two known variants; a trait reads cleaner if Docker (a third) is coming. Constitution "extract a trait at 2+ implementations" cuts toward a trait; the async friction cuts toward an enum | 2026-07-29 |
| **OPEN — resolved at the spec-acceptance gate:** the connect-card UI shape — one card with an SSH/WSL kind toggle that swaps the fields, vs. a separate WSL card reached from the kind choice | UX decision neither precedent nor constraint settles; both fit the existing `ConnectionScreen` | 2026-07-29 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged (one per implementable step)

## Verification

- [ ] `just ci` equivalent green for the touched crates (fmt + clippy `-D warnings` + tests); `crates/ssh` and `crates/app` compile warm-target clean.
- [ ] Unit test: the connection-kind discriminator round-trips through recents serde, and an older recents JSON without the field still loads (tolerant default → SSH).
- [ ] Unit test: `identity()` keys a WSL target by distro (two WSL targets on the same distro dedup; a WSL and an SSH target never collide).
- [ ] QA (on the Windows station with WSL2): choosing WSL on the connect card lists the installed distros, and connecting to one brings up the reactive terminal + explorer + git + LSP against files in the distro, with no host/user/port/key entered.
- [ ] QA: the WSL daemon is deployed into the distro (versioned path on the Linux FS), reused on reconnect, and the socket lives on the Linux FS (not `/mnt/c`).
- [ ] QA: a WSL target is saved to recents and reattaches from the recents `Preferred` path on the next launch, distinct from any SSH recent.
- [ ] QA: an unavailable distro (stopped / mistyped) surfaces a clear non-retryable connect error rather than a reconnect spin.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The seam refactor regresses the SSH path (the daily driver) | The four-verb contract is behaviour-preserving for `SshConnection`; keep SSH the default kind; QA the SSH channel before/after; the reconnect engine is unchanged above the seam |
| `wsl.exe` subprocess stdio corrupts the daemon binary framing (as a TTY would) | The transport is PTY-less by construction — plain stdio pipes, no `-t`, mirroring the daemon-channel design note; the daemon transport is binary framing over stdout/stdin |
| DrvFs socket path chosen by accident (unreliable AF_UNIX) | The WSL remote-dir/socket default is pinned to the Linux FS (`$HOME/.rift`); QA asserts the socket path is not under `/mnt` |
| `enum` vs `trait` picked wrong for a future Docker transport | Resolved at the gate; whichever is chosen, the four-verb contract is the stable surface a third transport implements later |
| WSL distro list empty or `wsl.exe` absent | The kind chooser surfaces "no distros found / WSL not installed" and leaves SSH selectable; not a crash |

## Decision log

- 2026-07-29: Scoped from a `crates/ssh` + connect-flow read (background scoping agent). Central finding: the SSH PTY path is dead code, so the transport contract is exactly the four daemon-lifecycle verbs; the daemon `sh` commands are already transport-neutral and run unchanged inside WSL; the musl daemon is already the WSL target. Two genuinely-open points carried to the gate: the seam shape (enum vs trait) and the connect-card UI shape.
