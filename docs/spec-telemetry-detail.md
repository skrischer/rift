# Spec: Telemetry detail + disk headroom

> Created: 2026-07-11

Rounds out the host-telemetry block with the "look closer" surfaces: a detail view
of the host sample (memory breakdown used / cached / buffers / available, load
1·5·15, uptime, cores), a client-side sparkline of recent MEM/CPU history (the trend
toward the limit), and a project-filesystem disk-headroom indicator (the worktree
`target/` dirs are heavy). Builds on the Phase-43 core: `load` and `cpu_count` are
already on the wire, the sparkline is a pure client ring buffer over the existing
push, and only the memory breakdown + uptime + disk need new daemon reads.

## Outcome

- [ ] A **detail view** of the current host sample shows the memory breakdown (total,
      used, available, plus cached + buffers), swap, load average (1 / 5 / 15), uptime,
      and core count — reading `load` and `cpu_count` already carried by `HostMetrics`
      (Phase 43) and the new breakdown / uptime fields. Rendered from pushed data; no
      new request path for it.
- [ ] A **sparkline** in the status area shows recent MEM% (and CPU%) history — the
      trend toward the limit — as a **pure client-side ring buffer** over the existing
      2 s `HostMetrics` pushes; **no protocol or daemon change** for the history. The
      retention window is a client constant (see Prior decisions).
- [ ] A **disk-headroom indicator** shows free / total for the relevant filesystem
      (which filesystem — the session `@root` mount vs the daemon's own — is resolved at
      the spec-acceptance gate), so a filling project disk (heavy worktree `target/`
      dirs) is visible before a write fails.
- [ ] `protocol` gains the daemon-global memory-breakdown + uptime fields on
      `HostMetrics`, and the disk field(s) whose shape depends on the disk-filesystem
      decision; `PROTOCOL_VERSION` bumps to the next free value at merge and the
      fingerprint test is re-pinned. `docs/protocol.md` documents the additions.
- [ ] The daemon stays pure-Rust / C-free static musl: the breakdown comes from a
      small `/proc/meminfo` read (Cached / Buffers, which `sysinfo` does not expose),
      uptime from `sysinfo`, and disk from `sysinfo`'s `disk` feature — a **named
      feature addition** to the existing `sysinfo` dependency (no new crate).
      `cargo deny check licenses` is unaffected.

## Scope

### In scope

- **`crates/protocol`** (daemon-global, on `HostMetrics`): additive fields
  `mem_cached: u64`, `mem_buffers: u64` (bytes, from `/proc/meminfo`), and
  `uptime_secs: u64`. `load` (`LoadAverage`) and `cpu_count` are **already present**
  (Phase 43) and are reused unchanged. `PROTOCOL_VERSION` bumped to the next free value
  at merge, fingerprint re-pinned; serde round-trip + malformed tests extended.
- **`crates/protocol`** (disk — shape conditional on the gate decision): either
  daemon-global fields `disk_total: u64` / `disk_available: u64` on `HostMetrics` (if
  the indicator tracks the **daemon's own** filesystem), **or** a **per-connection**
  disk push keyed to the connection's `@root` mount (if it tracks the **session
  `@root`** filesystem — `@root` is per-connection, #737, so this cannot ride the
  daemon-global `HostMetrics`; it follows the per-connection transport pattern Phase 45
  established). The gate resolves this **before** the protocol issue is cut.
- **`crates/daemon` memory breakdown + uptime**: in the existing sampler tick, read
  `Cached` + `Buffers` from `/proc/meminfo` (a small dedicated parse — `sysinfo`
  exposes total / free / available / used but not cached / buffers) and `sysinfo`'s
  `System::uptime()`, and populate the new `HostMetrics` fields. Runs in the existing
  `spawn_blocking`.
- **`crates/daemon` disk**: add `"disk"` to the `sysinfo` features (workspace
  `sysinfo` dep, `["system", "disk"]`); refresh `Disks` on the sampler tick (disk
  usage changes slowly, so the 2 s cadence is ample) and report free / total for the
  chosen filesystem — for `@root`, resolve the mount whose `mount_point` is the longest
  prefix of the connection's `@root`. Per-connection or daemon-global per the gate.
