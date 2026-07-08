# Spec: Pane-activity indicators v2 — structural busy/free derivation

> Status: READY
> Created: 2026-07-08
> Completed: —

Rederive each window tab's pane-activity indicator (free / busy / attention) from
the pane's **structural process state** — is the foreground process the shell, or a
running command — instead of output byte-flow recency, so the indicator is correct
for a full-screen TUI agent, independent of user interaction, and never lights a
pane that is idle at a prompt. Supersedes
[archive/spec-pane-activity-indicators.md](archive/spec-pane-activity-indicators.md)
(Phase 18).

## Why (the dogfooding bugs this fixes)

The shipped derivation (OSC-133 command lifecycle with an output-recency fallback)
misfires for the primary case — a full-screen coding agent (Claude Code) running in
a pane. Three regressions observed while dogfooding:

1. **A pane reads free while the agent is actively working** — it should be busy.
2. **The indicator flips to busy merely from window interaction** (hover / click /
   select) — it must be interaction-independent.
3. **The indicator lights on panes that never started a command** — an idle shell
   pane should read free.

### Diagnosis

- **OSC-133 is the wrong granularity for a TUI agent.** OSC-133 is *shell* prompt
  integration; its markers are emitted around the shell prompt cycle and, by design,
  do **not** fire on the alternate screen where a full-screen TUI lives (a
  documented OSC-133 limitation). A coding agent is a single long-lived foreground
  command: the shell emits one `133;C` (executing) when the agent launches and one
  `133;D` when it exits — there is no per-turn prompt cycle inside the TUI. So
  OSC-133 alone cannot represent the agent's think/idle cadence, and the single
  launch marker sits far in the past for the whole session.
- **The trust-window aging (#491) discards the one correct signal.** Because the
  agent emits output continuously (spinner, streaming) while the launch marker
  stays fixed, `osc133_trusted()` ages OSC-133 authority out after
  `OSC133_TRUST_WINDOW` (10 s of output past the marker) and hands busy/free to the
  output-recency fallback. Its premise — "output flowing 10 s past a marker means
  shell integration died" — is false for the common case: a legitimately
  long-running foreground command (agent, build, dev server, `tail -f`) produces
  exactly that pattern.
- **The output-recency fallback is byte-flow, so it is interaction-dependent and
  cannot represent "running but silent".** Once the derivation falls through to
  "busy if the pane emitted output within 1500 ms":
  - any repaint trips busy, including repaints the *user* caused — mouse reports
    forwarded to the PTY on hover, the redraw burst tmux sends on window-select,
    resize reflows (**bug 2**), and incidental output on an otherwise-idle pane
    (**bug 3**);
  - a genuinely-working-but-silent agent (waiting on an API call, spinner paused)
    ages to free within 1500 ms (**bug 1**).

Neither signal, as combined, answers the actual question — *is a command running in
this pane* — agent-agnostically and independent of what the user is doing.

### The corrected signal

rift already carries a **structural, interaction-independent** answer that the
shipped derivation predates: tmux's own
`#{==:#{pane_current_command},#{b:default-shell}}` comparison, evaluated server-side
by the daemon and plumbed per pane as `is_shell` (#510,
`rift_protocol` → `SessionSnapshot.pane_is_shell`). It is *the pane's foreground
process is / is not the login shell* — a tmux **format field**, which the
constitution explicitly permits as a signal source, and which compares against the
*default-shell*, never against any agent name. It does not change when the user
hovers, clicks, selects, or resizes; it does not depend on byte flow; and it holds
steady across an agent's silent think phases. v2 makes it the authoritative
busy/free signal, with a client-side structural fallback (alternate-screen mode +
OSC-133 `Executing`) only where `is_shell` is unavailable, and removes byte-flow
from the busy/free derivation entirely.

## Outcome

Observable, end-to-end criteria — not activities. A box is checked only when the
outcome holds end-to-end (typically at `COMPLETED`).

- [ ] **Busy while a command runs.** A pane whose foreground process is not the
  login shell reads **busy** and stays busy across the process's silent phases — a
  running coding agent reads busy the whole session, working *or* thinking, with no
  flicker to free.
- [ ] **Free at a prompt.** A pane sitting at its shell prompt reads **free** (no
  dot, no count), including a freshly split pane or one the user `exit`ed back to a
  prompt — no incidental output can light it.
- [ ] **Interaction-independent.** Hovering, focusing, clicking, selecting text in,
  scrolling, or switching to a pane's window never changes its busy/free state. The
  only interaction that changes any state is viewing a window, which *clears*
  attention (never sets busy).
- [ ] **Attention on the bell, unchanged in spirit.** A pane that rings the terminal
  bell while its window is not active reads **attention** (overlaying busy/free, and
  dominating the per-window aggregate) until the user views that window. Best-effort:
  a pane whose agent never rings the bell simply never shows attention.
- [ ] **Aggregate preserved.** Each window tab still shows the dominant pane state
  (precedence attention > busy > free) and, when > 1 active pane, a count of
  busy-or-attention panes. The rendering (a themed dot / badge, a count) is unchanged.
- [ ] **Both terminal paths.** The daemon-default path derives busy/free from
  `is_shell`; under `RIFT_TERMINAL_LEGACY` (no `is_shell`) it derives from the
  client-side structural fallback. Neither path uses output byte-flow.
- [ ] **Live, event-driven, no poll.** Indicators update as the foreground process,
  alternate-screen mode, OSC phase, or bell changes — off the snapshot and the
  per-pane byte stream — with the recency idle-tick removed (no periodic re-render).
- [ ] **Agent-agnostic.** A `grep` of the activity path finds no agent name, no
  `pane_current_command` matched against a command name (only compared, via tmux,
  to the default-shell), and no parsing of pane content.

## Scope

### In scope

- **Structural per-pane busy/free** (`crates/terminal`, `PaneView` /
  `ActivityTracker`): replace the OSC-133-lifecycle-plus-output-recency derivation
  with a pure classifier over structural inputs:
  - `is_shell: Option<bool>` — the pane's tmux foreground-process flag, pushed down
    from `SessionView::apply_snapshot` (`Some(false)` → a command is running,
    `Some(true)` → at the shell, `None` → unavailable on the legacy path);
  - `alt_screen: bool` — the client `Term`'s `TermMode::ALT_SCREEN` (a full-screen
    foreground TUI), read at derivation time;
  - `osc_executing: bool` — the client OSC-133 phase is `Executing`.

  Recommended derivation (authority: tmux field, then client structural fallback):
  `busy = match is_shell { Some(true) => false, Some(false) => true, None => alt_screen || osc_executing }`.
  Attention (the bell) overlays as today.
