# Spec: cockpit chrome

> Status: READY
> Created: 2026-07-05
> Completed: —

Bring the cockpit's window chrome to the Paper design: a custom title bar
(logo + connection/session group + settings + window controls), a 48px
activity rail with live badges, the window-tab redesign (index, type glyph,
name, distinct busy/attention states), and per-pane headers with a live
running pill — replacing the interim pane sidebar.

## Outcome

- [ ] The title bar matches the design: rift logo + wordmark; connection group
      (status dot + "user@host · session <name>") hosting the phase-19 session
      switcher popover; settings gear; min/max/close — all in one custom 38px
      bar (no native OS title bar).
- [ ] A 48px activity rail toggles the dock panels — files, source-control
      (badge: changed-file count), diagnostics (dot: worst severity present),
      settings at the bottom — with active-panel state per the design. No dead
      icons: entries exist only for panels that exist (no search icon until a
      search panel exists).
- [ ] Window tabs match the design anatomy: muted index, type glyph, name,
      fixed state slot — busy = green 7px dot (+ busy-pane count when > 1),
      attention = 16px danger badge with "!", idle = empty slot; active tab
      merges with the body background; "+" creates a window.
- [ ] Every terminal pane has a 32px header: prompt glyph, pane command title,
      cwd (muted), live state pill ("● running" while busy, hidden when free),
      split + zoom actions — fed by the pane metadata that #442/#469 wired
      (pane_current_command / pane_current_path) and the existing
      ActivityTracker state.
- [ ] The interim 160px pane sidebar is removed (per gate decision) — tabs +
      pane headers carry its information.
- [ ] All colors/typography via theme tokens (mono for session name, cwd,
      numerics; Inter for labels) — zero new hardcoded hex.

## Scope

### In scope

- `app`/`terminal`: custom title bar built on the vendored gpui-component
  TitleBar (gallery-proven), hosting: logo + wordmark (mono bold), the
  connection/session group (relocates the phase-19 switcher popover anchor
  from the statusbar label into this group; statusbar keeps a plain session
  name), settings gear (opens the settings surface), window controls.
- `app`: activity rail as a fixed 48px flex column left of the dock: 36×36
  icon buttons with active state (surface bg + fg icon), wired to the
  existing panel-toggle actions (ToggleExplorer / ToggleSourceControl /
  ToggleProblems + OpenSettings); source-control badge = changed count from
  the existing git model; diagnostics dot = worst severity from the existing
  diagnostics model.
- `terminal`: window-tab rendering to design anatomy (index caption, type
  glyph, name, fixed 16px state slot, close ×, "+" button); attention becomes
  the 16px danger "!"-badge, busy stays the green dot + count; idle keeps the
  slot empty (lane alignment). Type glyph derived agent-agnostically from
  tmux's `pane_current_command` of the window's active pane: default-shell →
  prompt glyph, anything else → process glyph. No command taxonomy, no agent
  names.
- `terminal`: per-pane header (32px): prompt glyph, `pane_current_command`
  title (mono 13px), cwd muted (mono 12px, home-relative), state pill from
  `ActivityTracker` (running = success pill; attention = danger pill), right:
  split-horizontal + zoom icons issuing the existing tmux commands
  (`split-window`, `resize-pane -Z`).
- `terminal`: remove the pane sidebar (gate decision) and its width handling;
  keyboard pane navigation and the pane-select affordances it carried move to
  the headers/tabs (click header = focus pane).

### Out of scope

- Composite status line (phase 22), editor chrome (phase 23), prompt-input
  zone under the terminal (not in the seeded roadmap row — candidate for a
  later papercut/phase after dogfooding the pane headers).
- Search panel + rail icon (post-v1 scope per the roadmap).
- Any change to the activity SIGNALS (OSC-133/bell/recency — #428/#491 own
  those); this phase only renders their states per the design.
- macOS window-control conventions (Windows/Linux only, per constitution).

## Constraints

