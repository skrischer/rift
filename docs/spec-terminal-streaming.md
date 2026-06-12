# Spec: Phase 6 — Terminal streaming

> Status: DRAFT
> Created: 2026-06-12
> Completed: —

The tmux engine connection moves behind the daemon: the daemon owns rift's own tmux control-mode client (paying the `crates/tmux-core` tech-debt row — termy's `TmuxClient` leaves the terminal path) and streams per-pane terminal output to the app over the rift protocol, while the app keeps its proven VTE + render stack. This completes the target architecture's process split (`architecture.md`: daemon runs the "tmux control mode client") through the single seam the scaffolding spec built the transport for — gated by the pre-recorded **VTE-location spike** (`archive/spec-daemon-scaffolding.md`: "Resolve via a one-day spike … before the terminal-streaming sub-spec").

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The app renders the tmux session **without opening an SSH PTY channel for tmux**: terminal bytes flow tmux → daemon (control-mode child process) → rift protocol → client VTE/render; keyboard input, resize, and tmux command emission flow the reverse path through the same seam.
- [ ] The daemon drives tmux through **rift's own control-mode client** in `crates/tmux-core` (musl-clean, `gpui`-free): `%begin`/`%end`/`%error` command guards, the notification stream (`%output` with octal-escape decoding, `%layout-change`, `%window-add`/`-close`, `%session-changed`, `%pane-mode-changed`), and command emission. termy's `TmuxClient` is no longer in the terminal path (tech-debt row paid).
- [ ] Per-pane output streams with **bounded flow control**: a flooding pane (a build, `yes`) never freezes the UI, starves other panes, or grows daemon/client buffers without bound; tmux's pause-after flow control is preserved through the daemon.
- [ ] **Multi-client semantics are unchanged**: each connected rift instance is its own tmux client (per-client attach daemon-side), so the dogfooding channels' mirrored views and `window-size largest` behavior work exactly as today.
- [ ] **Reconnect resumes**: dropping SSH and reconnecting reattaches to the running daemon and resumes streaming with a fresh snapshot — tmux remains the session persistence; no terminal state is lost.
- [ ] **Latency parity**: interactive typing and output feel are not perceptibly worse than the direct `-CC` path — the spike pins the measured bar and the swap meets it.
- [ ] Agent-agnostic throughout: pane bytes stay opaque; no parsing of any process's output beyond VTE semantics.
- [ ] The legacy direct `tmux -CC`-over-SSH-PTY path is handled per the gate decision (see Prior decisions) — no silent second code path lingers without a recorded plan.

## Scope

### In scope

