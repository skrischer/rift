# Spec: Phase 18 — Window-tab pane-activity indicators

> Created: 2026-07-02

Each tmux-window tab shows how many of its panes are running a foreground process
and their aggregated activity state (free / busy / attention), so a glance at the
tab bar tells you which windows have an agent working and which want your input —
derived agent-agnostically from signals rift already parses per pane (OSC-133 shell
integration + the terminal bell), never from any agent's output.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] Each window tab in the terminal tab bar carries a **state indicator**: a colored dot for the window's **dominant** pane state and, when > 0, a **count of active panes** (panes that are busy or waiting) in that window.
- [ ] **Three states**, precedence **attention > busy > free**:
  - **busy** — a foreground command is running in the pane (OSC-133 `CommandPhase::Executing`); stays busy while the process is silent (an agent thinking mid-turn is still busy, not free).
  - **free** — any non-`Executing` OSC-133 phase (`PromptShown` / `CommandInput` / `Idle`): the pane is at a shell prompt, i.e. the whole state map is `match phase { Executing => Busy, _ => Free }`.
  - **attention (waiting)** — the pane rang the terminal bell (BEL) while its window was **not** the active window, and the user has not yet acknowledged it; overlays busy/free and dominates the aggregate.
- [ ] The indicators are **live**: they update as panes start/finish commands and ring the bell, with no manual refresh — event-driven off the byte stream each `PaneView` already consumes, plus one lightweight idle tick for the recency fallback.
- [ ] **Attention clears on acknowledgement** — selecting the pane's window (viewing it) drops that window's attention state back to its underlying busy/free.
- [ ] Works on **both terminal paths** (daemon-default and `RIFT_TERMINAL_LEGACY`) because the derivation reads each pane's client-side `alacritty_terminal::Term` / OSC state, not any path-specific plumbing.
- [ ] Degrades cleanly: a window with no active panes shows no dot (or a muted one) and no count; a pane without shell integration still registers as busy via the output-recency fallback; never a panic, never a stuck indicator.
- [ ] **Agent-agnostic**: the count and states derive only from per-pane byte-stream signals (OSC-133 command lifecycle, terminal bell, output recency). A `grep` of the activity path finds no agent name and no pane-content parsing.

## Scope

### In scope