- **Remove the byte-flow machinery**: delete the output-recency fallback
  (`ACTIVITY_IDLE_WINDOW`, `last_output`, `on_output` and its call in the PTY read
  loop), the OSC-133 trust window (`OSC133_TRUST_WINDOW`, `last_osc133`,
  `osc133_trusted`, the `on_osc133` calls), and the `ACTIVITY_IDLE_TICK` recompute
  in `SessionView` (no time-based decay remains). Keep the OSC event handlers
  updating `command_lifecycle` (the phase source for `osc_executing`).
- **Wire the foreground-process signal** (`SessionView::apply_snapshot`): pass the
  per-pane `pane_is_shell.get(id).copied()` (an `Option`, not collapsed to `false`)
  into the pane via a new `PaneView::set_foreground_shell(Option<bool>)`, alongside
  the existing `set_window_active` call, on every snapshot and at pane creation. The
  existing collapsed `entry.is_shell` / `WindowState.is_shell` (glyph) is untouched.
- **Attention unchanged**: bell-raised-when-window-inactive, cleared on the
  active-window transition and on local select (tab click, `Alt+1..9`,
  `select_window`); the single invariant and its acknowledgement paths are retained.
- **Aggregate & rendering retained**: `aggregate_activity`, `tab_state_slot`, the
  tab state slot (themed dot / attention badge / count), and `status_windows` keep
  their shape and their theme-token colors; only the per-pane input changes.
- **Child→parent live wiring retained**: `SessionView` keeps observing each
  `PaneView` (`cx.observe`) so a background window's alt-screen/OSC/bell transition
  re-renders the tab bar without the removed idle tick.
