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
- [ ] A bound command that would enter a **pane mode** control clients cannot render (`copy-mode`, `choose-*`, `clock-mode`, `customize-mode`) is **intercepted, not dispatched**: rift surfaces a visible hint naming the GUI affordance that replaces it (mouse-wheel scrollback / the GUI pickers); the shared pane never enters the mode, so co-attached native clients are unaffected.
- [ ] A bound command that **renders on the issuing client** (`confirm-before`, `command-prompt`, `display-menu`, `display-panes`, `display-popup`) — which a control client cannot do — behaves per the gate decision (see Prior decisions): it never silently no-ops; stock defaults like `prefix x` (kill-pane confirm) have a defined, visible behavior.
- [ ] **Pending-prefix state is visible** (statusbar indicator) and cancelable (Escape); like native tmux at the 3.4 floor there is **no pending-prefix timeout** (tmux's `prefix-timeout` arrives in 3.5, default off — adopt later if wanted); a state-machine error can never swallow typing — passthrough always recovers.
- [ ] **Config changes are picked up without restarting rift**: prefix/option and binding state are re-queried on attach/reconnect and on the refresh triggers pinned in the issue (at minimum: an explicit refresh, dispatch of a binding-mutating command — `bind-key`/`unbind-key`/`source-file` — and dispatch of `set-option` touching `prefix`/`prefix2`/`repeat-time`).
- [ ] Agent-agnostic: nothing parses pane content; the only parsed text is the output of rift's own tmux queries (`list-keys`, `show-options`) over the framed command channel.

## Scope

### In scope

- **`list-keys` lookup**: query over the single command seam, parse into a `(table, key) → command` lookup. The parser handles tmux's quoting/escaping, mouse-binding entries (consciously skipped for keyboard lookup), and unknown lines (skip + log, never fail the table); tested with real-tmux fixtures, valid and malformed (constitution parser rule).
- **Option discovery via `show-options`**: the prefix is a **session option** (`prefix`, plus `prefix2`), not a binding — `list-keys` alone cannot provide the trigger key; `repeat-time` is likewise an option. Both are queried session-resolved over the same seam and refreshed with the table.
- **Keystroke → tmux key-name mapping**: GPUI keystrokes mapped to tmux key syntax (`C-b`, `M-Left`, `S-F5`, …) so lookups match `list-keys` entries — the reverse direction of today's `encode_keystroke`. Unmappable keys fall through to typing, never block.
- **Prefix state machine** at the established interception point (`on_key_down` before `encode_keystroke`, alongside the existing rift-native early returns): capture the chord after the prefix, resolve against the lookup, dispatch or fall through; `repeat-time` support; Escape/timeout cancel; pending-prefix indicator.
- **Dispatch** of resolved bound commands through the single command seam (today `TmuxClient::send_command`; after Phase 6, the daemon tmux-command path — the mirroring logic is seam-agnostic by constraint).
- **Pane-mode command interception** (`copy-mode`, `choose-*`, `clock-mode`, `customize-mode`): no dispatch; visible GUI-affordance hint.
- **Client-interaction command handling** (`confirm-before`, `command-prompt`, `display-menu`, `display-panes`, `display-popup`) per the gate decision.
- **Table-switching bindings** (`switch-client -T <table>` to an unmirrored table): intercepted with a hint, not dispatched — dispatching would desync the server-side client table from rift's mirror.
- **Table refresh/invalidation** on attach/reconnect plus the pinned triggers (binding mutations and `set-option` on `prefix`/`prefix2`/`repeat-time`).
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

- **Single seam, request/response form required**: `list-keys`/`show-options` queries and bound-command dispatch go through the one narrow command interface — today termy's `send_command`, after the Phase 6 transport swap the daemon tmux-command path. The query path needs the seam's **command-response** form (the `%begin`/`%end`-framed reply), which the Phase 6 protocol carries anyway (`capture-pane` scrollback and snapshots already require it) — this cross-phase requirement is explicit, not assumed. The mirroring logic consults an in-memory lookup and emits through the seam; it must not care which transport is behind it.
- **Sequencing**: the roadmap queue places Phase 7 behind Phase 6 (cross-milestone edge on the prior phase's last issue, #206). The design is seam-agnostic, so the swap landing first means this spec's dispatch path is the daemon one from day one.
- **Precedence layering is pinned**: rift-native early returns first (existing `Ctrl+Shift+C/V`, font zoom — the GUI affordances), then the tmux table lookup, then `encode_keystroke` fallthrough. A tmux binding shadowed by a rift-native shortcut stays shadowed in v1 (detection UI deferred).
- **tmux 3.4+** (hard requirement since Phase 2a); `list-keys` output fixtures are captured from real tmux, and unknown/unparseable lines degrade to skip+log, never to a failed table.
- **Unbound-after-prefix semantics follow native tmux**: a prefix-table miss is retried in the root table before being discarded (never forwarded as typing). Under a prefix-only v1 cut (gate option (a)), the root retry cannot run — a root-bound key after the prefix is then discarded where native tmux would run it; this deviation is accepted and named in the gate row.
- **Dispatch targeting relies on rift's focus sync**: bound commands without `-t` resolve against the session's current window/pane — correct only because rift already mirrors focus via `select-pane` on pane focus. That dependency is load-bearing; focus handling must not be changed out from under dispatch.
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
| **Pane-mode bound commands** (`copy-mode`, `choose-*`, `clock-mode`, `customize-mode`) **are intercepted, never dispatched** | Constraint-determined: control clients are not rendered pane modes (tmux contract), and dispatching would shove the *shared pane* into a mode that breaks co-attached native clients — the exact failure the interaction-fixes spec recorded when rejecting copy-mode forwarding. The GUI affordance (capture-pane scrollback / GUI pickers) already replaces the feature; rift surfaces a hint instead. | 2026-06-12 |
| **The prefix and `repeat-time` come from `show-options`** (session-resolved; `prefix2` included), refreshed together with the table | Constraint-determined tmux fact: the prefix is a session option checked server-side, not a `list-keys` entry — `bind C-b send-prefix` is convention, not contract; a mirror keyed off `list-keys` alone misses `set -g prefix C-a`. | 2026-06-12 |
| **Precedence: rift-native shortcuts → tmux tables → PTY fallthrough** | Constraint-determined: the existing early-return structure already runs rift-native handlers first, and rift's GUI affordances are deliberate replacements (interaction-fixes design framing) — a tmux binding must not preempt them. Conflict *surfacing* is deferred, the ordering is not. | 2026-06-12 |
| **Seam-agnostic dispatch; Phase 7 queues behind Phase 6** | Constraint-determined: the single-seam contract (`architecture.md`) and the Phase 6 transport swap; building against the seam keeps this spec indifferent to which transport is live. | 2026-06-12 |
| **OPEN — resolved at the spec-acceptance gate**: v1 table cut — (a) `prefix` table only (root bindings keep falling through as typing; root mirroring deferred; the native unbound-after-prefix root retry cannot run, so a root-bound key after the prefix is discarded where native tmux would run it), or (b) `prefix` + `root` tables (native parity for `bind -n` bindings including the post-prefix root retry, with rift-native shortcuts keeping precedence) | Genuinely open: neither precedent nor constraint settles where v1 cuts. (a) is the smallest fix for the actual pain (the prefix chord is what is inert today), with the named retry deviation; (b) is full native parity — `bind -n` power-user bindings (e.g. Alt-arrows pane nav) work — at the cost of consulting the table on every keypress and living with shadowing until conflict surfacing exists. The original stub's outcome leaned (b) ("at least `prefix` and `root`"). | — |
| **OPEN — resolved at the spec-acceptance gate**: v1 handling of client-interaction bound commands (`confirm-before`, `command-prompt`, `display-menu`, `display-panes`, `display-popup` — they render on the issuing client, which a control client cannot) — (a) intercept all with a visible hint naming the GUI affordance, or (b) native confirm dialog for `confirm-before` (render the prompt, dispatch the wrapped command on confirm — `prefix x`/`prefix &` then really work) plus hint for the rest | Genuinely open product cut: dispatching any of them silently no-ops on a control client — `prefix x` doing nothing would be a guaranteed dogfooding defect. (a) is minimal and honest; (b) additionally makes the kill confirmations actually function (bounded: parse `confirm-before`'s own `-p prompt` + wrapped command — tmux command syntax, not pane content) at the cost of one dialog surface. | — |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 7 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 7 — tmux key-table mirroring)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `list-keys` parser fixtures: real-tmux output including quoted/escaped commands, option-carrying binds (`-r`, `-n`), and mouse bindings (consciously skipped); malformed/unknown lines skip+log without failing the table
- [ ] `show-options` discovery: a config with `set -g prefix C-a` (and a `prefix2`) is mirrored correctly — the chord triggers on the configured key, not on `C-b`; changing `repeat-time` changes the repeat window after refresh
- [ ] Keystroke-mapping fixtures: modifier combinations and special keys map to tmux key syntax; unmappable keys fall through to typing
- [ ] `prefix c` opens a new tmux window; a custom binding from `.tmux.conf` runs its command; both visible to a co-attached native client
- [ ] A `bind -r` resize repeats within `repeat-time` without re-pressing the prefix; stops after the window expires
- [ ] An unbound key after the prefix is discarded (native semantics); an unbound key without prefix types normally; measured typing path unchanged
- [ ] A bound `copy-mode` chord shows the GUI-affordance hint and dispatches nothing; a co-attached native client's pane state is untouched
- [ ] Client-interaction chords behave per the gate decision; `prefix x` has a visible, defined behavior (hint or working confirm) — never a silent no-op
- [ ] `prefix prefix` (`send-prefix`) delivers a literal prefix byte to the pane via generic dispatch — not intercepted
- [ ] Pending-prefix indicator appears on prefix, clears on dispatch/cancel; Escape cancels capture; no timeout (3.4-native parity)
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
- 2026-06-12: Refreshed by `/loopkit:plan` (loop mode — roadmap Phase 7). Both stub preconditions are met: the interaction fixes shipped and archived (GUI scrollback via `capture-pane` exists), and the pane-mode interplay is settled by constraint — pane-mode bound commands are intercepted with a GUI-affordance hint, never dispatched (dispatching would break co-attached native clients). New since the stub: the Phase 6 transport swap is planned, so dispatch is recorded seam-agnostic (request/response form of the seam named explicitly) and the phase queues behind #206; precedence layering (rift-native → tmux tables → fallthrough) is pinned; repeat bindings, pending-prefix indicator, and refresh triggers are scoped. Two genuinely-open decisions — the v1 table cut (`prefix`-only vs. `prefix`+`root`) and the v1 handling of client-interaction bound commands — are flagged for the spec-acceptance gate.
- 2026-06-12: Review gate (fresh-context Agent review, `NEEDS CHANGES` → addressed). Blocking findings folded in: the prefix (and `prefix2`, `repeat-time`) is a session **option** discovered via `show-options`, not a `list-keys` entry — scope, outcomes, and refresh triggers corrected; the interception taxonomy was completed (pane modes gain `clock-mode`/`customize-mode`; the client-interaction class — `confirm-before`, `command-prompt`, `display-menu`, `display-panes`, `display-popup` — surfaced as a second gate decision so stock chords like `prefix x` never silently no-op). Non-blocking findings folded in: no pending-prefix timeout at the 3.4 floor (native parity; `prefix-timeout` is 3.5+), the native unbound-after-prefix **root retry** is recorded with the gate-option-(a) deviation named, `switch-client -T` to an unmirrored table is intercept-with-hint (terminology fixed), `send-prefix` verified as generic dispatch, the seam's request/response requirement made explicit, the dispatch-targeting dependency on rift's `select-pane` focus sync recorded, and mouse-binding fixtures added.
