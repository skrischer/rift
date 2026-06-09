# Spec: Phase 3 — Daemon scaffolding + transport

> Status: READY
> Created: 2026-06-05
> Completed: —

Stand up the remote daemon as a headless tokio service that the GPUI app reaches over a reconnectable SSH transport, auto-deploys, and talks the `rift-protocol` message loop with — the structural foundation every later Phase 3 sub-spec (file tree, git status, LSP, terminal streaming) builds on.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The GPUI app connects over SSH, detects the remote platform, and auto-deploys the versioned `rift-daemon` musl binary when it is missing or outdated, then spawns it.
- [ ] A `ClientMessage` sent from the app reaches the daemon and a `DaemonMessage` reply returns over the transport — a full protocol round-trip works.
- [ ] The daemon survives an SSH connection drop and a reconnect reattaches to the still-running daemon instead of spawning a second one — validated on the Windows host path (where `ControlMaster` does not apply and the fallback handshake is active), not only on Unix.
- [ ] The daemon holds a single `State` struct and notifies consumers via `tokio::sync::watch`/`broadcast` — no `Arc<Mutex<State>>`.
- [ ] The daemon is a flat dispatch loop routing `rift-protocol` messages to handlers — no GPUI dependency, no per-subsystem entity graph.

## Scope

### In scope

- **Daemon process skeleton**: tokio service, single `State` struct, channel-based notification, a flat dispatch loop over `rift-protocol` `ClientMessage`/`DaemonMessage`.
- **Transport layer**: SSH connection reuse (`ControlMaster` socket on Unix, magic-string handshake fallback on Windows) and auto-deploy of the versioned musl binary, lifted from Zed's `crates/remote`. A dedicated `russh` channel carries the `rift-protocol` framing as the client↔daemon channel — `russh` already multiplexes channels, so no extra protocol layer (no WebSocket) and no new dependency.
- **Lifecycle**: connect → detect platform (`uname -sm`) → upload/spawn daemon if absent/outdated → reattach on reconnect (daemon-as-proxy survives drops).
- **The single transport seam**: establish the interface so the existing client-side `TmuxClient` path can later swap to the daemon protocol as a one-seam change (per the `architecture.md` "tmux control-mode interaction model" contract).

### Out of scope

- **VTE parsing location** (client- vs daemon-side) — genuinely open; resolved by a separate spike before the terminal-streaming sub-spec. This spec carries raw protocol round-trips only, so it does not depend on the outcome.
- **Terminal streaming migration** — actually moving `%output` rendering from the client `TmuxClient` to the daemon. Depends on the VTE decision; own sub-spec.
- **File tree / worktree snapshot** — own sub-spec (decision pre-made, see Prior decisions).
- **Git status** — own sub-spec.
- **LSP integration** — own sub-spec (decision pre-made, see Prior decisions).

## Constraints

