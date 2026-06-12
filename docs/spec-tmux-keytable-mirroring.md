# Spec: Phase 7 — tmux key-table mirroring

> Status: DRAFT
> Created: 2026-06-04 (refreshed 2026-06-12 by /loopkit:plan — Phase 7 planning cycle)
> Completed: —

Make configured tmux keybindings work while focus is in a rift pane. Today input is sent as raw bytes into the pane PTY (`send-keys -H`), which *bypasses tmux's key tables entirely* (`architecture.md`, "tmux control-mode interaction model" — a recorded consequence of the `-CC` contract). As a result the prefix chord and every `bind-key` (window/pane management, custom bindings) are inert in rift. This spec restores them by mirroring tmux's key tables client-side: a `list-keys`-built lookup, a prefix state machine at the established interception point, and dispatch of bound commands through the single command seam.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] Pressing the configured tmux prefix followed by a bound key runs the bound tmux command exactly as in a native client (e.g. `prefix c` creates a window, `prefix %` splits) — for the key tables in the v1 cut (see Prior decisions).
- [ ] Bindings come from the **live tmux config** via `list-keys` — a user's custom bindings work with zero rift changes; there is no hardcoded table.
- [ ] **Repeat bindings** (`bind -r`, e.g. `prefix Left Left Left` to resize) honor tmux's `repeat-time` semantics.
- [ ] Keys with **no binding fall through unchanged** to the existing `encode_keystroke` → input path; plain typing latency is unaffected (the lookup is in-memory; no per-keypress round-trips).
- [ ] A bound command that would enter a mode control clients cannot render (`copy-mode`, `choose-*`) is **intercepted, not dispatched**: rift surfaces a visible hint naming the GUI affordance that replaces it (mouse-wheel scrollback / the GUI pickers); the shared pane never enters the mode, so co-attached native clients are unaffected.
- [ ] **Pending-prefix state is visible** (statusbar indicator) and cancelable (Escape; timeout per tmux semantics); a state-machine error can never swallow typing — passthrough always recovers.
- [ ] **Config changes are picked up without restarting rift**: the table is re-queried on attach/reconnect and on the refresh triggers pinned in the issue (at minimum: an explicit refresh, and dispatch of a binding-mutating command such as `bind-key`/`unbind-key`/`source-file`).
- [ ] Agent-agnostic: nothing parses pane content; the only parsed text is `list-keys` output — tmux's own command response over the framed command channel.

## Scope

### In scope

- **`list-keys` lookup**: query over the single command seam, parse into a `(table, key) → command` lookup. The parser handles tmux's quoting/escaping and unknown lines (skip + log, never fail the table); tested with real-tmux fixtures, valid and malformed (constitution parser rule).
- **Keystroke → tmux key-name mapping**: GPUI keystrokes mapped to tmux key syntax (`C-b`, `M-Left`, `S-F5`, …) so lookups match `list-keys` entries — the reverse direction of today's `encode_keystroke`. Unmappable keys fall through to typing, never block.
- **Prefix state machine** at the established interception point (`on_key_down` before `encode_keystroke`, alongside the existing rift-native early returns): capture the chord after the prefix, resolve against the lookup, dispatch or fall through; `repeat-time` support; Escape/timeout cancel; pending-prefix indicator.
- **Dispatch** of resolved bound commands through the single command seam (today `TmuxClient::send_command`; after Phase 6, the daemon tmux-command path — the mirroring logic is seam-agnostic by constraint).
- **Mode-entering command interception** (`copy-mode`, `choose-*`): no dispatch; visible GUI-affordance hint.
- **Table refresh/invalidation** on attach/reconnect plus the pinned triggers.
- **Root table (`bind -n`) mirroring** per the gate decision (see Prior decisions).

### Out of scope

