# Spec: Host resource telemetry core

> Status: DRAFT
> Created: 2026-07-11

The daemon samples the host's own CPU / memory / swap / load from `/proc` (via the
`sysinfo` crate) on a timer and pushes a new push-only `HostMetrics` message; the
app folds it into workspace state and renders an always-visible compact
`MEM% · CPU%` segment in the composite status line. This is the dogfooding root:
intensive sessions hit the host RAM limit and today the only way to see it is the
Windows Task Manager (which shows the WSL VM from outside as one opaque number) —
the daemon runs *inside* the host and reads `/proc` directly, so host resource
state becomes a reactive cockpit signal like git status and diagnostics already
are. Phase 43 is the foundation the sibling phases (44 pressure warning, 45
per-pane attribution, 46 detail + disk) build on.

## Outcome

- [ ] The daemon samples host CPU%, total/available RAM, total/used swap, and load
      average (1/5/15) from `/proc` via `sysinfo` on a fixed interval, and pushes a
      `DaemonMessage::HostMetrics` to every attached client — push-only, the same
      shape as `lsp_status` / `repo_state`. A freshly connected client receives the
      latest sample immediately (replayed once behind `welcome`), so the indicator
      appears without waiting a full interval.
- [ ] The composite status line (Phase 22) shows an always-visible right-side
      segment reading host `MEM% · CPU%` (RAM% computed from `MemAvailable`:
      `(mem_total − mem_available) / mem_total`). It is hidden until the first
      sample arrives (like the LSP dot before a server is known) and updates live
      as samples arrive. Phase 43 renders it in the neutral status-line color — the
      threshold coloring and the pressure warning are Phase 44.
- [ ] Host telemetry is a **daemon-global** signal, sampled **once** per daemon
      regardless of how many project-root contexts (sessions) are attached — it is
      the host, not any pane or root. It is pushed to every connection, and the
      daemon does no sampling while no client is connected (an idle daemon polls
      nothing).
- [ ] `protocol` gains the `HostMetrics` message and its payload types;
      `PROTOCOL_VERSION` bumps 12 → 13 (next free at merge) and the fingerprint
      test is re-pinned. `docs/protocol.md` documents the new push.
- [ ] The daemon stays a self-contained static musl binary and **pure-Rust /
      C-free** (`docs/constitution.md`): `sysinfo` on the Linux musl target reads
      `/proc` with no C dependency and needs no C cross-compiler. `cargo deny check
      licenses` passes (`sysinfo` is MIT).
- [ ] **Foundation docs ratified with this spec** (authored on the spec branch,
      accepted at the spec-acceptance gate): `docs/constitution.md`'s agent-agnostic
      principle is extended from "exactly two signals (PTY byte streams +
      filesystem events)" to admit **host resource state (`/proc`)** as a third
      agent-agnostic host observable; `docs/architecture.md`'s "two universal
      signals" section gains the third signal and a short "host resource telemetry
      (daemon-global signal)" subsection, and its technology map lists `sysinfo`.

## Scope

### In scope

- **`crates/protocol`**: a new push-only variant
  `DaemonMessage::HostMetrics { cpu, mem_total, mem_available, swap_total,
  swap_used, load, cpu_count }` (wire tag `host_metrics`, `snake_case`), modeled on
  the existing push-only decoration messages (`LspStatus` `lib.rs:568`, `RepoState`
  `lib.rs:525`). `load` is a small typed `LoadAverage { one, five, fifteen }`
  (f64), mirroring how `RepoState` nests `AheadBehind`. Field types: `cpu: f32`
  (0.0–100.0 aggregate), `mem_total`/`mem_available`/`swap_total`/`swap_used: u64`
  bytes, `cpu_count: u32`. `PROTOCOL_VERSION` 12 → 13 (next free at merge),
  fingerprint re-pinned; serde round-trip tests for the message and `LoadAverage`.
