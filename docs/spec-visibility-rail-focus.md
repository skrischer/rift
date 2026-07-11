# Spec: Visibility rail focus & dispatch — toggles survive hide/solo

> Status: READY
> Created: 2026-07-11
> Completed: —

A focus/dispatch hardening follow-up to the Phase-39 visibility rail
(`docs/spec-workspace-visibility-rail.md`). Making every workspace area —
including the focus-holding **Terminal** — hideable and soloable, where a hidden
area is **not rendered**, exposed a latent defect: the rail's area toggles are
delivered as **global actions** (`window.dispatch_action`) handled at the
**non-focusable** workspace-root `div`, while the workspace's keyboard focus
lives on the panels (`WorkspaceView::focus_handle` delegates to the Terminal, and
the Terminal grabs focus aggressively). Hiding the currently-focused area strands
the window's focus on an **unrendered** element, so `window.dispatch_action` can
no longer reach the root's `on_action` handlers — every subsequent rail (and
keyboard) toggle is silently dropped until focus happens to land on a rendered
surface again. This phase makes area visibility robust against focus state:
focus is always re-homed to a rendered surface on hide/solo, the workspace owns a
stable always-rendered focus anchor for its action handlers, and the rail's
clicks drive the workspace toggle path directly rather than through
focus-dependent action dispatch.

## Outcome

- [ ] Toggling any area from the rail always takes effect, regardless of which
      area (if any) currently holds keyboard focus — hiding the focused Terminal
      never disables the rail or the other toggles.
- [ ] After any hide/solo transition that removes the currently-focused area,
      keyboard focus lands on a still-rendered surface (never stranded on an
      unrendered panel); typing always reaches a live target.
- [ ] The workspace root hosts a stable, always-rendered focus anchor for its
      `on_action` handlers, so keyboard / command-palette / agent-dispatched
      actions route even when no panel is focused (e.g. the degenerate all-hidden
      state).
- [ ] The rail's area-icon clicks drive the workspace toggle path **directly**
      (not via focus-dependent `window.dispatch_action`); the `Toggle*` / `Solo*`
      actions + their `on_action` handlers remain for keyboard / command-palette /
      agent-driven entry points.
- [ ] No change to the visible-set / solo state machine, its persistence, or the
      tmux grid re-assertion (Phase-39 behaviour preserved); no protocol / daemon
      change; `PROTOCOL_VERSION` unchanged.

## Scope

### In scope

- **Stable workspace focus anchor** (`crates/app/src/workspace.rs` +
  `crates/app/src/main.rs`): a dedicated `FocusHandle` owned by `WorkspaceView`,
  tracked on the root render `div` (`.track_focus(&self.focus_handle)` +
  `.key_context("Workspace")`) so the node carrying the `on_action` handlers is
  present in the dispatch tree every frame — even when no panel is
  rendered/focused. Because `WorkspaceView::focus_handle` (`workspace.rs:2269`)
  stops delegating to the Terminal, the #358 "Terminal focused by default"
  behaviour — realized in `main.rs::enter_workspace` (~L1516:
  `workspace.focus_handle(cx).focus(...)`) — MUST be switched to an explicit
  terminal-focus call there; left unchanged it would silently focus the root
  anchor and regress #358.
- **Focus re-homing on hide/solo**: a single helper picks the preferred
  still-visible area and, whenever a visibility/solo change removes the area that
  currently contains focus, moves focus there — used by `toggle_area`,
  `toggle_solo_area`, and `reconcile_visibility`. Preference order **Terminal →
  Explorer+Editor → Diagnostics → Git**, falling back to the workspace root
  anchor when nothing is visible. Focus is only moved when the focused area
  actually became hidden; an unaffected focus is left untouched. Detecting which
  `Area` currently holds focus **reuses the per-panel `contains_focused` pattern
  already in `zoom_active_panel`** (`workspace.rs:1889-1905`) so the two
  focus-detection paths stay consistent.
- **Rail dispatch decoupling** (`crates/app/src/activity_rail.rs` +
  `workspace.rs`) — **architectural hardening, not required for the freeze fix**
  (re-home + anchor already resolve it; this makes the rail focus-immune by
  construction): the area icons invoke the workspace's toggle path directly
  (a `cx.listener` bound to the `Entity<WorkspaceView>` — a weak reference, no
  retain cycle — not `window.dispatch_action`). The `Toggle*` / `Solo*` `Action`s
  and their root `on_action` handlers stay in place for the keyboard, command
  palette, and any agent-driven dispatch.