- The daemon binary target is `x86_64-unknown-linux-musl`, statically linked, headless. It must not depend on `gpui`/`gpui-component`.
- The daemon depends only on crates that cross-compile to static musl. Re-add a sibling crate to `crates/daemon/Cargo.toml` only when the daemon actually uses it, and first verify it (and its transitive deps) is `gpui`-free and musl-clean. `rift-terminal` pulls `gpui`/`gpui-component` and must never become a daemon dependency — terminal rendering stays client-side.
- Daemon error handling uses `thiserror` in libs and `anyhow` in the binary; no `.unwrap()` in library code (per CLAUDE.md).
- Adding to `crates/protocol/` is a deliberate API change — both sides depend on it, never on each other.
- The app runs on the Windows host; SSH `ControlMaster` is a Unix-socket feature, so the Windows side needs Zed's documented fallback path (see Risks).
- `russh` is already the SSH dependency; reuse it rather than introducing another SSH stack.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| Daemon form is **Lapce-flat dispatch**, not Zed's `HeadlessProject` | `HeadlessProject` is a GPUI construct (`Entity<T>`, `Context`, the GPUI executor running server-side); the rift daemon is a headless `tokio`/musl service with no GPUI. CLAUDE.md already mandates a single `State` struct + `watch`/`broadcast` channels (not `Arc<Mutex>`) and "no premature abstraction" — Zed's dual `Local…`/`Remote…` trait pairs are exactly that. `crates/protocol` already exists as a flat tagged-enum RPC, which is the Lapce-proxy shape. | 2026-06-05 |
| **Transport pattern lifted from Zed**: connection reuse + auto-deploy of a versioned daemon binary. The reattach contract is the invariant; `ControlMaster` is the Unix optimization, with a magic-string handshake fallback on Windows | Validated by Zed, Lapce, Arbor, VS Code Remote (`prior-art.md` pattern #1, "industry-standard"). The primary dev/test host is Windows (`just dev-windows`), where `ControlMaster` never applies — so the fallback path is the one exercised first, and the reattach Outcome/Verification is validated there, not on Unix `ControlMaster`. | 2026-06-05 |
| Client↔daemon channel is a **dedicated `russh` channel carrying `rift-protocol` framing** (no WebSocket) | `russh` already provides multiplexed channels; layering WebSocket (`tokio-tungstenite`) on top would add a redundant framing layer and a new dependency, against CLAUDE.md's minimal-dependency rule. `architecture.md` updated to match. | 2026-06-05 |
| Reuse `russh` for the SSH transport | Already a dependency; `prior-art.md` Category 8 confirms it covers the PTY/forward flows needed. | 2026-06-05 |

**Recorded for later Phase 3 sub-specs (not owned by this spec, but resolved so they are not re-litigated):**

- **File-sync strategy** — Zed `crates/worktree` `Snapshot` model with incremental `UpdateWorktree` messages (daemon serves tree + incremental updates; not full sync), fed by `notify` + `jwalk`. Validated by `prior-art.md` ("exactly the daemon→client protocol rift needs"). Owned by the file-tree sub-spec.
- **LSP lifecycle** — daemon-side, lazily started per `DocumentSelector` on document open, trust-gated; multi-server-per-document via a registry; `async-lsp` as primary client crate (fallback: fork `helix-lsp`). Validated by Zed + Lapce. Owned by the LSP sub-spec; a one-day rust-analyzer round-trip spike precedes commitment.
- **VTE parsing location** — OPEN. Resolve via a one-day spike (client-side per termy vs daemon-side per WezTerm-mux) before the terminal-streaming sub-spec.

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under a Phase 3 milestone. Created once this spec is `READY`.

- Milestone: Phase 3 — Remote daemon (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` produces a static binary
- [ ] App connects, auto-deploys the daemon when the remote binary is missing or version-mismatched, and spawns it
- [ ] A `ClientMessage` → daemon → `DaemonMessage` round-trip completes over the `russh` channel
- [ ] Killing the SSH connection leaves the daemon running; reconnect reattaches without a second daemon process (verified by remote process count), exercised on the Windows dev host where the fallback handshake — not `ControlMaster` — is the active path
- [ ] The daemon exposes its `State` through channels; a grep confirms no `Arc<Mutex<State>>` in the daemon crate

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `ControlMaster` is Unix-only; the app runs on the Windows host | Adopt Zed's Windows fallback (the `ZED_SSH_CONNECTION_ESTABLISHED` magic-string handshake, `ssh.rs:202-230`) rather than relying on a control socket. This fallback is the **primary exercised path** — build and test it first, not only the Unix `ControlMaster` path. |
| musl cross-compile toolchain not set up in the dev/CI environment | Add the target and document the build step before implementation; the `cargo build` verification line gates this. |
| Auto-deploy version scheme undefined (when is the remote binary "outdated"?) | Define a versioned binary name `rift-daemon-<version>` and compare against the app's compiled-in version, mirroring Zed's `remote_server_dir_relative()` scheme. Decide the exact scheme in the first issue. |
| Protocol message set is currently tmux-shaped (`PaneOutput`, `TmuxCommand`) and may need handshake/version messages | Add a minimal handshake/version exchange to `rift-protocol` as a deliberate, reviewed API change; keep it additive. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-05: Spec created. Daemon form (Lapce-flat), transport pattern (Zed ControlMaster + auto-deploy), and channel recorded as pre-made; VTE parsing location left open for a spike. See Prior decisions for rationale.
- 2026-06-05: PR #52 review (NEEDS CHANGES, minor) addressed. (1) Channel changed from WebSocket-over-SSH to a dedicated `russh` channel — WebSocket was unjustified given `russh` already multiplexes and would add a dependency; `architecture.md` aligned. (2) Made the Windows fallback handshake the primary exercised/validated path in Outcome, Verification, and Risks, since `ControlMaster` never applies on the Windows dev host.
- 2026-06-09: #59 (musl build, PR #99) trimmed the daemon's premature dependencies — `crates/daemon/Cargo.toml` dropped `rift-terminal`, `rift-explorer`, `rift-tmux-core`, and `rift-plugin-api`. All were unused by the current skeleton; `rift-terminal` additionally pulls `gpui`/`gpui-component`, which is forbidden in the daemon and cannot cross-compile to musl, so the static musl build was impossible until it was removed. Kept `rift-protocol`, `tokio`, `anyhow`. Consequence for #58 onward: re-add only the deps the daemon actually uses and keep it `gpui`-free / musl-clean (see Constraints).
- 2026-06-09: #60 (transport seam, PR #114). Added a length-delimited JSON frame codec to `rift-protocol` (`u32` big-endian length prefix + `serde_json` payload; partial-read-safe `FrameDecoder`). The daemon speaks the protocol over **stdio** (`rift_daemon::serve` over `AsyncRead`/`AsyncWrite`) — no `russh` in the daemon, keeping it musl-clean; the SSH host wires the daemon's stdin/stdout to a non-PTY `russh` exec channel (`SshConnection::open_daemon_channel`). The single app-side emission seam is `rift_ssh::DaemonClient`. Two framing-integrity invariants fell out of review: the daemon's stdout is **frame-only** (the startup banner moved to stderr), and the protocol channel **drops remote stderr** (`ChannelMsg::ExtendedData`) instead of feeding it to the decoder — either would otherwise corrupt the length-delimited framing.
- 2026-06-09: #61 (auto-deploy, PR #115). `rift_ssh::ensure_daemon_deployed` does detect (`uname -sm`) + resolve versioned path (`rift-daemon-<version>`) + upload-when-absent only; it does **not** launch the daemon. A detached `&` background spawn over an exec channel is a no-op — the daemon inherits that channel's stdin/stdout, and once the channel drains and `sshd` closes the FDs the daemon reads EOF on stdin and exits (per `serve`'s contract). The real launch is opening the protocol channel on the resolved path via `open_daemon_channel`, deferred until a consumer wires the daemon protocol (the future `TmuxClient` swap); there is no persistent daemon to keep alive before then. **Consequence:** the "auto-deploys … and spawns it" Outcome/Verification line is met by the transport-consumption step, not by deploy. Two further decisions: the deploy logic lives in `crates/ssh` (not `crates/app`) so `cargo test --workspace --exclude rift-app` actually exercises its tests; and only `Linux x86_64` -> `x86_64-unknown-linux-musl` is mapped (a single `RIFT_DAEMON_BINARY` is uploaded with no per-arch selection, so other architectures return `None` rather than silently shipping the wrong binary).
