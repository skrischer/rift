# Spec: Memory-pressure warning

> Created: 2026-07-11

The cockpit proactively warns before the host wedges on memory: the Phase-43
`MEM% · CPU%` indicator recolours (neutral → warning → critical) and a one-shot
toast fires when host memory gets tight, driven by a portable pressure signal
(`MemAvailable` ratio + swap-in-use) that works on every host including WSL2, with
Linux PSI (`/proc/pressure/memory`) as an optional daemon-read precision
enhancement gated on file existence. Phase 43 shipped "see the number"; this phase
adds "know when it's bad." Builds on the Phase-43 host-telemetry core (daemon-global
`/proc` sampler + push-only `HostMetrics` + status-line segment).

## Outcome

- [ ] The Phase-43 status-line `MEM% · CPU%` segment renders in one of three states
      — **normal** (neutral `muted_foreground`), **warning** (`theme.warning`), and
      **critical** (`theme.danger`) — chosen by a client-side pressure level computed
      from the pushed sample. No hardcoded hex; the recolour reuses the existing
      semantic theme tokens the diagnostic/LSP segments already use.
- [ ] When the pressure level **rises** (normal → warning, warning → critical, or
      normal → critical) a single toast fires naming the condition (e.g. `Host memory
      low — 8% available`); it does **not** re-fire while the level holds, and it
      re-arms only after the level falls back to normal. Toasts use the vendored
      `gpui-component` notification surface (`window.push_notification`).
- [ ] The pressure level is computed on the **portable baseline** — `MemAvailable`
      ratio and swap-in-use ratio, both already on the wire since Phase 43 — so the
      warning works on every target host (VPS + the stock `microsoft-standard-WSL2`
      kernel, which ships no PSI). Separate enter/exit thresholds (hysteresis)
      prevent the indicator and toast from flapping around a boundary.
- [ ] Where the host exposes Linux PSI (`/proc/pressure/memory` exists), the daemon
      reads and pushes the memory-stall averages, and the client uses them to
      **escalate** the level (a real stall is stronger evidence than a static ratio).
      Where PSI is absent (WSL2), the field is `None` and the portable baseline alone
      drives the level — no degradation, no error.
- [ ] `protocol` gains the optional PSI payload on `HostMetrics`;
      `PROTOCOL_VERSION` bumps 13 → 14 (next free at merge) and the fingerprint test
      is re-pinned. `docs/protocol.md` documents the extension. The portable-baseline
      fields (`mem_available`, `swap_*`) need no protocol change — Phase 43 shipped
      them for exactly this.
- [ ] The daemon stays a pure-Rust / C-free static musl binary
      (`docs/constitution.md`): the PSI read is `std::fs` + a small dedicated parser,
      no new dependency, no C. `cargo deny check licenses` is unaffected (no new
      crate).

## Scope

### In scope

- **`crates/protocol`**: a new `MemoryPressure` payload type carrying the
  `/proc/pressure/memory` averages — `some_avg10`, `some_avg60`, `some_avg300`,
  `full_avg10`, `full_avg60`, `full_avg300` (all `f64`, the kernel's percent-stalled
  figures) — deriving `Copy` (six `f64`) so the app-local `status_bar::HostMetrics`
  that embeds it stays `Copy` — added to `DaemonMessage::HostMetrics` as a single
  field `psi: Option<MemoryPressure>` with `#[serde(default, skip_serializing_if =
  "Option::is_none")]` (intra-version optionality: a same-version PSI-less WSL2 host
  emits no `psi` key; the required `cpu` / `load` fields stay required, so the
  existing missing-required-field rejection test still holds). `LoadAverage`
  and the existing fields are unchanged. `PROTOCOL_VERSION` 13 → 14 (next free at
  merge), fingerprint re-pinned. serde round-trip tests for the message **with** and
  **without** `psi`, plus a `MemoryPressure` round-trip and a malformed-input test
  (mirroring the Phase-43 `HostMetrics` tests).
