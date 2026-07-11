# Spec: Per-pane resource attribution

> Created: 2026-07-11

When the host is under memory/CPU pressure, the cockpit answers *which pane is the
cause*: the daemon rolls up the `/proc` process subtree rooted at each tmux pane's
process (`pane_pid` → descendants' RSS + CPU via `sysinfo`) and pushes a per-pane
metrics list keyed by `pane_id`; the app surfaces it as a breakdown of the session's
panes ranked by resource use, each labelled by its `pane_current_command` — **never** by which agent runs there. Extends the Phase-43
host-telemetry model from a host aggregate to a per-pane attribution.

## Outcome

- [ ] The daemon attributes host resource use to tmux panes: for each pane it knows
      the pane's process id (tmux `#{pane_pid}`), walks the `/proc` process subtree
      rooted there, and sums the descendants' resident memory (RSS) and CPU — so a
      `cargo build` or an agent chewing RAM in a pane is attributable to *that pane*.
- [ ] The daemon pushes a per-pane metrics list keyed by `pane_id` (the id the client
      already has from the layout), each entry carrying the pane's rolled-up RSS + CPU
      and its agnostic label (`pane_current_command`); a **new per-connection**
      push-only message (each `serve_connection` sends its own session's list — **not**
      on the Phase-43 daemon-global broadcast bus), the same message *shape* as
      `HostMetrics`.
- [ ] The app surfaces a **per-pane breakdown** of the attached session's panes ranked
      by resource use (the "which pane is the cause" view), each row labelled by its
      `pane_current_command` and showing its RSS (and CPU). The exact surface (anchor +
      trigger) is resolved at the spec-acceptance gate.
- [ ] The attribution is **strictly agent-agnostic**: the pane label is
      `pane_current_command` only; no code path inspects pane content,
      matches an agent name, or special-cases any process (`docs/constitution.md`: "No
      agent detection"). The `/proc` subtree roll-up keys on the tmux pane's process,
      exactly as the Phase-43 constitution amendment already permits.
- [ ] `protocol` gains the per-pane metrics message; `PROTOCOL_VERSION` bumps to the
      next free value at merge and the fingerprint test is re-pinned. `docs/protocol.md`
      documents the new push. No new dependency (`sysinfo`'s `["system"]` feature
      already covers per-process RSS / CPU / parent-pid); `cargo deny` is unaffected.
- [ ] The daemon stays a pure-Rust / C-free static musl binary and adds negligible
      idle load: per-pane sampling is bounded (see the sampling-model decision) and the
      subtree roll-up is a single process snapshot per tick plus a cheap parent-pid
      index, never a per-pane `/proc` re-walk.

## Scope

### In scope

- **`crates/protocol`**: a new push-only `DaemonMessage::PaneMetrics { entries:
  Vec<PaneMetric> }` where `PaneMetric { pane_id: u32, rss: u64, cpu: f32, command:
  String }` — `pane_id` matches the layout's pane id, `rss` is the subtree resident
  bytes, `cpu` the subtree CPU% (0.0–100.0 × cores), `command` the agnostic
  `pane_current_command` label. `command` is re-shipped (the client already has it
  per `pane_id` from the layout) so each push is a **self-contained, sample-coherent**
  breakdown that renders correctly even if a pane vanishes from the layout between
  pushes. Modeled on `HostMetrics` (push-only, `snake_case`,
  wire tag `pane_metrics`). `PROTOCOL_VERSION` bumped to the next free value at merge,
  fingerprint re-pinned; serde round-trip test for the message and a `PaneMetric`, and
  a malformed-input test.
  **Conditional on the sampling-model gate decision:** the *on-demand* branch also adds
  a client to daemon opt-in `ClientMessage` (there is no client request path today —
  the telemetry channel is push-only), so per-connection sampling activates only while
  a breakdown is open; the *always-on* branch adds no second message. The gate resolves
  this **before** the protocol issue is cut (issues are created post-merge), so the
  protocol issue ships the right surface.
- **`crates/daemon` pane pid**: extend the internal pane query (`LAYOUT_QUERY`,
  `terminal.rs:65`) with `#{pane_pid}`, inserted **before** the tab-tolerant trailing
  `window_name` field and bumping the `splitn(13, '\t')` parse in `parse_layout_line`
  (`terminal.rs:1116`) to `splitn(14)`. `pane_pid` is added to the daemon's internal
  `ParsedPaneLine` (`terminal.rs:1103`) — **not** to the wire `PaneLayout`, which is
  today both the wire type *and* the daemon's only pane model; the client keys on
  `pane_id` and never needs the host pid, so the wire type is unchanged (no
  layout-format version concern). The `terminal_task` surfaces a
  `pane_id → pane_pid` map for its connection's session to `serve_connection`.
- **`crates/daemon` shared process snapshot (daemon-global)**: the one place that is
  genuinely daemon-global. Once per tick, the shared sampler refreshes the process
  table (`sysinfo`, `["system"]` feature — already present; the current sampler
  refreshes only cpu+memory, so a per-tick **process** refresh is added — see the
  priming risk) and publishes a shared, immutable snapshot — a `parent_pid → children`
  index plus per-PID `{ rss, cpu }` — on a daemon-global `watch`, built **once** per
  tick. This is the only reused daemon-global piece besides the connection-gating
  counter; a single process refresh serves all connections.
- **`crates/daemon` per-connection roll-up + push**: **not** the Phase-43 broadcast
  bus and **not** its single global replay cache — those fan one identical value to
  every connection and would hand each connection another session's pane data (the
  leak the per-connection scope forbids) and cannot serve two channels attached to
  different sessions. Instead, each `serve_connection` reads the shared snapshot, and
  for **its own** session's panes (its `pane_id → pane_pid` map) DFS-sums each subtree
  from the snapshot's child index, builds **its own** `PaneMetrics`, and writes it to
  **its own** socket — a per-connection computation with a per-connection latest-value
  cache (compute-and-send as soon as it has both a layout and a snapshot; no
  cross-connection replay needed). Runs under the existing `spawn_blocking`. The
  *activation* of per-connection sampling (always-on vs on-demand while the breakdown
  is open) is resolved at the gate; see the sampling-model decision and its protocol
  consequence (the conditional client→daemon opt-in message).
- **`crates/app` ingest**: a `PaneMetrics` router arm in `consume_daemon_messages`
  (mirroring the `HostMetrics` arm) routing to a new channel; a
  `pane_metrics: Vec<PaneMetric>` (or `HashMap<pane_id, PaneMetric>`) field on
  `WorkspaceView`, fed by a fold spawn loop mirroring the host-metrics loop; `notify`
  on update.
- **`crates/app` breakdown surface**: a per-pane breakdown listing the attached
  session's panes ranked by RSS (then CPU), each row = label (`command`) + RSS + CPU. Rendered via the vendored `gpui-component` popover primitives. The
  anchor/trigger (the Phase-43/44 `MEM% · CPU%` status indicator made clickable, vs a
  per-pane-header badge, vs both) is resolved at the gate; the status segments are
  already click-dispatch capable (`status_bar.rs`).
- **`docs/protocol.md`**: a "Pane metrics" push section and a next-version History line.

### Out of scope

- **Cross-session / host-wide pane attribution.** v1 attributes the **attached
  session's** panes (what the user is looking at). Panes in *other* tmux sessions on
  the same host (another connection) are not rolled up into this connection's
  breakdown; the host aggregate (Phase 43) still reflects total load. Host-wide
  per-pane attribution is deferred.
- **Detail history / sparkline per pane, disk per pane (Phase 46).** Phase 45 is an
  instantaneous ranked breakdown, not a trend.
- **Killing / signalling a pane's process from the breakdown.** Read-only attribution;
  no process control this phase.
- **Threshold coloring / warning on a per-pane basis.** The pressure warning (Phase
  44) is host-level; per-pane rows are neutral. (A "this pane is the cause" emphasis on
  the row is a rendering nicety, not a new warning axis.)
- **Any agent detection.** The label is `pane_current_command`; no
  content inspection, no process taxonomy, no agent-name matching — a hard constraint,
  not a scope choice.

## Constraints

- **Strictly agent-agnostic (`docs/constitution.md`).** "attributing it to a pane keys
  on the tmux pane's process, never on which agent runs there" — the Phase-43
  amendment already ratified exactly this attribution. The label comes from tmux's own
  `pane_current_command` (the daemon already carries `current_command`,
  computed server-side, `PaneLayout`); rift never inspects pane content or names an
  agent. This is the AVOID note in the prior-art index made binding.
- **No foundation-doc change.** Host resource state (`/proc`) is already the third
  agent-agnostic signal (Phase 43), and its constitution wording already sanctions
  per-pane attribution keyed on the pane process. This phase ratifies nothing new — the
  acceptance PR carries only the spec + roadmap link.
- **Builds on the Phase-43 core, but per-pane data is per-connection, not on the
  global bus.** The reused pieces are the sampler **tick** (one process refresh added
  to it) and the **connection-gating counter**. `PaneMetrics` is **not** put on the
  Phase-43 `broadcast` bus and **not** in its single global welcome-replay `watch` —
  both fan one identical value to every connection, which would leak another session's
  pane data and cannot serve two channels on different sessions. The daemon-global part
  is only the shared process snapshot; the roll-up + push is per-connection (each
  `serve_connection` computes and sends its own session's `PaneMetrics`). The
  host-aggregate `HostMetrics` path is unchanged.
- **Subtree roll-up is O(processes) per tick, not O(panes × /proc walks).** Build the
  `parent_pid → children` index once from a single process snapshot, then sum each
  pane's subtree from that index. A process-table refresh is heavier than the Phase-43
  mem/cpu-only refresh, so per-pane sampling must be **bounded** (the gate decides
  always-on vs on-demand) to stay frugal on the shared WSL host.
- **Never block the dispatch loop.** The process refresh + roll-up runs under the
  sampler's existing `spawn_blocking`, matching the codebase discipline.
- **Pure-Rust / no-C daemon.** No new crate — `sysinfo`'s `["system"]` feature already
  exposes `Process` (RSS via `memory()`, `cpu_usage()`, `parent()`); `cargo deny` is
  unaffected.
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step runs; the exploratory Paper sketch from the roadmap seed
  informs the breakdown's shape, and the durable visual contract is authored against
  the existing status line / pane chrome and verified at the milestone visual-QA gate.

## Prior art

- **[Host resource telemetry — prior-art index (Phases 43–46)](prior-art.md#host-resource-telemetry--prior-art-index-phases-4346)**
  — the Phase-45 row: `YlanAllouche/tmux-task-monitor` (the exact precedent —
  `pane_pid` → `/proc` children, grouped per window / session), `aristocratos/btop`
  (process-tree roll-up UX), and `sysinfo` per-PID RSS/CPU + parent PID; verdict
  "reference (method + tree UX) — roll up the `pane_pid` subtree; agnostic label from
  `pane_current_command`, never agent detection."
- **[Category 11: Host Resource Monitoring](prior-art.md#category-11-host-resource-monitoring)**
  — entry #5 (`tmux-task-monitor`: `pane_pid` → `/proc/$pid/task/$tid/children`,
  recursive, grouped per window/session — the attribution method) and #1 (`sysinfo`
  per-process API), plus the AVOID note (no agent-name-based labeling of resource use).
- **rift's own Phase-43 core** ([archive/spec-host-telemetry.md](archive/spec-host-telemetry.md))
  — the daemon-global sampler, bus, welcome-replay, and connection-gating this phase
  extends; the push-only `HostMetrics` message `PaneMetrics` mirrors.
- **rift's own layout query** (`crates/daemon/src/terminal.rs` `LAYOUT_QUERY`) — the
  server-side `list-panes -F` the `#{pane_pid}` field is added to; `PaneLayout` already
  carries the agnostic `current_command` computed server-side.
- **rift's own clickable status segments** (`crates/app/src/status_bar.rs`) — status
  segments already dispatch clicks (window select); the breakdown-trigger reuses that.

## Human prerequisites

- **none.** No new dependency (`sysinfo` `["system"]` already covers per-process
  metrics), no secret, no external provisioning. Every target host exposes `/proc/<pid>`
  and tmux exposes `#{pane_pid}` — no host setup.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The pane label is **`pane_current_command` only** — never agent detection or content inspection | Binding constitution rule ("No agent detection") + the Phase-43 attribution amendment ("keys on the tmux pane's process, never on which agent runs there") + the prior-art AVOID note. tmux computes `current_command` server-side and the daemon already carries it in `PaneLayout`. A friendlier `pane_title` label is a possible future refinement (it needs an added `#{pane_title}` query field — not wired today) and is out of scope here | 2026-07-11 |
| **No foundation-doc change** — the per-pane attribution is already sanctioned by the Phase-43 constitution/architecture wording | Phase 43 pre-ratified "attributing it to a pane keys on the tmux pane's process"; `/proc` is already the admitted third signal. Only a `protocol` extension (the deliberate API addition) is needed | 2026-07-11 |
| `pane_pid` stays **daemon-internal**; the wire carries per-pane **results** keyed by `pane_id` | The client already has `pane_id` from the layout and matches on it; shipping `pane_pid` on the wire would bloat `PaneLayout` and raise a layout-format version concern for data the client never uses. The daemon queries `#{pane_pid}` and rolls up locally | 2026-07-11 |
| Subtree roll-up = **one process snapshot + a `parent_pid→children` index per tick**, then a per-pane DFS sum | `sysinfo` exposes each process's parent pid; building the child index once and summing each pane's subtree is O(processes)+O(subtree), far cheaper than re-walking `/proc` per pane. The prior-art (`tmux-task-monitor`) uses the same recursive-children method | 2026-07-11 |
| A short-lived child racing a sample is **tolerated** — it is simply in or out of that tick's snapshot | Attribution is an ambient sample, not accounting; a process that appears/vanishes between ticks shows up on the next sample or not at all. No attempt to catch sub-tick churn (which would need per-process accounting the feature does not warrant) | 2026-07-11 |
| v1 attributes the **attached session's** panes (per-connection), not host-wide | The breakdown answers "which of *my* panes is the cause"; the connection already tracks its session's layout, so per-connection keys naturally to what the user sees and avoids leaking other sessions' process data. The host aggregate (Phase 43) still shows total load; cross-session attribution is deferred | 2026-07-11 |
| OPEN — the **breakdown surface** (anchor + trigger): the `MEM% · CPU%` status indicator made clickable → a ranked pane list, vs per-pane-header badges, vs both | resolved at the spec-acceptance gate — a UX surface call; recommended option presented there | — |
| OPEN — the **sampling activation model**: always-on (process refresh + per-connection roll-up every tick) vs on-demand (only while a breakdown is open, via a new client→daemon opt-in `ClientMessage`) | resolved at the spec-acceptance gate. Cost driver is the per-tick process-table refresh (a few ms at 2 s — modest even continuously, cf. htop). **Always-on is the simpler default** (no new wire message, processes stay CPU-primed); on-demand saves the refresh when nobody is looking but adds the opt-in message and a ~1-tick stale first CPU frame. This choice sets the protocol surface (see the protocol scope) — resolved before the protocol issue is cut | — |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: [Phase 450 — Per-pane resource attribution](#) (created at the acceptance gate)
- Issues: created from this spec once merged — `protocol` (PaneMetrics + version bump),
  `daemon` (pane_pid query + subtree roll-up + push), `app` (ingest + breakdown
  surface). Dependency edges in the issue bodies; the surface + sampling-model
  decisions shape the daemon + app issues.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace excluding
      `rift-app`); `app-check` compiles `rift-app`; `daemon-musl` builds with no C
      cross-compiler (no new dependency).
- [ ] `protocol`: `PaneMetrics` + `PaneMetric` land; `PROTOCOL_VERSION` bumps to the
      next free value; the fingerprint test passes re-pinned; the message round-trips
      serde (wire tag `pane_metrics`) and a malformed payload is rejected.
- [ ] Daemon unit/integration: the subtree roll-up sums a synthetic
      `parent_pid→children` tree correctly (a pane pid with N descendants yields the
      summed RSS/CPU); an unknown/dead `pane_pid` yields a zero/absent entry; the label
      is the pane's `current_command`; per-connection sampling honours the chosen
      activation model (under on-demand, no process refresh while every breakdown is
      closed); a freshly activated sampler's first CPU frame may read ~0 (RSS is
      immediate) and settles within a tick.
- [ ] App: a `PaneMetrics` push updates `WorkspaceView` and the breakdown re-renders;
      the breakdown lists the session's panes ranked by RSS with the agnostic label;
      opening/closing it behaves per the chosen trigger.
- [ ] Agent-agnostic audit: grepping the diff for agent names returns nothing; the
      label derives solely from `pane_current_command`; no pane-content
      read anywhere in the path.
- [ ] Behavioural (dev-channel QA): run a memory/CPU hog in one pane (e.g. a
      `cargo build`) and confirm the breakdown ranks that pane top, labelled by its
      command, with an RSS that tracks `top`/`htop` for that process subtree; an idle
      pane ranks low. Confirm the daemon adds no measurable idle load when the
      breakdown is inactive (per the sampling model).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Process-table refresh every 2 s is heavier than Phase-43's mem/cpu-only refresh and could load the shared host | Bound per-pane sampling (the gate's on-demand/gated model); build the child index once per tick; keep the host-aggregate path unchanged. A QA item checks idle load when inactive. |