- **`crates/daemon`**: a **daemon-global** resource sampler. One persistent
  `sysinfo::System`, refreshed on a `tokio::time::interval`
  (`HOST_METRICS_INTERVAL`, 2 s) inside a single task spawned at the daemon-process
  level (`serve_uds` — the long-lived shared daemon — and the single-connection
  `serve`), NOT per context. The tick's `sysinfo` refresh runs under
  `spawn_blocking` (it does `/proc` reads), then the sample is broadcast on a
  daemon-global bus and cached in a daemon-global `watch::Sender<Option<HostMetrics>>`.
  Each `serve_connection` subscribes to that global bus (an added `select!` branch
  alongside its per-context `events` bus, `lib.rs:1318`) and writes the frames to
  its socket, and on `Hello` replays the latest cached sample right after
  `write_snapshot` (`lib.rs:1183`), so a fresh client sees current metrics at once.
  The sampler is **connection-gated**: a process-global connection count (the accept
  loop already emits connect/disconnect via `KeepWarmEvent`, `lib.rs:2178`) gates
  sampling, so an idle daemon with zero clients polls nothing.
- **`crates/daemon` dependency**: add `sysinfo` — declared once in the root
  `[workspace.dependencies]` and referenced `sysinfo.workspace = true` in
  `crates/daemon/Cargo.toml`, with `default-features = false, features =
  ["system"]` (only CPU / memory / load; not disk/network/component — Phase 46 adds
  the disk feature). Reuse the version already resolved in `Cargo.lock` (0.31.x) to
  avoid a second `sysinfo` build. `sysinfo` (MIT) passes `cargo deny check
  licenses`; the daemon's own musl build graph excludes the app/gpui tree, so it
  gets only the `["system"]` feature.
- **`crates/app`**: a `HostMetrics` router arm in `consume_daemon_messages`
  (`main.rs:2734`, mirroring the `LspStatus` arm `main.rs:2858`) routing to a new
  channel; a `host_metrics: Option<HostMetrics>` field on `WorkspaceView`
  (`workspace.rs:476`, mirroring the `lsp` map field), fed by a fold spawn loop
  (mirroring the `lsp_status_rx` loop `workspace.rs:979-996`) with its
  `WorkspaceChannels` receiver + main.rs tx (mirroring `workspace.rs:288` /
  `main.rs:110/930/1037`).
- **`crates/app` status line**: a `MEM% · CPU%` segment added to the right group of
  the composite status line (`status_bar.rs:239-247`, before the clock), following
  the diagnostic-count / LSP-dot segment template (`status_bar.rs:202-227`,
  `count_segment` `:308`). A `metrics_text(...)` formatting helper beside
  `line_totals_text` / `cursor_text` (`status_bar.rs:112-131`) with unit tests. A
  new `StatusLineModel` field (`status_bar.rs:43-67`) populated at the assembly site
  (`workspace.rs:2360-2372`) from `WorkspaceView.host_metrics`. Rendered in the
  neutral status-line color (`theme.muted_foreground` / `theme.foreground`) — no
  threshold coloring in Phase 43.
- **`docs/protocol.md`**: a "Host metrics" push section and a `version 13` history
  line (documenting the shipped wire contract).
- **Foundation docs (authored on the spec branch, ratified at the acceptance
  gate)**: `docs/constitution.md` + `docs/architecture.md` per the Outcome's last
  item.

### Out of scope

- **The pressure warning (Phase 44)** — threshold coloring of the indicator, a
  proactive warning toast, `MemAvailable`/swap/load thresholds, and PSI
  (`/proc/pressure/memory`) as an optional enhancement. Phase 43 ships the neutral
  indicator and the data (`mem_available`, `swap_*`, `load`) the warning will read;
  it does **not** decide "when is it bad."
- **Per-pane / agent attribution (Phase 45)** — `pane_pid` → `/proc` process-subtree
  RSS/CPU roll-up and the per-pane popover. Phase 43 is host-aggregate only.