- **`crates/daemon`**: a `read_memory_pressure(path: &Path) -> Option<MemoryPressure>`
  helper — path-injectable so it is fixture-testable — reading the file and parsing
  its two `some` / `full` lines, returning `None` when the file is absent or
  unparseable. It parses the `avgN` tokens and **ignores the trailing `total=<µs>`
  counter** each line also carries (the `avgN` are the ready-to-use percentages). The
  PSI value is threaded into the pure builder as a parameter —
  `build_host_metrics_message(&System, Option<MemoryPressure>)` — keeping the builder
  pure/testable (its existing `test_build_host_metrics_message_produces_plausible_sample`
  is updated to pass the new arg). The file-existence check is resolved **once** at
  sampler start (PSI availability is a boot-time kernel property, not a per-tick
  change) and cached; the per-tick read runs inside the sampler's existing
  `spawn_blocking` alongside the `sysinfo` refresh. A small dedicated parser (sysinfo
  does **not** expose PSI) with unit tests over a real `/proc/pressure/memory`-shaped
  fixture and a malformed string. No new dependency.
- **`crates/app` pressure model**: widen the app-local `status_bar::HostMetrics`
  (`status_bar.rs`, the struct whose doc already flags "a later phase widens it")
  with the fields the computation reads — `swap_total`, `swap_used`, and
  `psi: Option<...>` — and update the fold-loop destructure at `workspace.rs:1026`.
  A pure `pressure_level(...)` function beside `metrics_text` returning a
  `PressureLevel { Normal, Warning, Critical }`, computed from the portable baseline
  (mem-available ratio + swap-used ratio) with enter/exit hysteresis, taking the
  previous level so the hysteresis band applies, and escalated by PSI when present.
  PSI escalation shape (default, tunable with the thresholds at the gate): a nonzero
  memory stall raises the level by one band — `some_avg10` above a small stall cutoff
  escalates a baseline `Normal` to `Warning`, and any `full_avg10 > 0` (the host is
  fully stalled on memory) escalates to `Critical`; PSI only ever raises, never
  lowers, the baseline verdict. Unit-tested across the bands, the hysteresis
  boundaries, and PSI-present vs absent.
- **`crates/app` recolour**: the `MEM% · CPU%` segment (`status_bar.rs`) takes its
  text colour from the level — `Normal → theme.muted_foreground`, `Warning →
  theme.warning`, `Critical → theme.danger` — replacing the unconditional
  `muted_foreground`. The level is stored on `WorkspaceView` and passed into the
  `StatusLineModel` at the assembly site (`workspace.rs`).
- **`crates/app` toast**: on an **upward** level transition, fire
  `window.push_notification((NotificationType::Warning | Error, message))`. The
  host-metrics fold loop (`workspace.rs:1020`, today a plain `cx.spawn`) is converted
  to `cx.spawn_in(window, ...)` + `update_in` — the exact pattern the sibling diff /
  nav loops right below it already use — so it has the `Window` handle
  `push_notification` needs. `WorkspaceView` tracks the previous level to detect the
  edge; the toast fires only on the rising edge and re-arms after a return to
  `Normal`. **Initial / replayed sample seeds silently:** because `HostMetrics` is
  welcome-replayed, a client (re)connecting while the host is already under pressure
  receives a `Warning` / `Critical` sample first; that first sample **establishes**
  the tracked level **without** firing a toast (the previous level is seeded from it),
  so a reconnect or a new window under sustained pressure does not spam — only a
  *subsequent* rise fires.
- **`docs/protocol.md`**: extend the "Host metrics" push section with the optional
  PSI payload and add a `version 14` History line.

### Out of scope

- **Per-pane / agent attribution (Phase 45)** — "which pane is the cause." Phase 44
  warns that the host is under memory pressure; it does not attribute it.
- **Detail popover, sparkline history, disk headroom (Phase 46)** — the memory
  breakdown, load/uptime/cores detail, client-side history ring buffer, and the
  disk-headroom indicator. Phase 44 shows a level, not a trend graph.
- **A CPU / load / scheduler-pressure warning.** This phase is *memory*-pressure
  specific. Load average is a different axis (CPU/scheduler contention, not RAM) and
  a "high load" warning would mislead when the host is CPU-bound but has ample RAM;
  the load figure surfaces in Phase 46's detail view, not as a Phase-44 trigger. PSI
  `cpu` / `io` files are likewise out — only `/proc/pressure/memory` is read.
- **User-configurable thresholds / a settings surface.** The thresholds are
  compile-time constants (tunable in a follow-up); no settings knob this phase.
- **Daemon-side level computation or a `PressureLevel` on the wire.** The daemon
  pushes raw signals (portable fields since Phase 43, PSI averages here); the client
  owns the UX policy (thresholds, hysteresis, toast). See Prior decisions.

## Constraints