| A pane's subtree misses a process (short-lived child) or double-counts | The parent-pid index is built from one coherent snapshot; a DFS from `pane_pid` visits each descendant once. Sub-tick churn is explicitly tolerated (Prior decisions), not chased. |
| Attribution could drift toward agent-specific labeling | Hard constraint + an explicit agent-agnostic audit verification item; the label is tmux's server-side `current_command` only, and no pane content is ever read. |
| `pane_pid` unavailable or a pane with no live process | tmux always exposes `#{pane_pid}` for a live pane; a pane whose pid has no `/proc` entry yields an empty roll-up (zero), not an error. |
| A new wire message plus the Phase-44 bump could collide on version numbers | `PROTOCOL_VERSION` is taken as "next free at merge"; whichever of Phase 44 / 45 merges first takes the next integer and the other re-pins against the then-current value (the standard strict-equality flow). |
| Per-process `cpu_usage()` needs two process refreshes spaced ≥ `MINIMUM_CPU_UPDATE_INTERVAL` (same two-sample rule as Phase-43 global CPU); the Phase-43 sampler never refreshes processes | Add a per-tick **process** refresh so the subtree CPU settles; the first frame after activation shows CPU ≈ 0 for ~1 tick. RSS is instantaneous, so the primary RSS ranking is correct on the first frame; only CPU settles a tick later. Under always-on the processes stay primed continuously; under on-demand, prime on activation. |
| Per-connection data accidentally routed onto the daemon-global bus | Explicit constraint + daemon-scope wording: `PaneMetrics` is per-connection, computed and written by each `serve_connection` from the shared snapshot, never on the `broadcast` bus or the single global replay `watch`. |

