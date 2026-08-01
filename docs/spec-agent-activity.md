# Spec: Agent activity — working vs idle signal

> Created: 2026-08-01

Split today's single `Busy` pane state into working vs idle-awaiting-input for a
non-shell foreground process (an agent), which the `is_shell` foreground-command
signal cannot — an agent pane reads permanently `Busy`. Strictly agent-agnostic.
Roadmap Phase 53.

## Outcome

- [ ] A pane running a long-lived non-shell foreground process (an agent) is distinguished between **working** (actively computing/emitting) and **idle** (awaiting input) — today both read `Busy` unconditionally (`classify_busy`, `pane_view.rs:79`).
- [ ] The distinction derives only from agent-agnostic host signals (per-pane `/proc` CPU of the pane's process subtree, and/or PTY byte-arrival cadence) — never from agent detection or output parsing; the pane label stays `pane_current_command` / `pane_title`.
- [ ] The working/idle state surfaces in the existing window-tab activity indicator and status-line window chip, extending the `PaneActivity` model (`pane_view.rs:57`) without disturbing the free/attention states.
- [ ] The existing structural `Busy` (a command is running) remains the gate: working/idle is a refinement *within* Busy, so a plain shell pane is unaffected.

## Scope

### In scope

- Extend `PaneActivity` (`pane_view.rs:57`) to carry a working-vs-idle refinement of `Busy` (a new variant or a sub-flag), and thread it through the aggregate + render path: `activity_rank` / `aggregate_activity` (`session_view.rs:369-394`), `tab_state_slot` (`:410`), the tab render (`:2340`), and the status-line `window_chip` (`status_bar.rs:328`).
- The working/idle signal source is **per-pane `/proc` CPU roll-up** (gate-resolved, the Phase-45 method): working = the `pane_pid` process-subtree CPU is above a threshold, idle = near-zero. This reuses Phase 45's per-pane metric (milestone #66), which must be implemented first (it is spec-only today — no `#{pane_pid}` query, no per-process sampler, no `PaneMetrics` message exist). The output-idle proxy and the layered variant were declined at the gate.
- The CPU threshold and hysteresis, tuned to avoid flicker at the boundary.

### Out of scope

- Any agent detection or output parsing — forbidden (constitution `:69`). No capture-pane content hashing (AVOID Arbor's approach; see Prior art). The signal is CPU and/or byte-arrival only.
- Changing the free / attention states or the `is_shell`-derived `Busy` gate itself — working/idle is a refinement within `Busy`, not a rework of Phase-18-v2's structural model.
- Per-pane CPU *numbers* in this indicator — that is Phase 45's per-pane breakdown popover; Phase 53 consumes a working/idle *classification*, not a metric display.
- Implementing Phase 45 as part of this spec — if the gate elects a `/proc`-CPU path, Phase 45's milestone (#66) is a hard dependency delivered on its own spec, and Phase 53's issues take a `Depends on milestone: #66` edge.
- Host-global resource state — this is per-pane attribution, keyed on the pane's process (`pane_pid`), never on which agent runs there (constitution `:28-30`).

## Constraints

- **`is_shell` / OSC-133 cannot supply working/idle.** An agent is a single long-lived foreground command: tmux reports `is_shell = Some(false)` for its whole lifetime (`classify_busy` `pane_view.rs:83` → unconditional `Busy`), and OSC-133 emits one `133;C` at launch and `133;D` only at exit (no per-turn prompt cycle; `spec-pane-activity-v2.md:29-44`). Both stay "running" the entire session. A new orthogonal signal (CPU and/or output cadence) is required.
- **Phase 45 is spec-only.** No `#{pane_pid}` in `LAYOUT_QUERY` (`daemon/src/terminal.rs:65`), no per-process sampler (the only `/proc` sampler is host-global, `daemon/src/lib.rs:2004`), no `PaneMetrics` message. So path (a)/(c) is hard-blocked on delivering Phase 45 first; only path (b) is buildable today.
- **Path (b) partially reverses a deliberate Phase-18-v2 decision.** V2 explicitly *deleted* the byte-flow machinery (`ACTIVITY_IDLE_WINDOW`, `last_output`, `on_output`; `spec-pane-activity-v2.md:122-129`, `:234`) because output cadence answers "is the pane repainting," not "is it working" — user repaints (mouse reports, redraw bursts, resize reflows) read as output, and a silent-but-working agent (blocked on an API with a paused spinner) ages to idle. Re-admitting it is a conscious tradeoff the gate must own.
- **Path (a) fidelity + cadence.** `/proc` subtree CPU directly measures "computing" (agent thinking → high CPU; waiting on API/user → ~0%), but at the ~2 s sampler cadence with a two-sample settling, and with a defensible spin-vs-work ambiguity (a busy-wait spinner shows CPU) — all agent-agnostic.
- Agent-agnostic; the pane label stays `pane_current_command` / `pane_title` (roadmap `:536` guardrail). Foundation-doc impact: none (Phase 43 admitted `/proc`; Phase 45 supplies the `pane_pid` method).

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 53 row: the shipped `is_shell` signal cannot see an agent's working→idle edge (permanently Busy); add the Phase-45 `pane_pid`→/proc CPU roll-up and/or a PTY output-idle timer as the missing edge signal. **AVOID Arbor's agent detection + capture-pane content hashing** (constitution). References rift's own Phase-18 pane-activity index + pattern #9; `penso/arbor` working/waiting indicators; per-pane `/proc` via `pane_pid`.
- rift's own Phase-45 spec (`docs/spec-pane-attribution.md`) — the `pane_pid` → subtree CPU/RSS method a `/proc`-CPU path reuses.
- rift's own Phase-18-v2 spec (`docs/spec-pane-activity-v2.md`) — the structural `Busy` model this refines, and the byte-flow signal it deliberately removed (which path (b) re-admits).

## Human prerequisites

- none for the spec itself. If the gate elects a `/proc`-CPU path (a or c), Phase 45 (milestone #66) must be implemented before Phase 53's CPU-dependent issues can start — a milestone dependency, not a human secret.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Working/idle is a refinement *within* the existing structural `Busy`, not a rework of it | Phase-18-v2's `is_shell` gate correctly answers "a command is running"; this only splits the non-shell Busy case; a shell pane is unaffected | 2026-08-01 |
| Strictly `/proc` CPU and/or byte-arrival cadence; never capture-pane hashing or agent detection | Constitution `:69`; Arbor's content-hashing approach is explicitly rejected | 2026-08-01 |
| The indicator carries a working/idle *classification*, not per-pane CPU numbers | Per-pane metric display is Phase 45's popover; this is an at-a-glance state | 2026-08-01 |
| The working/idle signal source is **(a) per-pane `/proc` CPU** — working = the `pane_pid` process-subtree CPU is above a threshold, idle = near-zero — reusing the Phase-45 method. NOT the output-idle proxy (b) and NOT the layered (c) | Accepted at the gate. Highest fidelity (measures "computing" directly), constitution-cleanest, and avoids re-admitting the interaction-dependent byte-flow Phase-18-v2 deliberately removed. The accepted cost: Phase 53's CPU issues are blocked on delivering Phase 45 (milestone #66, spec-only today) and inherit the ~2 s cadence + spin-vs-work ambiguity | 2026-08-01 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged. Shape depends on the gate: path (a)/(c) → a `Depends on milestone: #66` edge and CPU-classification issues that consume Phase 45's per-pane metric; path (b) → a self-contained client output-idle-timer issue. Plus the shared `PaneActivity` model + render-thread issue either way.

## Verification

- [ ] `just ci` equivalent green for the touched crates (fmt + clippy `-D warnings` + tests); warm-target clean.
- [ ] Unit test: the working/idle classifier maps its input signal (CPU sample series and/or output-cadence timeline) to the expected state with hysteresis (no flicker at the threshold).
- [ ] Unit test: a plain shell pane and the free/attention states are unchanged; working/idle only refines the non-shell `Busy` case.
- [ ] QA: an agent visibly generating/streaming reads **working**; the same agent paused at its input prompt reads **idle** within the threshold window; a shell pane reads neither (free).
- [ ] QA (path a/c): waiting on an API/user with ~0% CPU reads idle; thinking/generating reads working. QA (path b): document the known false-positives (user repaints) and false-negatives (silent-but-working) accepted for the proxy.
- [ ] QA: no agent-specific behavior — swapping the agent (Claude Code ↔ Codex) changes nothing in the signal.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Path (a/c) is blocked on Phase 45, which is unimplemented | The gate owns this; if a/c is chosen, Phase 53's CPU issues carry `Depends on milestone: #66` and wait for Phase 45; the shared model/render issue can still proceed |
| Path (b) false-positives on user repaints / false-negatives on silent-but-working agents (the reason v2 removed it) | Documented and accepted at the gate for path (b); tune the threshold + hysteresis; the state is an at-a-glance hint, not authoritative |
| Flicker at the threshold | Hysteresis / a minimum dwell time on state transitions; unit-tested |
| Re-admitting byte-flow reintroduces the v2 complexity that was deleted | Scope it to the working/idle refinement only; do not reconnect it to the free/Busy structural gate (which stays `is_shell`-driven) |
| Spin-vs-work ambiguity in CPU (a busy-wait spinner shows CPU) | Agent-agnostic and defensible; documented; the classification reflects host truth (the process is burning CPU) |

## Decision log

- 2026-08-01: Scoped from `pane_view.rs` / `session_view.rs` / `daemon` + the Phase-45 and Phase-18-v2 specs (Explore agent). `is_shell` and OSC-133 both stay "running" for an agent's whole lifetime, so a new orthogonal signal is required. Phase 45 (the `/proc` `pane_pid` method) is 100% spec / 0% code, so a CPU path blocks on delivering it; the output-idle path is buildable now but re-admits the byte-flow signal Phase-18-v2 deliberately removed. The single open item — the signal source (a `/proc` CPU vs b output-idle vs c both), which also sets the `Depends on milestone: #66` edge — carried to the gate.
- 2026-08-01: Spec-acceptance gate (PR #942) — accepted. Review returned APPROVE (verified the agent-agnostic crux for all three options — no content inspection / agent-name match / output parsing in any; confirmed `is_shell` + OSC-133 both stay "running" for an agent's whole lifetime, Phase 45 is 100% spec / 0% code, and the byte-flow machinery was genuinely deleted by v2) with only informational nits. Gate decision: **(a) per-pane `/proc` CPU** — highest fidelity, avoids re-admitting the byte-flow v2 removed; the accepted cost is a hard dependency on Phase 45 (milestone #66), so the CPU-classification issues carry `Depends on milestone: #66` and wait until Phase 45 ships (the shared `PaneActivity` model + render-thread issue can proceed independently).