- **Builds on the Phase-43 core, unchanged.** The daemon-global sampler,
  `HostMetricsBus`, welcome-replay, and connection-gating
  ([archive/spec-host-telemetry.md](archive/spec-host-telemetry.md)) are reused as-is;
  Phase 44 only adds a field to the sample and adds client-side interpretation. No
  change to the sampling cadence (2 s), the daemon-global model, or the replay hook.
- **No foundation-doc change.** Phase 43 already amended `docs/constitution.md` /
  `docs/architecture.md` to admit host resource state (`/proc`: CPU / memory / swap /
  load) as the third agent-agnostic signal; `/proc/pressure/memory` is a further read
  under that same admission, so this phase ratifies nothing new — the acceptance PR
  carries only the spec + the roadmap link, no foundation-doc edit.
- **Portable baseline is the guaranteed path; PSI is optional and file-gated.** The
  stock `microsoft-standard-WSL2` kernel ships without `CONFIG_PSI` (confirmed live
  in Phase-43 planning), so `/proc/pressure/memory` is absent on the developer's own
  host. PSI must never be the primary signal; it only sharpens the level where
  present. Gate strictly on file existence.
- **Client owns UX policy (`docs/constitution.md` reactive model).** Thresholds and
  hysteresis are stateful UX decisions over the sample stream; they live in the app
  beside the render, the same place diagnostic-count colouring and LSP-dot colouring
  already decide their colours from raw pushed data. The daemon stays the raw signal
  source.
- **Pure-Rust / no-C daemon (`docs/constitution.md`).** The PSI read is a `std::fs`
  read of a fixed two-line text file plus a hand-written parser — no new crate, no C.
  A dedicated parser is warranted here (unlike Phase 43's "no hand-rolled /proc
  parser" rule, which applied to metrics `sysinfo` already covers — `sysinfo` does
  not expose PSI).
- **Never block the dispatch loop.** The PSI file read joins the sampler's existing
  `spawn_blocking` refresh; no new blocking on the async path.
- **Strict-equality protocol versioning (`docs/protocol.md`).** Even an additive
  optional field bumps `PROTOCOL_VERSION` and re-pins the fingerprint (as Phase 43's
  12 → 13 did); a version-skewed client is redeployed by the Phase-20 negotiation, so
  a v13 client never has to parse a v14 sample.
- **Design phase not enabled.** `docs/design.md` does not exist, so no formal
  `/loopkit:design` step runs. The three-state colour legend in the exploratory Paper
  sketch `Host Telemetry — Sparring Options` (the Phase-43 roadmap seed, which spans
  the whole milestone, not just Phase 43) informs the warning/critical colours; the
  durable visual contract is the existing status-line segment styling plus the
  `theme.warning` / `theme.danger` tokens, verified at the milestone visual-QA gate.

## Prior art

- **[Host resource telemetry — prior-art index (Phases 43–46)](prior-art.md#host-resource-telemetry--prior-art-index-phases-4346)**
  — the Phase-44 rows: the **portable memory-pressure signal** (`MemAvailable` ratio +
  swap + load via `sysinfo`, the guaranteed path that works on WSL2) and **PSI as the
  optional stall-time enhancement** (`/proc/pressure/memory`, gate on file existence,
  never primary).
- **[Category 11: Host Resource Monitoring](prior-art.md#category-11-host-resource-monitoring)**
  — entry #4 (Linux PSI: the `some`/`full` stall-average format and the
  systemd-oomd / Netdata precedent for acting on memory pressure before an OOM; the
  reality-check that PSI is off on the stock WSL2 kernel) and #1 (`sysinfo`, already
  the metrics source; it provides `mem_available` / swap but **not** PSI).
- **rift's own Phase-43 core** ([archive/spec-host-telemetry.md](archive/spec-host-telemetry.md))
  — the daemon-global sampler, the push-only + welcome-replay `HostMetrics` message,
  and the status-line segment this phase recolours; the app-local
  `status_bar::HostMetrics` was deliberately shaped to be widened here.
- **rift's own status-line semantic colours** (`crates/app/src/status_bar.rs`) —
  `theme.warning` / `theme.danger` / `theme.success` already colour the diagnostic
  counts, pane-activity dots, and LSP health dot; the recolour reuses them (no new
  colour system, no hardcoded hex).
- **rift's own `gpui-component` notification surface** (`window.push_notification`,
  exercised in `crates/app/src/gallery/demos.rs`) — the vendored toast used for the
  pressure warning; `NotificationType::{Warning, Error}`.