### Out of scope

- The visible-set / solo state machine, its persistence, and the tmux grid
  re-assertion (Phase 39 — unchanged; this phase only touches focus + dispatch
  wiring).
- The rail visuals, the area layout, the Settings gear and non-area rail entries.
- The per-tmux-pane zoom and the zoom/solo *semantics* (Phase 39 owns them).
- Any protocol / daemon / foundation-doc change.

## Constraints

- **The defect is focus-coupled action dispatch.** `window.dispatch_action`
  routes from the currently-focused node up its dispatch path; the root `div`
  that registers the `on_action` handlers (`workspace.rs:2391` ff.) is
  non-focusable, and focus lives on the panels — `WorkspaceView::focus_handle`
  (`workspace.rs:2269`) delegates to `session_view.focus_handle`, and the
  Terminal grabs focus. A hidden (not-rendered) focused panel leaves the window's
  focus handle pointing at an unpainted element, so the dispatch path no longer
  contains the root handlers and the action is dropped.
- **Entity / binding lifetime is unchanged.** Phase 39 keeps `SessionView`,
  `editor`, `problems_panel`, etc. alive across hide/show (their reactive /
  tmux bindings survive). Re-homing focus must **only move the focus target** —
  never drop, rebuild, or re-subscribe a panel entity.
- **gpui does not auto-clear focus on unmount.** A focus handle stays "focused"
  even when its element is not painted this frame; therefore the re-home must be
  explicit, and the root `track_focus` gives an always-valid fallback dispatch
  node.
- **Preserve existing focus semantics.** #358's "Terminal focused by default" +
  the `FocusTerminal` action, and `zoom_active_panel`'s focused-area detection
  (`workspace.rs:1883-1905`, which reads each panel's focus to choose the solo
  target), must stay coherent after the change.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Root cause is **focus-coupled action dispatch**, not the visible-set state machine | Code investigation: `Visibility` (`workspace.rs:394-473`) and the `apply_*` functions are correct in isolation; the freeze is `window.dispatch_action` failing to reach the non-focusable root handlers once the focused panel is unrendered. | 2026-07-11 |
| Fix composes **re-home focus on hide + stable root focus anchor + decouple the rail from action dispatch** | The three are complementary: re-home fixes the freeze and keeps the keyboard alive; the root anchor guarantees a valid dispatch node in the degenerate all-hidden case; decoupling makes rail clicks focus-immune by construction (the rail→workspace relation is direct — routing it through global action dispatch was the coupling that broke). | 2026-07-11 |
| Focus re-home preference **Terminal-first** | `docs/vision.md`: the terminal is rift's primary surface / the agent's star — restore focus there first when it is visible. | 2026-07-11 |
| **Keep** the `Toggle*` / `Solo*` actions + `on_action` handlers (decouple only the rail's mouse path) | Keyboard shortcuts, the command palette, and agent-driven dispatch still route through these actions; only the rail's own click path is decoupled from focus. | 2026-07-11 |
| **No** design artifact | No UI-surface change — the rail visuals and area layout are untouched; this is a behaviour / focus-architecture fix. | 2026-07-11 |

## Prior art

- `docs/prior-art.md` "Workspace visibility rail — prior-art index (Phase 39)"
  (the dock-substrate row): **Zed `crates/workspace/src/dock.rs`** — Dock
  entities, panel open / zoom lifecycle. The **workspace-owned focus anchor +
  re-home on panel hide** pattern is extrapolated from the Zed monorepo
  (`crates/workspace/src/workspace.rs`'s workspace-level focus handle +
  `focus_in`/`focus_out` + per-panel focus lifecycle) — beyond the letter of the
  index row, noted here as the spec's own elaboration; **study-only** (Zed's
  Workspace/Project focus is tightly coupled, GPL-3.0), reimplemented minimally
  against rift's own `WorkspaceView`.
- Extends the focus/dispatch wiring introduced by
  `docs/spec-workspace-visibility-rail.md`; supersedes none.

## Human prerequisites

- none — an app-internal focus/dispatch fix; no secret, provisioning, or account
  required to build or test it.

## Verification