- **Per-pane activity state** (`crates/terminal`, `PaneView`): a `PaneActivity` value (`Free` / `Busy` / `Attention`) derived from the pane's existing `command_lifecycle` (OSC-133 `CommandPhase`, `pane_view.rs:120/448/514-535`) for busy/free, the pane's terminal **bell** for attention, and an **output-recency fallback** (busy if the pane emitted output within the last idle window) for panes that have never produced an OSC-133 marker. Exposed via an accessor on `PaneView`.
- **Terminal-bell capture** (`PaneView`): observe `alacritty_terminal`'s bell (the `Bell` event on its listener) so a BEL in the pane byte stream sets an unacknowledged-attention flag. This is the one new per-pane signal; OSC-133 lifecycle is already computed.
- **Per-window aggregate** (`crates/terminal`, `SessionView`): fold the panes in each `WindowState` (`session_view.rs:43-49`, `pane_ids` + `panes` map) into an `active_count` (busy-or-attention panes) and a `dominant` state (attention > busy > free), recomputed on pane state change and on the idle tick. Attention acknowledgement is tied to the active-window transition.
- **Child→parent live wiring** (`SessionView`): a `PaneView`'s own `cx.notify()` re-renders only its own subtree, not the parent tab loop — so `SessionView` must observe its child pane entities for the tab dots to update live off a **background** window's OSC-133/bell transition. Wire it as `cx.observe(&pane_entity, …)` when panes are (re)built in `apply_snapshot`, or a small activity channel mirroring the existing `font_zoom_tx` pattern. Without this, background dots would refresh only on the next incidental `SessionView` render, contradicting the "event-driven, idle-tick-only-for-recency" scoping.
- **Tab rendering** (`SessionView`, the tab loop at `session_view.rs:904-974`): add a colored state dot + active-pane count onto each `gpui_component::tab::Tab`, reusing the existing status-dot idiom (`div().size(px(8.0)).rounded_full().bg(color)`, `:1025`) and the existing state→color palette (`:883-888` — hardcoded Catppuccin `rgb(...)` literals, as the connection dot uses, not gpui-component theme tokens). Compact — it must not crowd the tab label or the close/rename affordances.
- **Idle tick**: one small periodic recompute (≈1 s) to age the output-recency fallback from busy back to free after the idle window; OSC-133 and bell updates stay event-driven.
- **Tests**: pure-logic tests for the state machine (OSC-133 transitions → states; bell → attention and its clearing; recency fallback timing; aggregate precedence + count) with valid and edge fixtures (constitution's logic/parser test rule).

### Out of scope

- **tmux `monitor-activity` / `monitor-silence` / `monitor-bell` alerts** — the alternative mechanism (see Prior decisions). It is coarser than OSC-133 (activity ≠ "a command is running"), it mutates the user's **shared** tmux session via `set -g monitor-*` (a side effect on a vanilla environment), and it requires new `tmux-core` event variants + daemon routing + protocol fields for a signal rift can already derive client-side. Rejected.
- **A per-pane indicator inside the terminal grid / pane sidebar** — v1 is the window-tab aggregate only (the user's ask is "pro tmux-window Tab"). The pane sidebar (`render_pane_sidebar`, `:658-755`) is untouched.
- **Notifications, sounds, or OS badges** on state change (Superconductor-style) — later, if ever.
- **Any agent detection** — no matching `pane_current_command` against agent names, no parsing of agent output or spinner glyphs, no agent hook config (`@claude_state`). Forbidden by the constitution; explicitly out.
- **Daemon / protocol / `tmux-core` changes** — none needed; the feature is client-side in `crates/terminal`.
- **A configurable idle threshold / color settings UI** — a sensible constant for v1; theming follows the existing tokens.

## Human prerequisites

None. Pure client-side derivation and rendering from signals rift already parses; no new dependency, no protocol change, no secrets, no external provisioning.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-18 concern anchors this spec.

- [Pane activity state on window tabs (Phase 18)](prior-art.md#pane-activity-state-on-window-tabs-phase-18) — the backing concern. **Adopt** the UX (color-coded dot + per-window aggregate) from Arbor / Claude-Squad / zellij; **avoid** their detection mechanisms (Arbor's hardcoded agent detection, Claude-Squad's `capture-pane` content hashing, agent `@claude_state` hooks). The entry leaned on tmux `monitor-*` alerts; this spec **refines** that to OSC-133 lifecycle + bell after code review showed rift already computes the finer, client-side signal (see Decision log).
- Architecture pattern 9 in prior-art.md — *"Per-pane state machine with bell/activity awareness"* — is exactly this feature's shape (bell as an attention signal; per-pane state folded to an aggregate).
- rift-local grounding: `PaneView::command_lifecycle` (OSC-133 `CommandPhase` `Idle`/`PromptShown`/`Executing`, `crates/terminal/src/pane_view.rs`), `SessionView::WindowState` (`session_view.rs:43-49`), the tab loop + status-dot idiom (`session_view.rs:904-974`, `:1025`, `:883-888`), and `gpui_component::tab::Tab` (`.child()`/`.suffix()`).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Client-side only, in `crates/terminal`; no daemon / protocol / `tmux-core` / tmux-global-option change** | Constraint-determined: OSC-133 lifecycle and the bell are per-pane signals already present in the client `Term`, and **both** terminal paths feed `PaneView`, so a client-only derivation is path-agnostic and avoids mutating the user's shared tmux session. Minimal-solution. | 2026-07-02 |
| **Busy/free from OSC-133 `command_lifecycle`, not output activity** | Constraint-determined: `Executing` means a command is running even while it is silent — so an agent thinking mid-turn stays **busy**. Raw output-activity (tmux `monitor-activity`) would flip such a pane to idle, the exact wrong signal for "is the agent working". | 2026-07-02 |
| **Output-recency fallback for panes without OSC-133** | Robustness: a pane whose shell emits no OSC-133 markers would otherwise always read "free" while a process runs; a "busy if output within the idle window" fallback keeps the count meaningful. It is a **degraded baseline** — it cannot keep a *silent* running process busy (that reintroduces the `monitor-activity` weakness), so OSC-133 is the only signal that satisfies the "busy while thinking" outcome. Shell OSC-133 integration is therefore validated first (see Verification). | 2026-07-02 |
| **tmux `monitor-*` alerts rejected as the mechanism** | Coarser than OSC-133, mutates the shared tmux session (`set -g`), and needs cross-crate plumbing for a client-derivable signal. Recorded as the considered alternative (roadmap prior-art leaned on it). | 2026-07-02 |
| **Aggregate precedence attention > busy > free; count = busy-or-attention panes** | The tab must foreground the most action-worthy state; the count answers the user's literal "in how many panes a session runs". | 2026-07-02 |
| **Attention invariant: raised by a bell only when the pane's window is not active; cleared by the active-window transition in `apply_snapshot` (`is_active` flip)** | One rule handles both edges: a bell in the already-active window never raises attention (you are looking at it), and acknowledgement keys off the snapshot's `is_active` flip — so tab-click, Alt+1..9 (`session_view.rs:1052`), and `%output`-confirmed selects all clear it uniformly, not just the click handler. Simple, deterministic, no timer. | 2026-07-02 |
| **3-state model; the terminal bell is the "waiting"/attention signal (best-effort, agent-dependent)** | Resolved at the spec-acceptance gate: the user's request is explicitly free/busy/waiting, and the bell is the only agent-agnostic attention signal. Accepted as **best-effort** — a pane whose agent never rings the bell simply never shows waiting; if the milestone-QA gate finds the bell unreliable in practice, the documented fallback is to ship 2-state (free/busy) without re-planning. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 18 milestone. Created once this spec is merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created after merge (Phase 180 — Window-tab pane-activity indicators)
- Issues: created from this spec once merged (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace` passes; `app-check` compiles the app
- [ ] Pure-logic tests for the per-pane state machine: OSC-133 `PromptShown`/`Idle` → free, `Executing` → busy; a bell → attention; acknowledgement clears attention to the underlying state; the output-recency fallback flips busy→free after the idle window and busy on new output; valid and edge fixtures
- [ ] Pure-logic tests for the per-window aggregate: precedence attention > busy > free, `active_count` counts busy-or-attention panes, empty/single/mixed windows
- [ ] Running a command in a pane (in the terminal) turns its window's dot busy live; returning to the prompt turns it free; a second busy pane bumps the count
- [ ] A pane that rings the bell turns its window's dot to attention; selecting that window clears it
- [ ] A pane that is busy but silent (a long-running or thinking process) stays busy, does not flicker to free
- [ ] **First-issue validation**: OSC-133 command markers are confirmed present in a real Claude Code pane in the dogfooding shell; if absent, the output-recency fallback is exercised as the degraded baseline and the OSC-133 path is the precision enhancement
- [ ] A state change in a **background** (non-active) window updates its tab dot live — verifying `SessionView` observes its child panes, not merely a refresh on the idle tick
- [ ] Behaviour is identical on the daemon-default path and under `RIFT_TERMINAL_LEGACY`
- [ ] `grep` of the activity/aggregate/render path confirms no agent name, no `pane_current_command` name-matching, and no pane-content parsing
- [ ] Milestone QA (dev channel): with a real Claude Code pane, the window tab reads busy while it works, attention when it rings the bell for input, free at the prompt; a multi-pane window shows the correct active count

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| OSC-133 shell integration is absent in the dogfooding pane, so busy never lights | The first issue validates OSC-133 presence against a real Claude Code pane; if absent, the **output-recency fallback** (already in scope) carries busy/free and the OSC-133 path becomes the precision enhancement. |
| The terminal bell is unreliable for "waiting" — an agent may not ring it | Resolved to a best-effort 3-state at the acceptance gate; a pane that never rings the bell simply never shows waiting. Claude Code rings the bell on permission prompts and turn end, but agents are black boxes — so the milestone-QA fallback is to ship 2-state (free/busy) if the bell proves unreliable, without re-planning. |
| The dot/count crowds the tab label or the close/rename affordances | Compact fixed-size dot + a short count; reuse the existing 8px dot idiom; verified at the milestone QA gate. |
| The idle-tick recompute is a busy poll | One ≈1 s tick only for the recency fallback; OSC-133 and bell updates are event-driven. If the fallback is dropped (OSC-133 universal), the tick goes with it. |
| Attention state leaks across the pane lifecycle (a closed/renumbered pane keeps a stale flag) | State is keyed to the live pane entity in the `panes` map and recomputed from it on every `apply_snapshot`; a removed pane drops out of the aggregate. |
| Divergence from the prior-art entry's `monitor-*` mechanism | Recorded in Prior decisions + Decision log; the backing concern (Pattern 9: bell/activity state machine, and the Arbor/Claude-Squad/zellij UX) still holds — only the detection mechanism is refined to the finer client-side signal. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec-acceptance gate. Human prerequisites confirmed **none**. The one genuinely-open decision — the terminal bell as the "waiting" signal vs shipping 2-state — resolved to the **3-state bell-based** model (the user's explicit ask; the bell is the only agent-agnostic attention signal), accepted as best-effort with a documented milestone-QA fallback to 2-state if the bell proves unreliable. Spec accepted and merged.
- 2026-07-02: Review gate (fresh-context Agent review) — `APPROVE`, no blocking findings. Both load-bearing mechanism claims verified against pinned source (OSC-133 `command_lifecycle` already computed client-side and stays `Executing` through silence; `alacritty_terminal::Event::Bell` reachable via the pane `Term`, needing the `Listener` filter at `pane_view.rs:34-38` extended). Non-blocking findings folded in: (1) `SessionView` must observe its child `PaneView` entities (a `PaneView`'s own `cx.notify()` does not re-render the parent tab loop) so background-window transitions update live; (2) `CommandPhase` has four variants — the state map is `match { Executing => Busy, _ => Free }`, including `CommandInput`; (3) the recency fallback is a degraded baseline that cannot keep a *silent* process busy, so OSC-133 presence is a first-issue validation; (4) a single attention invariant (raised only when the window is not active, cleared on the `is_active` flip in `apply_snapshot`) handles the already-active-window and Alt+1..9 edge cases uniformly; (5) the dot colors are hardcoded Catppuccin `rgb(...)` literals like the connection dot, not gpui-component theme tokens. The stale roadmap Phase-18 row (still naming tmux `monitor-*` and "idle") is aligned in the Step-8 roadmap PR.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 18). A code investigation (integration-point map) found rift already computes an agent-agnostic per-pane busy/free signal — `PaneView::command_lifecycle` from OSC-133 shell integration — that is finer than tmux `monitor-activity` (it distinguishes "command running" from "at prompt" and does not flip to idle when a running process is silent) and lives entirely client-side, feeding both terminal paths via `PaneView`'s `Term`. The spec therefore **refines the roadmap prior-art's tmux-`monitor-*` mechanism to OSC-133 lifecycle + terminal bell**, keeping the whole feature in `crates/terminal` with no daemon/protocol/`tmux-core` change and no mutation of the user's shared tmux session. Busy/free, aggregate precedence, count semantics, and attention-clearing are constraint/precedent-determined; the one genuinely-open point is whether the terminal bell is the accepted (best-effort) "waiting" signal or v1 ships free/busy only — carried to the spec-acceptance gate.