- **Detail popover, sparkline history, disk headroom (Phase 46)** — the memory
  breakdown (cached/buffers), load/uptime/cores detail view, client-side history
  ring buffer, and the `sysinfo` disk feature.
- **User-configurable sampling interval / a settings surface.** The interval is a
  compile-time constant; a settings knob is deferred.
- **Non-Linux host metrics parity.** The daemon targets Linux (musl) and reads
  `/proc`; load average is a Unix concept. No Windows/macOS daemon exists to spec.

## Constraints

- **musl-static, pure-Rust / no-C daemon (`docs/constitution.md`).** `sysinfo` on
  `x86_64-unknown-linux-musl` reads `/proc` in pure Rust (its Windows/macOS
  backends are `cfg`-gated out; the Linux path uses the `libc` crate's bindings, no
  C compilation), so the daemon musl build needs no C cross-compiler — verified by
  the existing `daemon-musl` CI job. This is the same bar that ruled out
  `git2`/`libgit2`.
- **Host telemetry is host-global, not per-context.** Every existing daemon push
  (`worktree` / `git` / `diagnostics` / `lsp_status`) is per-root **context** state
  (Phase 35: one reactive context per project root). Host CPU/RAM is a property of
  the **machine**, not of any root — so it is sampled once at the daemon-process
  level and pushed to all connections, NOT once per context (which would read
  `/proc` redundantly N times and mismodel a global as per-root). This is the
  central design decision; see Prior decisions.
- **Push-only, `welcome`-replayed (`docs/protocol.md`).** `HostMetrics` is a
  push-only decoration message like `lsp_status`; it is not requested. The latest
  sample is cached and replayed to a newly connected client behind `welcome`, so
  the reactive-signal contract (a fresh client sees current state at once) holds.
  `protocol` additions are a deliberate API change (`PROTOCOL_VERSION` bump +
  fingerprint re-pin).
- **Never block the dispatch loop.** The `sysinfo` refresh runs under
  `spawn_blocking` (it does syscalls / `/proc` reads), matching the codebase's
  "async for I/O, `spawn_blocking` for blocking work" discipline
  (`docs/constitution.md`).
- **Frugal on the shared host.** The interval is 2 s and sampling is
  connection-gated (zero clients → no polling), so telemetry adds negligible load
  to the shared WSL dev host.
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step runs. The exploratory Paper sketch `Host Telemetry —
  Sparring Options` (roadmap seed) informed the indicator's shape; the durable
  visual contract for the segment is authored against the existing Cockpit status
  line during implementation / visual-QA, reusing the established status-line
  segment styling.

## Prior art