## Human prerequisites

- **none.** No new dependency (the PSI read is `std::fs`; `sysinfo` already provides
  the portable fields), no secret, no external provisioning. PSI is read only where
  the kernel already exposes `/proc/pressure/memory`; its absence needs no host setup
  and is the expected case on the dogfooding WSL2 host.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The pressure **level is computed client-side**, not pushed by the daemon | The portable baseline reads only fields already on the wire since Phase 43 (`mem_available`, `swap_*`); thresholds + hysteresis are stateful UX policy that belongs beside the render (like the diagnostic-count and LSP-dot colouring), and the daemon stays the raw agent-agnostic signal source (`docs/constitution.md` reactive model). The daemon pushes only raw PSI averages, which the client can't read remotely | 2026-07-11 |
| The **only protocol change is the optional PSI payload** (`psi: Option<MemoryPressure>`); the portable baseline reuses existing fields | Phase 43 deliberately shipped the full host-aggregate sample (mem/swap/load) so sibling phases consume existing fields without a bump per phase (Phase-43 decision log). PSI is the one genuinely-new datum here — it must be read daemon-side from `/proc/pressure/memory`, which the client cannot reach | 2026-07-11 |
| The warning triggers on **memory signals only** — `MemAvailable` ratio + swap-in-use (+ PSI escalation) — **not** load average | Phase 44 is memory-pressure specific (the dogfooding motivator is the RAM limit). Load average measures CPU/scheduler contention, a different axis; recolouring the MEM indicator on high load would mislead when the host is CPU-bound with ample RAM. Load surfaces in Phase 46's detail view. The roadmap's "load trend" seed phrasing is refined here (roadmap notes decisions are the plan's to make) | 2026-07-11 |
| PSI, where present, **escalates** the level; it never replaces the portable baseline | A real memory stall (PSI `some`/`full` above 0) is stronger evidence of pressure than a static available-ratio, so it raises the level; but PSI is absent on WSL2, so the portable baseline must stand alone everywhere. PSI-present = "baseline OR PSI, whichever is worse" | 2026-07-11 |
| **Hysteresis** via separate enter/exit thresholds; the toast fires only on a **rising** level edge and re-arms after a return to `Normal` | An ambient indicator that flaps colour or spams toasts around a boundary is worse than none; separate enter/exit bands and edge-triggered, re-arming toasts are the standard notification-fatigue guard (systemd-oomd / monitoring precedent) | 2026-07-11 |
| Recolour reuses `theme.warning` / `theme.danger`; `Normal` stays `muted_foreground` | The semantic tokens the diagnostic-count and LSP-dot segments already use — no new colour system, no hardcoded hex (aligns with the Phase-26 hardcoded-palette cleanup direction) | 2026-07-11 |
| The PSI file-existence check is resolved **once** at sampler start and cached | PSI availability is a boot-time kernel-config property (`CONFIG_PSI`), not a runtime-varying condition; checking every tick is wasteful | 2026-07-11 |
| The first / welcome-replayed sample **seeds** the tracked level without firing a toast | `HostMetrics` is welcome-replayed, so a client (re)connecting under sustained pressure receives a Warning/Critical sample first; seeding the previous level from it (silent) means a reconnect or new window shows the right colour at once but does not re-toast — only a genuine *rise* during the session fires | 2026-07-11 |
| PSI escalation **shape** is fixed (only the numeric cutoffs are gate-open): a nonzero `some_avg10` stall raises one band; any `full_avg10 > 0` → `Critical`; PSI never lowers the baseline | Keeps the daemon/client contract unambiguous for the implementer while leaving the taste-level numbers to the gate; a full memory stall is unambiguous evidence of critical pressure | 2026-07-11 |
| OPEN — the concrete **warning / critical thresholds and hysteresis bands** (available-memory % enter/exit, swap-used % enter/exit, and the PSI `some_avg10` stall cutoff that escalates to Warning) | resolved at the spec-acceptance gate — a UX taste call on the developer's own dogfooding host; recommended defaults presented there. The escalation *shape* (PSI raises by one band, `full_avg10 > 0` → Critical) is fixed in Prior decisions; only the numeric cutoffs are open | — |

## Tracking

The decomposition into steps lives as GitHub issues, one per implementable step,
under the milestone. This spec owns the design; the issues own progress.

- Milestone: [Phase 440 — Memory-pressure warning](#) (created at the acceptance gate)
- Issues: created from this spec once merged — `protocol` (PSI payload + version
  bump), `daemon` (PSI read + parse + wire into the sampler), `app` (pressure model +
  segment recolour), `app` (pressure toast). Dependency edges in the issue bodies.

## Verification

- [ ] `just ci` passes (fmt-check + clippy `-D warnings` + tests, workspace excluding
      `rift-app`); `app-check` compiles `rift-app`; the `daemon-musl` job builds the
      daemon for `x86_64-unknown-linux-musl` with no C cross-compiler (the PSI read
      adds no dependency).
- [ ] `protocol`: `MemoryPressure` + `psi: Option<MemoryPressure>` land;
      `PROTOCOL_VERSION` bumps to 14 (next free at merge); the fingerprint test passes
      re-pinned; `HostMetrics` round-trips serde **with and without** `psi` and the
      wire tag stays `host_metrics`; a malformed PSI payload is rejected.
- [ ] Daemon unit test: the PSI parser reads a `/proc/pressure/memory`-shaped fixture
      into the six averages and returns `None` on a malformed / empty string; the
      sampler yields `psi: None` where the file is absent (the WSL2 dev host) and a
      populated `psi` where a fixture path exists, without a new dependency.
- [ ] App unit tests: `pressure_level` returns `Normal` / `Warning` / `Critical`
      across the threshold bands; hysteresis holds a level inside the enter/exit gap;
      a present PSI stall escalates a baseline `Normal` upward; absent PSI leaves the
      baseline verdict intact.
- [ ] Behavioural (dev-channel QA): under real memory load the segment recolours
      neutral → warning → critical and a single toast fires on each upward step (not
      repeatedly); relieving the load returns it to neutral and re-arms the toast. On
      the WSL2 host (no PSI) the portable baseline alone drives this; cross-check the
      trigger point against `free -m`. Confirm no flapping when memory hovers at a
      boundary.
- [ ] Behavioural (dev-channel QA): (re)connecting a client — or opening a second
      channel / window — while the host is already under pressure shows the correct
      warning colour immediately but fires **no** toast on connect (the replayed
      sample seeds the level silently); only a subsequent rise toasts.
- [ ] The daemon binary embeds **no new dependency** for PSI (the read is `std::fs` +
      a local parser); `cargo deny check licenses` is unchanged.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Thresholds flap the colour / spam toasts when memory hovers at a boundary | Separate enter/exit hysteresis bands; the toast is edge-triggered on a rising level only and re-arms after `Normal`. A QA item explicitly checks boundary-hover stability. |
| PSI parsing is brittle (format drift, partial lines) | The parser is defensive — any parse failure yields `None`, falling back to the portable baseline (never an error, never a panic); unit-tested with malformed input. PSI only ever escalates, so a parse miss degrades gracefully to baseline-only. |
| The toast needs `Window`, but the metrics fold loop runs in a plain `cx.spawn` | Convert that one loop to `cx.spawn_in(window, ...)` + `update_in` — the identical pattern the diff / nav / terminal loops directly below it already use in `workspace.rs`; no new infrastructure. |
| Recolouring on load (a non-memory axis) would mislead | Decided out of scope — the trigger is memory-only (mem-available + swap + PSI); load is Phase-46 detail. Recorded in Prior decisions. |
| A v13 client receiving a v14 sample | The Phase-20 strict-equality negotiation redeploys / reconnects on version skew before any v14 sample is parsed; `psi` is additionally `#[serde(default)]` so a missing field is tolerated regardless. |

## Decision log

- 2026-07-11: Spec drafted from the Phase-44 roadmap seed. Codebase mapped against
  the shipped Phase-43 core: the protocol `HostMetrics` variant already carries
  `mem_available` / `swap_*` (the portable baseline needs no wire change — only the
  optional PSI payload does); the daemon sampler (`build_host_metrics_message` /
  `host_metrics_sampler`) is the PSI read site; the app fold loop
  (`workspace.rs:1020`) and the `status_bar.rs` segment are the recolour + toast
  sites, with `theme.warning` / `theme.danger` and `window.push_notification` already
  present in-tree, and `cx.spawn_in` the established window-aware loop pattern. Central
  decisions: client-side level computation (daemon pushes raw signals only), PSI as an
  optional file-gated escalation over the portable baseline, memory-only trigger.
  One open item — the concrete threshold / hysteresis numbers — carried to the
  acceptance gate as a UX taste call on the dogfooding host.