- **Tests**: pure-logic unit tests for the classifier — every
  `(is_shell, alt_screen, osc_executing, attention)` combination; the three
  dogfooding regressions as named cases (silent-but-running stays busy; a byte burst
  does not set busy; a bare shell stays free); the aggregate precedence + count
  (retained). Legacy-fallback path (`is_shell == None`) covered.

### Out of scope

- **A "working vs waiting" sub-state derived from output activity.** Reintroducing
  byte-flow to distinguish an actively-streaming agent from an idle-but-open one is
  exactly the interaction-dependence this spec removes. Busy = "a command is
  running"; "wants your input" stays the best-effort bell/attention overlay.
- **Any agent detection** — no matching `pane_current_command` against agent names,
  no spinner/prompt-glyph parsing of `capture-pane`, no agent `@claude_state` hooks.
  Forbidden by the constitution.
- **tmux `monitor-activity` / `monitor-silence` / `monitor-bell`** — coarser than
  the structural signal and mutates the user's shared session; rejected in the
  archived spec, still rejected.
- **Daemon / protocol / `tmux-core` changes** — `is_shell` (#510), the bell, and the
  alternate-screen mode are already present; the feature stays client-side in
  `crates/terminal`.
- **A per-pane in-grid indicator, notifications/sounds/OS badges, a configurable
  threshold or color UI** — out, as in the archived spec.
- **Reworking the aggregate, the count semantics, the tab layout, or the colors** —
  retained as shipped (theme tokens: busy = `success` dot, attention = `danger`
  badge, index/count = `muted_foreground`).

## Constraints

- Client-side only, in `crates/terminal`; no daemon / protocol / `tmux-core` change,
  no mutation of the user's shared tmux session.
- **Theme tokens only** (Catppuccin Mocha via gpui-component) — the rendering path
  already uses `cx.theme().success` / `.danger` / `.muted_foreground`; no hardcoded
  hex. (This supersedes the archived spec's stale "hardcoded Catppuccin `rgb(...)`
  literals" decision — the tab chrome moved to theme tokens under
  `spec-cockpit-chrome`.)
- Agent-agnostic: busy/free derives only from tmux format fields (`is_shell`) and
  client `Term` structural state (alternate-screen mode, OSC-133 phase); attention
  from the terminal bell (a byte-stream *event*, not content). No agent-name match,
  no pane-content parsing.
- Crate boundaries via `lib.rs`; the pure classifier stays GPUI-free and
  unit-testable. No `.unwrap()` in library code (`.expect("reason")` only for true
  invariants). Reuse the existing gpui-component `Tab` / dot idiom; do not fork it.
- `is_shell` refreshes on the layout-snapshot / format-subscription cadence
  (`pane_current_command` is a subscription trigger), so busy/free tracks a
  process change promptly, not on a slow poll.

## Human prerequisites

None. Pure client-side rederivation from signals rift already parses (`is_shell`,
alternate-screen mode, OSC-133 phase, the bell); no new dependency, no protocol
change, no secrets, no external provisioning.

## Prior art

Consulted [prior-art.md](prior-art.md); this redesign refines the same Phase-18
concern ("Pane activity state on window tabs") and its Pattern 9 ("per-pane state
machine with bell/activity awareness").

- **Adopt the UX, avoid the mechanism** (unchanged from the archived spec): the
  color-coded dot + per-window aggregate roll-up from Arbor (working/waiting
  indicators), Claude Squad (`Instance` state), and zellij's status bar. **Avoid**
  Arbor's hardcoded agent detection and Claude Squad's `capture-pane` content
  hashing (last-N-lines spinner/prompt-glyph parsing) — both are agent-specific and
  forbidden here; `samleeney/tmux-agent-status` and `accessd/tmux-agent-indicator`
  (spinner/prompt parsing, per-agent config) are in the AVOID column for the same
  reason.
- **Structural signal, corrected.** A 2026-07 prior-art pass confirms OSC-133 prompt
  markers "don't really work in fullscreen apps, on the alt screen" (a documented
  emulator limitation) — validating that a full-screen TUI agent yields no per-turn
  OSC-133 cadence and that the alternate-screen mode is itself an agnostic
  "a full-screen program is running" signal. The honest agent-agnostic busy/free
  question — *is the foreground process the shell or a command* — is answered by
  tmux's `pane_current_command` vs default-shell, which the constitution sanctions as
  a tmux format field and which every agent, build, or TUI trips identically.
- rift-local grounding: `is_shell` (#510, `rift_protocol` →
  `SessionSnapshot.pane_is_shell`, `crates/app/src/main.rs` seam, consumed in
  `session_view.rs:apply_snapshot`), `TermMode::ALT_SCREEN` (already read in
  `pane_view.rs`), `PaneView::command_lifecycle` (OSC-133 phase), the bell
  (`alacritty_terminal::Event::Bell`), and the retained aggregate/render path
  (`aggregate_activity`, `tab_state_slot`, `session_view.rs:1839-1943`).

## Prior decisions

Decisions the implementor must respect; rationale included so edge cases can be
judged.

| Decision | Rationale | Date |
|---|---|---|
| **Supersede the OSC-133-lifecycle + output-recency derivation; make tmux `is_shell` the authoritative busy/free signal** | `is_shell` (#510, added after the archived spec) is tmux's own truth about the foreground process — structural, interaction-independent, byte-flow-independent, and stable across an agent's silent think phases. It directly fixes all three dogfooding regressions. | 2026-07-08 |
| **Remove output byte-flow from busy/free entirely** | Byte-flow recency is the root of interaction-dependence (mouse reports, redraw bursts on select/resize → busy) and cannot represent a running-but-silent process (ages to free within the idle window). It answers "is the pane repainting", not "is a command running". | 2026-07-08 |
| **Remove the OSC-133 trust window (#491)** | Its premise ("output 10 s past a marker means shell integration died") is false for any legitimate long-running foreground command; it discarded the correct signal for the exact case v1 targeted. The stale-marker case it guarded against is instead handled structurally: `is_shell == Some(true)` authoritatively reads free even if a client OSC phase is stuck `Executing`. | 2026-07-08 |
| **Busy = "a foreground command is running", not "actively emitting output"** | Keeps the archived spec's accepted property (an agent thinking mid-turn stays busy) but on a robust structural signal. A pane running `vim` / a build / a dev server reads busy — the honest agnostic meaning of "a command is running"; the feature cannot and must not special-case "agent". | 2026-07-08 |
| **Client-side structural fallback (alt-screen ∨ OSC-133 `Executing`) only where `is_shell` is unavailable** | The legacy path (`RIFT_TERMINAL_LEGACY`, slated for removal #285) sends no `is_shell`. The fallback stays structural and interaction-independent — no byte-flow — and covers both full-screen TUIs (alt-screen) and scrolling shell commands (OSC `Executing`). Never treat an absent `is_shell` as `false` (that would read every legacy pane busy). | 2026-07-08 |
| **Remove the activity idle tick** | With byte-flow gone there is no time-based decay; busy/free is fully event-driven (snapshot for `is_shell`, per-pane `cx.notify` for alt-screen/OSC/bell via the retained parent observation). One fewer busy poll. | 2026-07-08 |
| **Attention = the terminal bell, unchanged (best-effort 3-state)** | The bell remains the only agnostic "wants input" proxy; the raise-when-inactive / clear-on-view invariant and its acknowledgement paths are retained verbatim. If the bell proves unreliable at the QA gate, the documented fallback is 2-state (free/busy) without re-planning. | 2026-07-08 |
| **Aggregate, count, rendering, and theme tokens retained** | Only the per-pane busy/free input is wrong; the roll-up (precedence attention > busy > free, count = busy-or-attention panes) and the themed tab slot are correct and stay. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step
under this spec's milestone, created once this spec is merged to `develop` (the
issue-spec gate resolves the spec path against the default branch). This spec owns
the design; the issues own progress.

- Milestone: created after merge (Phase 18 — Pane-activity indicators v2)
- Issues: created from this spec once merged (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that
traces back here (planning gate).

## Verification

How Claude Code (or the developer) knows the whole spec is complete.

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
  passes; `app-check` compiles the app.
- [ ] Pure-logic tests for the classifier: `is_shell == Some(false)` → busy and
  `Some(true)` → free regardless of `alt_screen` / `osc_executing`; `None` →
  `alt_screen ∨ osc_executing`; attention overlays busy and free.
- [ ] **Regression: silent-but-running stays busy.** A pane whose foreground process
  is a command reads busy with zero output for well beyond the old 1500 ms window
  (no flicker to free).
- [ ] **Regression: interaction never sets busy.** A pane at the shell (`is_shell`
  true, no alt-screen, non-`Executing`) stays free across simulated
  hover/focus/select/scroll and a redraw byte burst — nothing but the structural
  inputs can flip it.
- [ ] **Regression: an idle shell reads free.** A never-ran / freshly-split /
  `exit`ed-to-prompt pane reads free.
- [ ] Pure-logic tests for the aggregate (retained): precedence attention > busy >
  free; `active_count` = busy-or-attention panes; empty / single / mixed windows.
- [ ] A state change in a **background** (non-active) window updates its tab dot live
  (verifying the retained per-pane observation), with **no** idle-tick timer present.
- [ ] Behaviour holds on the daemon-default path (`is_shell`) and under
  `RIFT_TERMINAL_LEGACY` (client-structural fallback); neither reads output byte-flow.
- [ ] `grep` of the activity/derivation path confirms no agent name, no
  `pane_current_command` matched against a command name, and no pane-content parsing.
- [ ] Every `docs/spec-pane-activity-indicators.md` reference in code comments is
  repointed to `docs/spec-pane-activity-v2.md`.
- [ ] **Milestone QA (dev channel)**: a real Claude Code pane reads busy the whole
  time it is open (working and thinking), attention when it rings the bell for input,
  and its window reads free only after the agent exits to a prompt; hovering /
  clicking / switching windows never changes busy/free; a second running pane bumps
  the count.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `is_shell` snapshot latency makes busy/free lag a fast command start/stop | `pane_current_command` is a format-subscription trigger, so the snapshot fires on the process change, not a slow poll. If the QA gate finds a perceptible lag, the documented follow-up is to OR the instant client signals (alt-screen ∨ OSC `Executing`) in as busy-*accelerators* while keeping `is_shell == Some(true)` as the free authority — not built now (minimal solution). |
| A pane running a non-agent command (`vim`, `htop`, a pager) reads busy | Correct and intended: busy = "a command is running", agent-agnostic. The feature cannot special-case "agent"; "wants input" is the bell overlay. Documented in Prior decisions. |
| The bell is unreliable for "waiting" — an agent may not ring it | Retained best-effort 3-state (archived-spec decision); a pane that never rings the bell never shows attention. QA-gate fallback is 2-state (free/busy) without re-planning. |
| Legacy path (`is_shell == None`) degrades to the client fallback | Explicitly in scope and tested; the fallback is structural (alt-screen ∨ OSC `Executing`), interaction-independent, and the legacy path is slated for removal (#285). |
| Removing the trust window reintroduces a stuck OSC `Executing` reading busy at a prompt | On the daemon path `is_shell == Some(true)` authoritatively overrides to free; on the legacy path a stuck `Executing` is the pre-existing best-effort baseline, unchanged by this spec. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: **Supersedes `docs/archive/spec-pane-activity-indicators.md` (Phase
  18).** That spec derived per-pane busy/free from OSC-133 command lifecycle with an
  output-recency fallback and a 10 s OSC-133 trust window (#491). Dogfooding showed
  three misfires for a full-screen TUI agent: (1) a working pane reads free — OSC-133
  does not fire on the alt screen, the single launch marker ages out of trust, and
  the byte-flow fallback ages a silent-but-working agent to free; (2) the indicator
  flips busy on interaction — hover/select/resize produce PTY byte-flow, which the
  fallback reads as busy; (3) idle shell panes light from incidental output. Root
  cause: the derivation answers "is the pane repainting" (byte-flow), not "is a
  command running" (structural). v2 rederives busy/free from tmux's own
  `is_shell` foreground-process field (#510, added after the archived spec),
  authoritative on the daemon path, with a client-side structural fallback
  (alt-screen ∨ OSC `Executing`) for the legacy path, and removes byte-flow, the
  trust window, and the idle tick. Attention (the bell), the aggregate, the count,
  and the themed rendering are retained. Client-side only in `crates/terminal`; no
  daemon / protocol / `tmux-core` change.