## Decision log

- 2026-07-11: Spec drafted from the Phase-45 roadmap seed. Codebase mapped: the daemon
  `LAYOUT_QUERY` (`terminal.rs:65`) lacks `#{pane_pid}` (must be added, daemon-internal);
  `PaneLayout` already carries the agnostic `current_command` computed server-side;
  `sysinfo` `["system"]` already exposes per-process RSS/CPU/parent-pid (no new dep);
  the Phase-43 sampler/bus/gating is the reuse substrate; status segments are already
  click-capable for the breakdown trigger. Central decisions: daemon-internal `pane_pid`
  + per-pane results keyed by `pane_id`; one-snapshot parent-pid-index subtree roll-up;
  per-connection (attached-session) scope; strictly agnostic labels — no foundation
  change (Phase 43 already sanctioned pane attribution). Two open items — the breakdown
  surface (anchor/trigger) and the sampling activation model — carried to the acceptance
  gate.
- 2026-07-11 (spec review — REQUEST_CHANGES, addressed): the reviewer caught that the
  Phase-43 transport (a daemon-global `broadcast` bus + one global welcome-replay
  `watch`) cannot carry **per-connection** `PaneMetrics` — it would leak other sessions'
  pane data and can't serve two channels on different sessions, and the global sampler
  has no `pane_pid` knowledge. Reworked the daemon design: the daemon-global part is
  only a **shared process snapshot** (`parent_pid→children` index + per-PID rss/cpu on a
  `watch`, one process refresh per tick); each `serve_connection` surfaces its session's
  `pane_id→pane_pid` map (from `terminal_task`'s `LAYOUT_QUERY` parse) and does its own
  DFS roll-up + push to its own socket — never the bus. Also: the on-demand sampling
  branch is not protocol-neutral (it needs a client→daemon opt-in `ClientMessage`), so
  the protocol surface is now conditional on the gate answer and fixed before the
  protocol issue is cut. Folded non-blocking fixes: `splitn(13)→(14)` inserting
  `#{pane_pid}` before `window_name` and onto `ParsedPaneLine` (not the wire
  `PaneLayout`); a per-tick process refresh + the per-process CPU two-sample priming
  note; `pane_title` dropped as a hard label (needs an added query field — future
  refinement), label is `pane_current_command`; `command` re-ship justified as a
  sample-coherent snapshot.