- Theme tokens only; reference values are Catppuccin Mocha (§0 of the design
  distillation): title bar / rail / tab strip on sidebar bg (ref #181825),
  active tab = editor bg (ref #1e1e2e), busy dot success (ref #a6e3a1),
  attention badge danger (ref #f38ba8) with white "!", selection/active
  surface (ref #313244).
- Typography: Caption 11/500 for tab indices and eyebrows; Label 13/500 for
  tab names; mono (JetBrains Mono family) for session name, cwd, command
  titles, and every numeric.
- gpui-component widgets over custom primitives (TitleBar, Tab/TabBar, Badge,
  Tooltip); no new dependencies.
- Agent-agnostic: the type glyph and pane titles derive ONLY from tmux-provided
  fields (`pane_current_command`, `pane_current_path`) — never from output
  content, never from an agent-name list.
- The window-controls behavior on Windows (drag, snap, double-click maximize)
  must not regress vs the native title bar — verify explicitly at the QA gate.
- No dead controls: every rendered icon acts.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Build on gpui-component `TitleBar` | Vendored, gallery-proven, handles Windows caption semantics; constitution: don't rebuild primitives | 2026-07-05 |
| Type glyph = default-shell vs other, from `pane_current_command` | The design shows per-window glyphs; a shell/non-shell split is the largest agent-agnostic distinction available from pure tmux metadata (no taxonomy, no agent detection). #442/#469 already deliver the field | 2026-07-05 |
| Attention rendering becomes the 16px danger "!"-badge (replacing the peach dot) | Wave-1 confirmed the hue-only dot is indistinguishable from busy at a glance; the design specifies two distinct shapes/sizes | 2026-07-05 |
| Omit the search rail icon | The design shows it, but v1 has no search panel — a dead control violates the polish bar; the rail gains it with the future search phase | 2026-07-05 |
| Session switcher anchor relocates into the title-bar connection group | The interim statusbar anchor was explicitly a phase-19 placement decision pending this phase | 2026-07-05 |
| Rail badges read the EXISTING client models (git changed count, diagnostics severity) | Both models already stream; phase adds rendering only | 2026-07-05 |

## Prior art

- `docs/prior-art.md` → Phases 19–26 index, Phase 21 row: gpui-component
  TitleBar/Dock/Tab/Badge (reuse); `zed` `crates/title_bar` (reference).

## Human prerequisites

None.

## Tracking

- Milestone: created after this spec merges (phase 21).
- Issues: one per implementable step, each referencing this spec path.

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] Visual (dev channel, QA gate): title bar, rail, tabs, and pane headers
      match the Cockpit — IDE artboard (layout, spacing, states); window drag/
      snap/double-click-maximize work on Windows
- [ ] Behavioral: bell in a background window shows the "!"-badge, activating
      the window clears it; a busy pane shows the green dot on its window tab
      AND the running pill in its pane header; both clear on idle
- [ ] Rail badges update live (stage a file → SCM count changes; introduce an
      error → diagnostics dot turns danger) without any refresh
- [ ] Session switcher opens from the title-bar group (phase-19 behavior
      preserved after relocation)
- [ ] No hardcoded hex in the new/touched rendering code (grep-verified)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Custom title bar regresses native window behaviors on Windows (snap layouts, drag zones) | gpui-component TitleBar is built for this; explicit QA-gate checks; fall back to retaining the native bar for window controls only if a blocker appears (documented deviation) |
| Removing the pane sidebar loses a navigation affordance someone relies on | Gate decision (human-approved); pane focus via header click + existing keyboard bindings; revert is a small PR if dogfooding misses it |
| `pane_current_command` flaps (e.g. shell spawning subprocesses) making the glyph flicker | Glyph follows the coalesced layout refresh cadence, not per-output; acceptable churn, no debouncing complexity in v1 |

## Decision log

- 2026-07-05: Spec drafted from the wave-1 design-gap analysis (title bar /
  rail / tab anatomy / pane header gaps all CONFIRMED) on top of the shipped
  papercuts (#428 bell gating, #442/#469 pane metadata, #424 client sizing).