- **[Host resource telemetry — prior-art index (Phases 43–46)](prior-art.md#host-resource-telemetry--prior-art-index-phases-4346)**
  — the per-phase index. For Phase 43 specifically: `sysinfo` (Category 11 #1) is
  the adopted metrics dependency (reuse; MIT; instantiate once, two-sample CPU%,
  hold state across ticks); `ClientMessage/bottom` (Category 11 #2) is the
  reference for metric selection and sampling cadence; the status-line segment
  reuses rift's own Phase-22 composite status line.
- **[Category 11: Host Resource Monitoring](prior-art.md#category-11-host-resource-monitoring)**
  — the full entries (sysinfo, bottom, btop, PSI, tmux-task-monitor) and the AVOID
  notes (no embedded TUI monitor; no agent-name labeling).
- **`docs/protocol.md` — Language server health (`lsp_status`)** — the exact
  push-only + `welcome`-replay precedent this message copies (keyed decoration,
  push-only, replayed once per known server behind `welcome`).

## Human prerequisites

- **none.** `sysinfo` is a crate dependency named in this spec (installed
  autonomously per the workflow autonomy grant; `cargo deny check licenses` gates
  it). No secrets, no external provisioning, no dashboard configuration. Every
  target host (WSL2, VPS) exposes `/proc/meminfo`, `/proc/stat`, and
  `/proc/loadavg` — no host setup is required (unlike PSI, which Phase 44 gates on
  file existence).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Host telemetry is a **daemon-global** signal — one sampler per daemon process, pushed to all connections — NOT per-root context state | Host CPU/RAM/swap/load is a property of the machine, not of any project root; a per-context sampler (the simpler wiring, reusing `Core`/`write_snapshot`) would read `/proc` redundantly once per attached context and mismodel a global as per-root. The daemon-global sampler + a global bus/`watch` alongside the per-context bus is localized and additive (no change to `Core`), and matches the ratified architecture note | 2026-07-11 |
| **Push-only** `HostMetrics`, replayed once behind `welcome` (not request/response) | Identical to `lsp_status` / `repo_state` — a reactive decoration signal; the client never requests it, and a fresh client gets the latest sample immediately via the replay hook (`write_snapshot`, `lib.rs:1183`) | 2026-07-11 |
| Sampling interval **2 s** (`HOST_METRICS_INTERVAL` constant), **connection-gated** | An ambient indicator, not a live graph; 2 s is frugal on the shared WSL host and near htop's default. Gating on ≥1 live connection means an idle daemon polls nothing | 2026-07-11 |
| `HostMetrics` carries the **full host-aggregate sample** (cpu, mem_total, mem_available, swap_total, swap_used, load 1/5/15, cpu_count), though Phase 43's UI renders only RAM% · CPU% | One `sysinfo` refresh yields all of these together; shipping the coherent host sample now means Phase 44 (needs mem_available + swap + load) and Phase 46 (load + cores) consume existing fields instead of forcing a `PROTOCOL_VERSION` bump per sibling phase (each bump is a strict-equality break + fingerprint re-pin + redeploy). Phase 44 then adds only optional PSI stall fields; Phase 46 adds memory-breakdown + disk | 2026-07-11 |
| RAM% = `(mem_total − mem_available) / mem_total` (MemAvailable-based), CPU% = `sysinfo` aggregate | `MemAvailable` is the meaningful "how much is really free" figure and is the same basis Phase 44's portable pressure baseline uses — one definition of "RAM used" across the milestone | 2026-07-11 |
| Phase 43 indicator is **neutral-colored** (no threshold/pressure coloring) | Clean 43/44 split: 43 = "see the number", 44 = "know when it's bad" (threshold coloring + toast + PSI). The three-state color legend in the Paper sketch spans the milestone, not this phase | 2026-07-11 |
| Indicator **hidden until the first sample**, right group, before the clock | Mirrors the LSP dot (absent until a server is known); `Option<HostMetrics>` renders nothing until populated. Placement follows the sketch (right-side ambient info) | 2026-07-11 |
| `sysinfo` with `default-features = false, features = ["system"]`, workspace-declared, version reused from `Cargo.lock` (0.31.x) | Minimal surface (CPU/mem/load only — disk is Phase 46), musl-clean, and reusing the resolved version avoids a second `sysinfo` build | 2026-07-11 |
| **Foundation ratification**: extend the "exactly two signals" principle to three (add host resource state / `/proc`) in `docs/constitution.md` + `docs/architecture.md`, authored on this spec branch | The roadmap seed flagged this Phase-43 foundation impact; `/proc` is a genuinely new signal source (neither a PTY byte stream nor a filesystem-change event), still fully agent-agnostic (host-global, `/proc` knows nothing about agents). Ratified at the spec-acceptance gate, never edited from the roadmap | 2026-07-11 |
| OPEN — ratify the exact constitution/architecture wording of the third-signal amendment | resolved at the spec-acceptance gate | — |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: [Phase 430 — Host resource telemetry core](#) (created at the
  acceptance gate)
- Issues: created from this spec once merged — `protocol` (HostMetrics + version
  bump), `daemon` (global sampler + push + replay + `sysinfo` dep), `app` (ingest +
  workspace state), `app` (status-line segment). Dependency edges in the issue
  bodies.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace
      excluding `rift-app`); `app-check` compiles `rift-app`; the `daemon-musl` job
      builds the daemon for `x86_64-unknown-linux-musl` **with no C cross-compiler**
      (pure-Rust, C-free) with `sysinfo` added.
- [ ] `cargo deny check licenses` passes with `sysinfo` on the daemon graph.
- [ ] `protocol`: `HostMetrics` + `LoadAverage` land; `PROTOCOL_VERSION` bumps to 13
      (next free at merge); the fingerprint test passes re-pinned; `HostMetrics`
      round-trips serde (full payload) and the wire tag is `host_metrics`.
- [ ] Daemon unit/integration: the sampler produces a plausible sample (mem_total >
      0, cpu in 0..=100, load fields present) from a real `sysinfo` read; the sample
      is broadcast on the global bus and cached for replay; a freshly handshaking
      connection receives one `HostMetrics` right after `welcome`; with zero
      connections the sampler does not poll.
- [ ] App: a `HostMetrics` push updates `WorkspaceView.host_metrics` and the status
      line re-renders; `metrics_text` unit tests cover formatting (e.g. rounding,
      the `MEM% · CPU%` layout); the segment is absent before the first sample.
- [ ] Behavioural (dev-channel QA): connect and confirm the status line shows a live
      `MEM% · CPU%` that tracks the host — cross-check against `free -m` / `top` on
      the host, and against the Windows Task Manager's WSL figure (the dogfooding
      motivator); the number moves under load (e.g. run a `cargo build` in a pane).
      Two channels (stable + dev) attached to the one daemon both show the same
      figure (one daemon-global sample).
- [ ] The daemon binary embeds **no C dependency** for telemetry (`sysinfo`'s
      Windows/macOS-only C crates absent from the musl `Cargo.lock` graph); the
      sampler adds no measurable idle load (2 s cadence, connection-gated).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `sysinfo` CPU% needs two samples spaced ≥ `MINIMUM_CPU_UPDATE_INTERVAL`; the first sample's CPU% is 0/garbage | Prime the persistent `System` with an initial refresh at sampler start; the first *emitted* sample may carry `cpu = 0` until the second tick — acceptable, the indicator settles within ~1 interval. Note in the daemon module. |
| A daemon-global bus reaching every connection is a new concept (today each connection drains only its per-context bus) | It is small and additive: one `broadcast::Sender<DaemonMessage>` + one `watch<Option<HostMetrics>>` created at the daemon-process level, one extra `select!` branch + one replay write per connection; `Core` and the per-context machinery are untouched. |
| `sysinfo` pulls a C-compiled crate on musl, breaking the pure-Rust/no-C rule | The Windows (`windows`, `ntapi`) and macOS (`core-foundation-sys`) deps are `cfg`-gated off the Linux target; the Linux path is `/proc` + `libc` bindings (no C build). Verified by the `daemon-musl` CI job and `cargo deny`. If a C dep appears, park with `blocked:human` — do not add a C toolchain to the daemon build. |
| Feature unification pulls extra `sysinfo` code into the daemon (via the app's transitive `sysinfo`) | The daemon musl build (`-p rift-daemon`) is a separate graph that excludes gpui/the app, so only the daemon's `["system"]` feature applies there. |
| Two dogfooding channels on one daemon could show divergent numbers | The daemon-global single sampler guarantees one sample fanned out to all connections — both channels render the identical figure (a verification item). |

## Decision log

- 2026-07-11: Spec drafted from the Phase-43 seed. Codebase mapped: push-only bus
  + `welcome`-replay (`write_snapshot`, `lib.rs:1183`), the `Daemon::run` timer
  seam, the composite status-line segment template (`status_bar.rs`), and the
  client ingest path (`main.rs:2734` → `WorkspaceView`). Central decision:
  host telemetry is a **daemon-global** signal (one sampler, all connections), not
  per-context. One open item — the exact constitution/architecture wording of the
  third-signal amendment — carried to the acceptance gate for ratification.