- [ ] `just ci` passes; `app-check` compiles `rift-app`.
- [ ] Unit (`crates/app`): the focus-target-selection helper picks the preferred
      visible area for a given visible-set / solo state (Terminal-first; falls
      back down Explorer+Editor → Diagnostics → Git; yields the root-anchor
      fallback when nothing is visible). Pure-logic, directly testable.
- [ ] The three existing tests that encode the old
      `focus_handle == session_view.focus_handle` delegation contract are updated
      to the new anchor semantics, not left red:
      `test_workspace_focus_delegates_to_the_terminal` (`workspace.rs:3193`) and
      the command-palette / settings dialog tests (`workspace.rs` ~L4040 /
      ~L4064). The replacements assert the workspace root anchor is focusable and
      hosts the root `on_action` handlers, and that startup focus
      (`main.rs::enter_workspace`) still lands on the Terminal.
- [ ] Behavioural (dev-channel QA): with the Terminal focused, hide it from the
      rail → every rail icon still toggles its area, and the Terminal re-shows;
      keyboard shortcuts still route.
- [ ] Behavioural (dev-channel QA): hide the focused area — each of Terminal /
      Explorer+Editor / Diagnostics / Git in turn — → focus lands on a visible
      surface and typing reaches it; no toggle freezes.
- [ ] Behavioural (dev-channel QA): solo an area, then exit solo via the rail →
      toggles stay responsive throughout; a previously-focused-but-now-hidden
      area never strands input.
- [ ] Behavioural (dev-channel QA): the degenerate all-areas-hidden state → the
      rail still re-shows any area (the root anchor keeps action dispatch alive).
- [ ] Phase-39 behaviour intact: the visible-set / solo, persistence across
      restart, and the terminal grid correct on re-show — no regression.

## Risks and mitigations

- **Changing the `focus_handle` delegation could alter default focus or break
  `FocusTerminal` / `zoom_active_panel`'s focused-area detection.** Mitigation:
  keep an explicit terminal-focus at startup; the re-home preserves the same
  focus semantics (it only *moves* focus when the focused area is hidden);
  `zoom_active_panel` still reads each panel's own focus handle, unchanged.
  Covered by the QA items above.
- **Re-homing mid-reconcile could fight the Terminal's aggressive focus grab or
  cause focus flicker.** Mitigation: re-home *only* when the focused area became
  hidden; otherwise leave focus untouched — no unconditional per-reconcile focus
  write.
- **Decoupling the rail via a captured handle could create a retain cycle.**
  Mitigation: use a `cx.listener` bound to the `Entity<WorkspaceView>` (gpui
  listeners hold a weak reference), not a strong captured clone.

## Tracking

- Design doc: this spec.
- Milestone + issues: created at the spec-acceptance gate / after merge.

## Decision log

- 2026-07-11: Spec drafted from the Phase-390 QA finding. Root cause:
  focus-coupled action dispatch — hiding the focused (unrendered) Terminal
  strands the window focus, dropping every subsequent rail / keyboard action
  dispatch (the reported "terminal won't reopen, then all toggles freeze"). Fix
  composes focus re-home on hide/solo + a stable workspace-root focus anchor +
  decoupling the rail's clicks from `window.dispatch_action`. No state-machine /
  persistence / grid change; no protocol / daemon change. No genuinely-open
  decisions carried to the acceptance gate (the milestone / roadmap framing of a
  Phase-39 follow-up fix is confirmed there).
- 2026-07-11: Spec-review refinements folded pre-acceptance (VERDICT was
  REQUEST_CHANGES; the root-cause diagnosis was CONFIRMED against the pinned gpui
  rev `4bee412` — `dispatch_path(root_node)` excludes the WorkspaceView div's
  handler node once the focused panel is unpainted). Folded: (blocking) added
  `main.rs::enter_workspace` (~L1516) to scope as the #358 startup-focus call
  site that must switch to an explicit terminal-focus, and named the three
  existing `focus_handle`-delegation tests (`workspace.rs:3193`, ~L4040, ~L4064)
  that must be updated to the anchor semantics; (non-blocking) reused
  `zoom_active_panel`'s `contains_focused` pattern for focused-area detection,
  named the `key_context` (`"Workspace"`), softened the Zed focus-model prior-art
  attribution to the spec's own elaboration, and marked the rail decoupling as
  architectural hardening (not required for the freeze fix).