- **VTE-location spike (the milestone's first issue and commitment gate)**: forward raw `%output` bytes for one pane end-to-end (tmux → daemon → protocol → client `alacritty_terminal`), measure interactive latency and flood throughput against the direct `-CC` path, and pin the verdict: primary direction is **client-side VTE (raw byte forwarding)**; fallback is daemon-side VTE with cell diffs (WezTerm-mux precedent). The rest of the milestone proceeds only on the spike's verdict.
- **`crates/tmux-core` — rift's own control-mode client**: notification parser (guards, `%output` octal decoding, layout/window/session/pane-mode notifications), command emission, connection state. A library the daemon consumes; tested against real-tmux fixtures, valid and malformed (constitution).
- **Protocol — terminal streaming message set** (`crates/protocol`): per-pane output streaming, session/window/pane layout state updates, and the client→daemon input / resize / tmux-command path. **Supersedes the placeholder sketches in `protocol.md`** (`pane_output` with `cells`, `input`, `resize_pane`, `tmux_command`): if the spike confirms client-side VTE, pane output carries **bytes**, not cells. Exact message granularity and the snapshot-on-attach shape are pinned in the protocol issue.
- **Daemon — own the tmux session**: spawn or attach `tmux -C` as a child process (local pipes — no PTY needed for control mode), one attach per connected rift client, route notifications onto the per-connection stream, own flow control on both legs (tmux→daemon and daemon→client).
- **App — swap the seam**: the terminal widget's byte source and the input/resize/command emission switch from termy's `TmuxClient` (flume bridge) to the daemon protocol; the rendering stack (`termy_terminal_ui` grid + `alacritty_terminal::Term`) stays untouched.
- **Legacy-path handling** per the gate decision (remove in-milestone vs. env-selected fallback until the milestone QA gate).

### Out of scope

- **Daemon-side VTE / cell-diff streaming** — the recorded fallback, built only if the spike fails the byte path; never built in parallel.
- **Pane-awareness plugins** (`crates/plugin-api`) — a later phase. Forward-note: the daemon owning the pane byte streams is exactly the feeding point those plugins will consume; this spec creates the stream, not the plugin surface.
- **tmux key-table mirroring and status-line mirroring** — phases 7 and 8, own DRAFT specs; this spec moves the existing interaction model, it does not extend it.
- **Scrollback redesign** — `capture-pane` fetching keeps working through the moved seam (command emission), not redesigned.
- **Multiplexer features beyond what rift uses today** (floating panes, choose-mode rendering, …).
- **macOS** — unchanged deferral.

## Human prerequisites

None. tmux on the remote host is already required today; the daemon and its auto-deploy exist (scaffolding milestone). No new secrets, accounts, or provisioning.

## Constraints

- **The single-seam contract** (`architecture.md`, "tmux control-mode interaction model"): all input and command emission already flow through one narrow seam so this swap is a single-seam change. The client render pipeline (steps 4–6 of the rendering pipeline) is explicitly not part of the swap.
- **`crates/tmux-core` must be musl-clean and `gpui`-free** — it becomes a daemon dependency. termy's `TmuxClient` lives in `termy_terminal_ui`, which pulls `gpui`, and can therefore never run daemon-side — this is why rift writes its own control-mode state (the tech-debt row's plan).
- **Control mode has no formal spec** (`prior-art.md` caveat 8): the parser is written against the tmux wiki, man page, `control.c`, and `docs/tmux-reference.md`, and tested with real tmux output fixtures including malformed input — constitution: "parsers are tested with valid and malformed input".
- **Flow control on both legs, bounded buffers**: today the direct client sets `refresh-client -f pause-after=5`; after the swap the daemon owns that flag per attach and must also bound the daemon→client leg — the dispatch loop never blocks on a slow client, and no buffer grows without bound. Backpressure semantics are pinned in the daemon issue.
- **Per-client attach cardinality**: one daemon-side `tmux -C` attach per connected rift client. Collapsing to a shared attach would break tmux's native per-client size semantics (`window-size largest`, transient-resize note in `CLAUDE.md`) that the dogfooding channels depend on.
- **The protocol changes are additive and serialization-agnostic**; the superseded `protocol.md` placeholders are updated in the same change (`protocol.md` Rules).
- **The spike precedes commitment** — pre-recorded in the scaffolding spec; the tmux-core/protocol/daemon/app issues sequence behind its verdict.
- **Phase-queue sequencing**: the milestone's first issue carries the cross-milestone edge on the prior phase's last issue (#198), per the handover convention — the roadmap is the sequenced queue.
- **Dogfooding safety**: the stable channel is the daily driver and currently runs daemonless (`RIFT_DAEMON_BINARY` unset skips the daemon). The swap makes the daemon load-bearing for the terminal; the gate decision governs the transition so a broken streaming path can never strand the daily driver.
- `thiserror` in libraries, `anyhow` in the daemon binary; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **The daemon owns the tmux control-mode client**; the app stops running `tmux -CC` over an SSH PTY | Constraint-determined: the target architecture places the "tmux control mode client" in the daemon (`architecture.md` diagram); the tech-debt table commits the Phase 3 transport swap; the scaffolding spec built the transport seam for exactly this. | 2026-06-12 |
| **rift writes its own control-mode client in `crates/tmux-core`** | Constraint-determined: the daemon must be `gpui`-free and musl-clean; termy's `TmuxClient` lives in `termy_terminal_ui` (pulls `gpui`) and is structurally unusable daemon-side. The tech-debt row ("own control-mode state … Phase 3 transport swap") pre-commits this. References: `docs/tmux-reference.md`, iTerm2, `control.c`. | 2026-06-12 |
| **Primary direction: VTE stays client-side; the daemon forwards raw per-pane bytes** — the spike is the commitment gate; the recorded fallback is daemon-side VTE with cell diffs (WezTerm-mux precedent) | Spike-resolved by design (pre-recorded OPEN in `archive/spec-daemon-scaffolding.md`). Client-side is primary because it is the minimal seam change: the entire client render stack (`termy_terminal_ui` + `alacritty_terminal::Term`) already consumes raw bytes via the flume bridge, tmux already coalesces output and provides flow control, and raw ANSI bytes are the compact wire form. Daemon-side VTE would duplicate terminal state server-side and require a new cell-diff protocol — built only if the byte path fails the spike's latency/throughput bar. | 2026-06-12 |
| **Per-client tmux attach**: each connected rift client gets its own daemon-side control-mode attach | Constraint-determined: preserves tmux's native multi-client semantics (per-client size, `window-size largest`) that the dogfooding channels' mirrored stable/dev views rely on; a shared attach would couple the two instances' sizes. | 2026-06-12 |
| **Single-seam swap; the client render pipeline is untouched** | Constraint-determined: `architecture.md` records the seam precisely so this swap stays narrow; touching the render path would widen a transport change into a rendering rewrite. | 2026-06-12 |
| **tmux stays the engine** — this phase moves the attach point, it does not reinterpret the interaction model | `architecture.md` "tmux control-mode interaction model" (durable contract + exit criteria) and `vision.md` "tmux-native" are unchanged by this spec. | 2026-06-12 |
| **OPEN — resolved at the spec-acceptance gate**: fate of the legacy direct `tmux -CC` path — (a) remove in the same milestone (one path, no drift; stable adopts the daemon at the next promote), or (b) keep as an env-selected fallback until the milestone QA gate passes, removed in a follow-up chore | Genuinely open: neither precedent nor constraint settles it. This is daily-driver risk management — (a) is cleaner and avoids maintaining two transports; (b) keeps a known-good escape hatch while the new path proves itself in dogfooding, at the cost of a temporary second path. | — |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 6 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 6 — Terminal streaming)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with `crates/tmux-core` linked
- [ ] **Spike verdict recorded**: raw-byte forwarding measured against the direct path (interactive latency + flooding-pane throughput), numbers in the spike issue; the verdict (client-side VTE confirmed or cell-diff fallback taken) is in this spec's decision log
- [ ] Control-mode parser fixtures: command guards, `%output` octal decoding (including multi-byte UTF-8 split across notifications), layout/window/session/pane-mode notifications — valid and malformed inputs
- [ ] Integration test: a scripted real-tmux session driven through daemon + protocol — pane output arrives at a client-side `Term`, typed input round-trips to the pane, resize propagates (`refresh-client -C`)
- [ ] Flow-control test: a flooding pane keeps the UI responsive and other panes streaming; daemon and client buffers observably bounded
- [ ] Reconnect test: killing the SSH connection leaves tmux + daemon running; reconnect reattaches, snapshots, and resumes streaming
- [ ] Two simultaneous clients attach with independent sizes and mirrored views (dogfooding-channels semantics)
- [ ] Legacy-path handling matches the gate decision (removed, or env-selected fallback with a recorded removal issue)
- [ ] A `grep` confirms: no agent detection in the streaming path; `crates/tmux-core` pulls no `gpui`; the daemon pulls no `termy_terminal_ui`
- [ ] Milestone QA (dev channel): typing feel, fast scroll under load, pane split/zoom/resize interactions — visual acceptance against these scenarios

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Control-mode parser correctness — no formal spec exists | Write against tmux wiki / man / `control.c` / `docs/tmux-reference.md`; conformance fixtures captured from real tmux; malformed-input tests; iTerm2 as the reference consumer. |
| Latency or throughput regression vs. the direct `-CC` path | The spike is the commitment gate with pinned numbers before the milestone proceeds; the cell-diff fallback is recorded, not improvised. |
| Flow-control interplay: pause-after semantics per control client, plus a new daemon→client leg | The daemon owns the tmux-side flags per attach and bounds the protocol leg; flooding-pane test in Verification; backpressure semantics pinned in the daemon issue. |
| Breaking the daily driver (stable channel) with a load-bearing new transport | The gate decision governs the legacy-path transition; `just promote` only ships what the milestone QA gate accepted; dev channel takes the risk first. |
| UTF-8/octal decode edge cases (multi-byte characters split across `%output` notifications) | Dedicated parser fixtures for split sequences; the client `Term` already tolerates partial sequences (VTE is incremental). |
| The daemon becomes mandatory where it was optional (deploy/bootstrap friction) | Auto-deploy + reattach already exist and are live-validated (scaffolding #61/#62); the gate decision covers the transition window. |
| Scope creep into key-table / status-line mirroring | Hard boundary: phases 7/8 own those (DRAFT specs exist); this spec only moves the existing interaction model. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-12: Spec created from `/loopkit:plan` (loop mode — roadmap Phase 6). Recorded as constraint-/precedent-decided: daemon ownership of the control-mode client, rift's own `crates/tmux-core` client (tech-debt row), client-side VTE as the spike-gated primary with the cell-diff fallback, per-client attach cardinality, the single-seam swap, and tmux-stays-the-engine. The one genuinely-open decision — the fate of the legacy direct `-CC` path (remove in-milestone vs. env-selected fallback until milestone QA) — is flagged for the spec-acceptance gate.