- **copy-mode / choose-mode rendering** — not delivered to control clients; permanently replaced by GUI affordances (`archive/spec-terminal-interaction-fixes.md` design framing; scrollback via `capture-pane` already shipped).
- **Rebinding or editing tmux config from rift.**
- **Conflict detection/reporting UI** between tmux bindings and rift-native shortcuts — precedence is pinned (rift-native first, see Constraints); surfacing shadowed bindings is a later refinement.
- **User-defined key tables beyond the v1 cut** (`bind -T mytable`, `switch-client -T`) — a binding that switches to an unmirrored table is treated as unbound (falls through); revisit on demand.
- **tmux status-line mirroring** — Phase 8, own spec.

## Human prerequisites

None. Everything runs against the user's existing tmux config; no secrets, accounts, or provisioning.

## Constraints

- **Single seam**: `list-keys` queries and bound-command dispatch go through the one narrow command interface — today termy's `send_command`, after the Phase 6 transport swap the daemon tmux-command path. The mirroring logic consults an in-memory lookup and emits through the seam; it must not care which transport is behind it.
- **Sequencing**: the roadmap queue places Phase 7 behind Phase 6 (cross-milestone edge on the prior phase's last issue, #206). The design is seam-agnostic, so the swap landing first means this spec's dispatch path is the daemon one from day one.
- **Precedence layering is pinned**: rift-native early returns first (existing `Ctrl+Shift+C/V`, font zoom — the GUI affordances), then the tmux table lookup, then `encode_keystroke` fallthrough. A tmux binding shadowed by a rift-native shortcut stays shadowed in v1 (detection UI deferred).
- **tmux 3.4+** (hard requirement since Phase 2a); `list-keys` output fixtures are captured from real tmux, and unknown/unparseable lines degrade to skip+log, never to a failed table.
- **An unbound key after the prefix is discarded**, matching native tmux semantics — not forwarded as typing.
- **No agent detection, no pane-content parsing** — the parsed input is tmux's own `list-keys` response on the framed command channel.
- No termy changes anticipated; if one becomes necessary, contribute upstream / extend the pinned fork as the interaction-fixes capture change did — never a parallel mechanism.
- `thiserror` in library code; no `.unwrap()`; no emojis in UI (the prefix indicator is text/iconography per existing statusbar conventions).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Mirror via `list-keys`, not a hardcoded table** | A user's custom bindings must work with zero rift changes; reading the live config is the only config-agnostic approach. | 2026-06-04 |
| **Split out of the interaction-fixes spec** | Order-of-magnitude larger (prefix state machine, per-mode tables); would have sunk the small-fix batch — which has since shipped and archived. | 2026-06-04 |
| **Client-side interception before `encode_keystroke`** at the `on_key_down` seam | The interception point is established practice (font zoom, clipboard early returns landed there in the interaction fixes); prefix capture must happen before bytes are encoded for the PTY. | 2026-06-04 |
| **Mode-entering bound commands are intercepted, never dispatched** | Constraint-determined: control clients are not rendered copy-mode/choose-mode (tmux contract), and dispatching would shove the *shared pane* into a mode that breaks co-attached native clients — the exact failure the interaction-fixes spec recorded when rejecting copy-mode forwarding. The GUI affordance (capture-pane scrollback) already replaces the feature; rift surfaces a hint instead. | 2026-06-12 |
| **Precedence: rift-native shortcuts → tmux tables → PTY fallthrough** | Constraint-determined: the existing early-return structure already runs rift-native handlers first, and rift's GUI affordances are deliberate replacements (interaction-fixes design framing) — a tmux binding must not preempt them. Conflict *surfacing* is deferred, the ordering is not. | 2026-06-12 |
| **Seam-agnostic dispatch; Phase 7 queues behind Phase 6** | Constraint-determined: the single-seam contract (`architecture.md`) and the Phase 6 transport swap; building against the seam keeps this spec indifferent to which transport is live. | 2026-06-12 |
| **OPEN — resolved at the spec-acceptance gate**: v1 table cut — (a) `prefix` table only (root bindings keep falling through as typing; root mirroring deferred), or (b) `prefix` + `root` tables (native parity for `bind -n` bindings, with rift-native shortcuts keeping precedence) | Genuinely open: neither precedent nor constraint settles where v1 cuts. (a) is the smallest fix for the actual pain (the prefix chord is what is inert today); (b) is full native parity — `bind -n` power-user bindings (e.g. Alt-arrows pane nav) work — at the cost of consulting the table on every keypress and living with shadowing until conflict surfacing exists. The original stub's outcome leaned (b) ("at least `prefix` and `root`"). | — |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 7 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 7 — tmux key-table mirroring)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `list-keys` parser fixtures: real-tmux output including quoted/escaped commands and option-carrying binds (`-r`, `-n`); malformed/unknown lines skip+log without failing the table
- [ ] Keystroke-mapping fixtures: modifier combinations and special keys map to tmux key syntax; unmappable keys fall through to typing
- [ ] `prefix c` opens a new tmux window; a custom binding from `.tmux.conf` runs its command; both visible to a co-attached native client
- [ ] A `bind -r` resize repeats within `repeat-time` without re-pressing the prefix; stops after the window expires
- [ ] An unbound key after the prefix is discarded (native semantics); an unbound key without prefix types normally; measured typing path unchanged
- [ ] A bound `copy-mode` chord shows the GUI-affordance hint and dispatches nothing; a co-attached native client's pane state is untouched
- [ ] Pending-prefix indicator appears on prefix, clears on dispatch/cancel/timeout; Escape cancels capture
- [ ] Editing `.tmux.conf` (`bind-key` + `source-file`) and triggering refresh makes the new binding work without restarting rift
- [ ] Table-cut behavior matches the gate decision (root bindings dispatch under (b) / fall through under (a))
- [ ] A `grep` confirms no pane-content parsing and no agent detection in the key path
- [ ] Milestone QA (dev channel): the user's real tmux config drives windows/panes by keyboard as in a native client

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `list-keys` output format varies across tmux versions or contains exotic quoting | tmux 3.4+ hard floor; fixtures from real tmux; skip+log unknown lines — a partial table beats a failed one. |
| Keystroke→tmux-key mapping gaps (layouts, special keys) | Unmapped keys always fall through to typing; the mapping grows by fixture; never block the input path. |
| A state-machine bug swallows typing | Escape and timeout always restore passthrough; the capture state is a single enum at one seam; regression tests on the fallthrough path. |
| Root-table mirroring (if gated in) shadows or surprises (`bind -n` vs rift shortcuts) | Precedence pinned (rift-native first); shadowing accepted in v1 with conflict surfacing deferred; gate decision controls whether root is in at all. |
| Dispatched bound commands change bindings themselves (`bind-key`, `source-file`) leaving the lookup stale | Binding-mutating commands are a pinned refresh trigger; attach/reconnect refresh is the backstop. |
| Phase 6 swaps the transport underneath this work | Seam-agnostic constraint + queue edge: Phase 7 starts after the swap, dispatching via the daemon path from day one. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-04: Stub created when splitting key-table mirroring out of `spec-terminal-interaction-fixes.md`. Remains DRAFT until the interaction fixes land and the copy-mode/mode-table interplay is settled.
- 2026-06-12: Refreshed by `/loopkit:plan` (loop mode — roadmap Phase 7). Both stub preconditions are met: the interaction fixes shipped and archived (GUI scrollback via `capture-pane` exists), and the mode-table interplay is settled by constraint — mode-entering bound commands are intercepted with a GUI-affordance hint, never dispatched (dispatching would break co-attached native clients). New since the stub: the Phase 6 transport swap is planned, so dispatch is recorded seam-agnostic and the phase queues behind #206; precedence layering (rift-native → tmux tables → fallthrough) is pinned; repeat bindings, pending-prefix indicator, and refresh triggers are scoped. The one genuinely-open decision — v1 table cut, `prefix`-only vs. `prefix`+`root` — is flagged for the spec-acceptance gate.