- **`crates/app` detail view**: render the memory breakdown + swap + load 1/5/15 +
  uptime + cores from the pushed sample. Surface (a hover card on the `MEM% · CPU%`
  indicator vs a section added to the Phase-45 breakdown popover) resolved at the gate.
- **`crates/app` sparkline**: a fixed-length client ring buffer of recent samples
  (MEM% and CPU%), rendered as a small sparkline in the status area / detail view. Pure
  client state fed by the existing host-metrics fold loop; nothing on the wire.
- **`crates/app` disk indicator**: a `DISK <n>%` (used) status segment beside the
  MEM/CPU segment, reading the pushed disk field(s); hidden until the first disk sample.
- **`docs/protocol.md`**: extend the "Host metrics" push section with the breakdown /
  uptime / disk fields and a next-version History line.

### Out of scope

- **Per-pane disk or per-pane history (Phase 45 is per-pane; this is host / project
  level).** The sparkline and disk indicator are host / project-filesystem scope, not
  per-pane.
- **Multi-filesystem / all-mounts disk view.** One filesystem (the chosen one), not a
  `df -h` of every mount. A full disk browser is out.
- **Configurable retention / sampling / a settings surface.** The sparkline window and
  cadence are compile-time constants; a settings knob is deferred.
- **Alerting on disk / uptime.** Phase 44 owns pressure warnings (memory); a
  disk-full warning colour is a possible later add, not this phase (the indicator is
  neutral, like Phase 43's).
- **Historical persistence.** The sparkline is an in-memory ring buffer for the live
  session; no on-disk history.

## Constraints

- **Builds on the Phase-43 core; `load` + `cpu_count` are already on the wire.** The
  detail view and sparkline reuse existing `HostMetrics` fields; only the memory
  breakdown, uptime, and disk are new daemon reads. No change to the sampler cadence or
  the daemon-global host-aggregate model.
- **Sparkline is pure client-side.** History is a client ring buffer over the existing
  push — the reactive-signal model keeps derived history in the client, not the daemon
  (`docs/constitution.md`); nothing new on the wire for it.
- **Disk-of-`@root` is per-connection.** `@root` is a per-connection resolved value
  (#737, the Attach seam); the daemon-global `HostMetrics` cannot carry a per-session
  disk figure. If the gate picks `@root`, the disk field is a per-connection push (the
  Phase-45 per-connection transport pattern — each `serve_connection` computes and
  sends its own); if it picks the daemon's own fs, it rides `HostMetrics`
  daemon-globally. This is the reason the disk protocol shape is gate-conditional.
- **Pure-Rust / no-C daemon.** The breakdown is a `/proc/meminfo` read + a small
  parser (Cached / Buffers are always present on Linux — no file-gating, unlike PSI);
  uptime + disk come from `sysinfo` (the `disk` feature is pure-Rust `/proc` /
  `statvfs`-backed on Linux, no C). Adding the `disk` feature keeps the musl build
  C-free; `cargo deny` is unaffected (same crate).
- **Named dependency-feature change.** `sysinfo`'s `disk` feature is named here, so the
  autonomy grant covers enabling it; it is a feature of the already-approved `sysinfo`
  crate, not a new dependency.
- **Never block the dispatch loop.** The meminfo read + disk refresh run in the
  sampler's existing `spawn_blocking`.
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step; the roadmap-seed Paper sketch informs the detail/sparkline
  shape, and the durable visual contract is the existing status line + Phase-45 popover
  styling, verified at the milestone visual-QA gate.

## Prior art

- **[Host resource telemetry — prior-art index (Phases 43–46)](prior-art.md#host-resource-telemetry--prior-art-index-phases-4346)**
  — the Phase-46 row: `ClementTsang/bottom` time-series widgets (sparkline / history
  UX), `aristocratos/btop` detail panels (the breakdown layout), and `sysinfo` `Disks`
  for project-FS headroom; verdict "reference (history + breakdown UX) / reuse
  (`sysinfo` disks); history is a client-side ring buffer."
- **[Category 11: Host Resource Monitoring](prior-art.md#category-11-host-resource-monitoring)**
  — entries #2 (`bottom`: metric selection + time-series widgets) and #1 (`sysinfo`:
  `Disks` API, memory fields; note it does **not** expose cached / buffers — the reason
  for the small `/proc/meminfo` read).
- **rift's own Phase-43 core** ([archive/spec-host-telemetry.md](archive/spec-host-telemetry.md))
  — the `HostMetrics` push (already carrying `load` + `cpu_count`), the sampler, and
  the status-line segment the detail / sparkline / disk surfaces extend.
- **rift's own Phase-45 per-connection transport** ([spec-pane-attribution.md](spec-pane-attribution.md))
  — the pattern the `@root` disk push follows if the gate picks the session filesystem
  (each `serve_connection` computes + sends its own, not on the daemon-global bus); the
  breakdown popover the detail view may extend.

## Human prerequisites

- **none.** `sysinfo`'s `disk` feature is a feature of the already-named dependency
  (covered by the autonomy grant); the `/proc/meminfo` read needs no crate. No secret,
  no external provisioning. `/proc/meminfo`, `/proc/uptime`-equivalent (`sysinfo`), and
  the filesystem stat are present on every target host.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The sparkline history is a **pure client-side ring buffer** — no protocol / daemon change | Derived history over an already-pushed signal belongs in the client (reactive model); the daemon stays a stateless sampler. The client already receives 2 s `HostMetrics` pushes and folds them into `WorkspaceView` | 2026-07-11 |
| `load` (1/5/15) and `cpu_count` are **reused from the existing `HostMetrics`**, not re-added | Phase 43 deliberately shipped the full host sample so sibling phases consume existing fields without a redundant bump; the detail view reads them directly | 2026-07-11 |
| Cached / buffers come from a **small `/proc/meminfo` read**, not `sysinfo` | `sysinfo` exposes total / free / available / used but **not** cached / buffers; a tiny dedicated parse (the same daemon-side `/proc` read pattern as Phase 44's PSI) is warranted. Unlike PSI, `Cached` / `Buffers` are always present on Linux — no file-gating | 2026-07-11 |
| Disk uses `sysinfo`'s **`disk` feature** (added to the existing dep); disk is sampled on the 2 s tick | `Disks` is behind the `disk` feature (not in today's `["system"]`); a named feature of an approved crate, musl-clean. Disk usage changes slowly, so the host cadence is ample and no separate gating is needed | 2026-07-11 |
| The disk indicator is **neutral-coloured** (no threshold warning) this phase | Consistent with Phase 43's neutral indicator; a disk-full warning colour is a possible later add (Phase 44 owns memory pressure). Keeps scope bounded | 2026-07-11 |
| OPEN — the **disk filesystem**: the session `@root` mount (per-connection, tracks the project fs where `target/` lives) vs the daemon's own filesystem (daemon-global, simpler) | resolved at the spec-acceptance gate — accuracy-vs-simplicity; this **sets the disk protocol shape** (per-connection push vs a daemon-global `HostMetrics` field), fixed before the protocol issue is cut. Recommended: `@root` | — |
| OPEN — the **detail + sparkline surface**: a hover card on the `MEM% · CPU%` indicator (independent of Phase 45) vs a section added to the Phase-45 breakdown popover (couples to Phase 45) | resolved at the spec-acceptance gate — a UX surface call; the sparkline placement (inline in the status area vs inside the detail) rides this choice | — |
| The sparkline retention window defaults to **~150 samples (~5 min at 2 s)** as a client constant | Enough to show a trend toward the limit without unbounded memory; tunable later. Not a gate decision — a bakeable default | 2026-07-11 |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: [Phase 460 — Telemetry detail + disk headroom](#) (created at the acceptance gate)
- Issues: created from this spec once merged — `protocol` (breakdown + uptime + disk
  fields + version bump), `daemon` (meminfo breakdown + uptime + `sysinfo` disk
  feature + disk read), `app` (detail view + sparkline + disk indicator). Dependency
  edges in the issue bodies; the disk-filesystem + surface decisions shape the daemon +
  app issues.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace excluding
      `rift-app`); `app-check` compiles `rift-app`; `daemon-musl` builds with no C
      cross-compiler with the `sysinfo` `disk` feature added.
- [ ] `cargo deny check licenses` passes (no new crate; a feature of the existing
      `sysinfo`).
- [ ] `protocol`: the breakdown / uptime (and disk-shape) fields land;
      `PROTOCOL_VERSION` bumps to the next free value; the fingerprint test passes
      re-pinned; `HostMetrics` round-trips serde and a malformed payload is rejected.
- [ ] Daemon unit test: the `/proc/meminfo` parser reads `Cached` + `Buffers` from a
      fixture (and tolerates missing lines); uptime is plausible (> 0); the disk read
      reports total / available for the chosen filesystem (for `@root`, the longest
      mount-point prefix of the root is selected).
- [ ] App: the detail view renders breakdown + swap + load 1/5/15 + uptime + cores from
      a pushed sample; the sparkline accumulates successive samples into a bounded ring
      buffer and renders a trend; the disk segment shows a plausible used %.
- [ ] Behavioural (dev-channel QA): the detail matches `free -m` (breakdown) and
      `uptime` on the host; the sparkline visibly trends as memory rises/falls under
      load; the disk indicator matches `df -h` for the chosen filesystem, and drops as a
      `cargo build` fills `target/`. Confirm the ring buffer is bounded (no unbounded
      growth over a long session).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Disk-of-`@root` is per-connection and cannot ride the daemon-global `HostMetrics` | Made explicit: the disk protocol shape is gate-conditional; the `@root` branch reuses the Phase-45 per-connection push pattern (each `serve_connection` computes + sends its own), the daemon-fs branch rides `HostMetrics`. Resolved before the protocol issue is cut. |
| `/proc/meminfo` field names differ / a field is absent | The parser matches `Cached:` / `Buffers:` by key and tolerates absence (yields 0 for a missing field), never panics; unit-tested with a fixture missing a line. |
| The `sysinfo` `disk` feature drags in a C dep on musl | The Linux disk backend is `/proc` / `statvfs` via `libc` bindings (no C build), same as the `system` feature; verified by the `daemon-musl` job and `cargo deny`. If a C dep appears, park `blocked:human` — do not add a C toolchain. |
| The sparkline ring buffer grows unbounded over a long session | A fixed-length ring buffer (default ~150 samples); a QA item checks bounded memory. |
| Phase 46 duplicates or collides with the Phase-45 popover on the same indicator | The surface decision at the gate settles this: either a distinct hover card (independent) or an added section in the Phase-45 popover (the implementer merges), never two competing click-popovers. |
| Concurrent `PROTOCOL_VERSION` bumps across Phases 44/45/46 | "Next free at merge" + fingerprint re-pin: whichever lands takes the next integer, the others re-pin against the then-current value (standard strict-equality flow). |

## Decision log

- 2026-07-11: Spec drafted from the Phase-46 roadmap seed. Codebase mapped: `HostMetrics`
  already carries `load` + `cpu_count` (detail reuses them; no re-add); `sysinfo`
  exposes uptime + total/free/available/used but **not** cached/buffers (small
  `/proc/meminfo` read) and gates `Disks` behind the `disk` feature (named addition);
  `@root` is per-connection (#737), so disk-of-root follows the Phase-45 per-connection
  transport while the memory-breakdown/uptime fields stay daemon-global on `HostMetrics`;
  the sparkline is a pure client ring buffer over the existing push. Two open items — the
  disk filesystem (which sets the disk protocol shape) and the detail/sparkline surface
  (hover card vs Phase-45 popover section) — carried to the acceptance gate.
