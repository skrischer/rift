# Spec: Workspace visibility rail — rail-driven area visibility + solo

> Status: COMPLETED
> Created: 2026-07-10
> Completed: 2026-07-11

Evolve the shipped left activity rail (#513), the gpui-component Dock (#325), and
the panel zoom (#716/#665) from a per-dock open/close model into a **rift-owned
visibility set + solo** model. Each workspace area — **Explorer+Editor as one**,
plus **Terminal**, **Diagnostics**, **Git** — has a rail icon; clicking it toggles
that area's visibility, where an inactive area is **not rendered** (its panel
element is not built), not merely collapsed. The panel zoom becomes **solo** (show
only that area, the rest deselected); re-toggling an area from the rail adds it
back. The visible set + solo target are rift-owned state, persisted across
restart, driving the dock beneath.

## Outcome

- [ ] The rail carries one icon per **area** — Explorer+Editor (one), Terminal,
      Diagnostics, Git — plus the existing Settings gear (not an area). Clicking an
      area icon toggles that area between **visible** (rendered) and **hidden**
      (its panel element not built); the rail's active state reads the rift-owned
      visible set, not `dock.is_open`.
- [ ] A hidden area is **not rendered** — its panel element is not built — while
      its underlying models and subscriptions stay alive, so re-showing it
      re-attaches with no reconnect and no lost reactive state (file tree, git,
      diagnostics, terminal scrollback).
- [ ] **Zoom = solo**: the per-panel zoom control shows only its area and hides the
      rest, driven through the rift-owned visible set (a single source of truth);
      re-toggling any area from the rail exits solo by adding that area back. The
      current direct `ToggleZoom` forwarding (#716) is superseded by routing the
      control through the visible set.
- [ ] **Explorer+Editor are one area**: toggling it hides/shows the Explorer (left
      dock) and the Editor (center region) together as a unit.
- [ ] The visible set + solo target **persist across restart** (the Phase-9
      window-state store), additively — an old state file with the fields absent
      loads to the default (all areas visible, no solo).
- [ ] The **terminal grid stays correct** across hide/solo/re-show — hiding then
      re-showing an area never leaves tmux frozen at a stale grid size (the
      render-coupled `refresh-client -C` hazard, see Constraints/Risks).
- [ ] No protocol / daemon change; no foundation-doc change (an app-internal UI
      evolution). `PROTOCOL_VERSION` unchanged.

## Scope

### In scope

- **Rift-owned visibility state** in `crates/app` (`workspace.rs`): a visible set
  over the fixed area enum `{ ExplorerEditor, Terminal, Diagnostics, Git }` plus a
  `solo_area: Option<Area>`, authoritative over dock/zoom rendering. The rail
  (`activity_rail.rs`) reads it for active state and dispatches area-toggle
  actions against it (replacing today's `ToggleExplorer`/`ToggleSourceControl`/
  `ToggleProblems` → `toggle_dock` open/close wiring).
- **"Not rendered" mechanic**: a hidden area's panel is not mounted/built in the
  Dock tree; its Entity models + detached subscription tasks (held by
  `WorkspaceView`, not the view render) stay alive, so re-show re-attaches. Solo
  reuses gpui-component's render-one path (the dock renders only the soloed
  subtree) but driven by the rift-owned set — reconciling **both** gpui-component
  zoom states (`DockArea.zoom_view` + `TabPanel.zoomed`) by intercepting the
  built-in `ToggleZoom` → `PanelEvent` path so neither is set behind the set's back
  (see Constraints).
- **Explorer+Editor as one area**: the left-dock Explorer and the center Editor
  region toggle together; Terminal remains its own center region. The center is an
  `h_split([Editor, Terminal])` (`workspace.rs:1000-1008`), so hiding the
  Explorer+Editor area removes the left dock and the Editor half, and the Terminal
  tab-panel expands to fill the center.
- **Persistence**: additive `visible_areas` + `solo_area` on `WindowState`
  (`window_state.rs`), `#[serde(default)]` with tolerant load, mirroring
  `recent_roots` / `DiffViewMode` (a field + a read-modify-write `save_*` helper);
  re-applied on launch in place of the current unconditional `set_*_dock` seeding.
  The hand-written `Default` must seed `visible_areas` to **all areas visible**
  (not `Vec::default()`, which is empty = blank workspace) and `solo_area` to
  `None`. The `Area` enum carries `#[serde(other)]` (or a field-level tolerant
  deserializer) so a present `visible_areas` with an unknown variant degrades that
  entry rather than failing the whole parse.
- **Terminal grid re-assertion**: whatever the Terminal decision (below), the spec
  must guarantee the terminal is correctly sized after a visibility/solo change —
  extending the existing #596 dock-toggle resize-reassert observer
  (`workspace.rs:1035-1062`) to visible-set / solo transitions.
- **Design contract**: this phase supersedes the activity-rail behaviour of
  `docs/spec-cockpit-chrome.md` / the Paper "Cockpit — IDE" artboard; the updated
  rail design is authored as the design artifact referenced below and reviewed at
  the spec-acceptance gate.

### Out of scope

- Drag-rearrange of areas, multiple docks, floating panels, per-user free layout —
  the area set is fixed and opinionated (prior-art AVOID list).
- The per-tmux-pane zoom (`resize-pane -Z`, `session_view.rs:288-291`) — a distinct
  in-terminal control, untouched.
- Any protocol / daemon change; any new reactive capability.
- Reworking the panels' internal content (Explorer, Editor, Git, Diagnostics
  render unchanged); this phase only governs area visibility + solo + persistence.

## Constraints

- **Bindings are entity-lifetime, not render-lifetime.** The tmux control-mode
  subscription lives on the `SessionView` entity and the daemon reactive feeds on
  `WorkspaceView` detached `cx.spawn` loops (`workspace.rs:525-864`); the reactive
  models are `WorkspaceView` `Entity` fields. A not-rendered panel keeps these
  alive — this is what makes "inactive = not rendered" safe (re-show re-attaches
  without a reconnect).
- **The tmux client grid size is render-coupled (load-bearing).** `refresh-client
  -C` is emitted only from the terminal render path — the `grid_observer` prepaint
  calls `resize_client_to_area` off the terminal pane's measured post-layout bounds
  (`session_view.rs:2536-2551`, `:870-901`). If the Terminal is not rendered, no
  resize is sent and tmux stays at its last grid until re-show. Today's #596 patch
  re-asserts on dock toggles only, not zoom/solo. Any design that can leave the
  Terminal unrendered MUST re-assert the resize on re-show (or keep the Terminal
  always rendered) — this is the primary technical risk and gates the Terminal
  decision below.
- **Single source of truth for solo.** rift's visible set is authoritative;
  gpui-component holds **two** internal zoom states — `DockArea.zoom_view`
  (`gpui-component .../dock/mod.rs:1127`) and per-`TabPanel.zoomed: bool`
  (`.../dock/tab_panel.rs:82`) — and the #716 header control forwards
  `gpui_component::dock::ToggleZoom` (`workspace.rs:1416-1418`), whose
  `PanelEvent::ZoomIn/Out` path mutates **both** in parallel inside gpui-component.
  Making rift's set authoritative therefore requires **intercepting or replacing
  that built-in `ToggleZoom` → `PanelEvent` path** (so it cannot set `zoom_view` +
  `TabPanel.zoomed` independently and desync the header button's selected state
  from the rift set), not merely driving `zoom_view`. The panel zoom control
  becomes the solo trigger routed through the rift set.
- **Additive persistence.** `WindowState` is `#[serde(default)]` + tolerant load
  (`window_state.rs:92,359-364`); the new fields follow that contract so old state
  files still load (default: all areas visible, no solo).
- **No foundation-doc change.** App-internal UI evolution; supersedes the
  cockpit-chrome rail design via this spec's design artifact, not a
  constitution/architecture edit.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Rail icons toggle **area visibility** (rift-owned visible set), inactive = **not rendered**, replacing the current per-dock open/close toggle | Roadmap seed + prior-art (VS Code Activity Bar, Zed dock-toggle): icon-per-area, click toggles visibility, click on the active area hides it. The rail's active state inverts from `dock.is_open` to the rift-owned set. | 2026-07-10 |
| **Zoom = solo, rift-owned**: the visible set drives the dock's render-one; the panel zoom control becomes the solo trigger routed through the set, superseding #716's direct `ToggleZoom` forward | Prior-art verdict: keep the visibility SET as rift-owned state driving gpui-component Dock's show/hide so "not rendered" and "solo" compose; gpui-component's zoom already renders only the zoomed subtree (`dock/mod.rs:1127`), reused as the substrate, not a second source of truth. | 2026-07-10 |
| **Explorer+Editor are one area** | Roadmap seed. Today they are separate (left dock vs center tab); this phase unifies their toggle. | 2026-07-10 |
| **Persistence = additive `visible_areas` + `solo_area` on `WindowState`** | The Phase-9 store is the established layout-persistence pattern (`recent_roots`/`DiffViewMode`), `#[serde(default)]` + tolerant load — no schema break, old files default to all-visible. | 2026-07-10 |
| **Not-rendered preserves bindings** (entity/subscription lifetime, not render lifetime) | Investigated: tmux + daemon bindings live on Entity models + detached tasks held by the workspace, not the panel view; a not-built panel keeps them, re-show re-attaches. | 2026-07-10 |
| **Terminal grid resize is re-asserted on re-show** (extend the #596 observer to visible-set/solo transitions) | The tmux grid is render-coupled; without re-assertion a re-shown Terminal is stale. Mandatory regardless of the Terminal-visibility decision. | 2026-07-10 |
| **Terminal is fully symmetric** — a peer area, hideable and soloable like the rest, keeping its prominent side-by-side placement (the Editor ∥ Terminal center `h_split`); the render-coupled tmux grid is re-asserted on Terminal re-show | Resolved at the spec-acceptance gate. The terminal is rift's primary surface (the agent's star) and stays first-class and side-by-side — the visibility rail never re-arranges or demotes it. Full symmetry is chosen over an always-visible floor; the grid-resize-on-reshow work (extending the #596 observer to a Terminal (re)build) is accepted as the cost. | 2026-07-10 |

## Prior art

- `docs/prior-art.md` "Workspace visibility rail — prior-art index (Phase 39)":
  rail-icon-toggles-visibility (VS Code Activity Bar; Zed dock-toggle), solo/
  maximize = deselect the rest (Zed `workspace::ToggleZoom`, VS Code Toggle
  Maximized Panel, JetBrains Hide All Windows), and the Dock substrate (reuse
  gpui-component Dock #325; reference Zed `crates/workspace/src/dock.rs`). ADOPT
  the icon-rail + solo semantics with a rift-owned visible set; AVOID
  drag-rearrange / multiple docks / floating panels / free layout.
- Supersedes the activity-rail behaviour specified in
  `docs/spec-cockpit-chrome.md` (48px rail, per-dock toggle, active = dock
  visibility) and the Paper "Cockpit — IDE" artboard's rail.

## Design artifact

- Paper file `rift`, artboard **"Workspace visibility rail"** (this phase's design
  contract): the rail's per-area icons and their visible / hidden / solo states,
  the four areas' layout (Explorer+Editor as one, Terminal, Diagnostics, Git), and
  the solo interaction. Authored in this spec PR and reviewed at the
  spec-acceptance gate; supersedes the "Cockpit — IDE" artboard's rail behaviour.

## Human prerequisites

- none — an app-internal UI change; no secret, provisioning, or account required
  to build or test it.

## Verification

- [ ] `just ci` passes; `app-check` compiles `rift-app`.
- [ ] Unit (`crates/app/src/window_state.rs`): `visible_areas` + `solo_area`
      round-trip; a state file with the fields absent loads to the default (all
      areas visible, no solo); the tolerant-load contract holds.
- [ ] Unit (`crates/app`): the visible-set / solo state machine — toggling an
      area flips its visibility; solo yields exactly the target area (the Terminal
      is a normal peer — soloing a non-Terminal area hides the Terminal too);
      re-toggling an area exits solo by re-adding it.
- [ ] Behavioural (dev-channel QA): clicking each rail icon shows/hides its area
      (hidden = not rendered); the soloed area fills the workspace and the rest are
      hidden; re-toggling restores; the set + solo persist across an app restart.
- [ ] Behavioural (dev-channel QA): hide/solo away then re-show the Terminal (or,
      if always-visible, solo another area and restore) — the terminal is correctly
      sized with no stale grid, and its scrollback / tmux stream is intact (no
      reconnect). Diagnostics / Git / Explorer re-show with live reactive state.
- [ ] The rendered rail + states match the referenced Paper "Workspace visibility
      rail" artboard.

## Risks and mitigations

- **Render-coupled tmux grid (primary).** A not-rendered Terminal freezes the tmux
  grid until re-show. Mitigation: with the Terminal a fully symmetric peer, a
  mandatory resize re-assertion on Terminal re-show — extending the #596 observer to
  visible-set/solo transitions — re-emits `refresh-client -C` when the Terminal
  element is (re)built. Verified by the QA item above.
- **Two sources of truth for solo.** gpui-component holds its own `zoom_view`;
  driving it from rift's set without reconciling would diverge. Mitigation: the
  rift set is authoritative and the only trigger path; the panel zoom control
  routes through it.
- **Explorer+Editor unification.** They live in different dock regions today (left
  dock vs center); toggling them as one must not desync their individual panel
  state. Mitigation: the area maps to both mounts as a unit; QA covers hide/show of
  the combined area.
- **State drift on area set changes.** A persisted `visible_areas` referencing an
  area that no longer exists must degrade gracefully. Container `#[serde(default)]`
  only fills *missing* fields — a *present* list with an unknown enum variant fails
  the whole parse, and the tolerant `load` then resets the entire `WindowState` to
  default (losing bounds / theme / recents). Mitigation: `#[serde(other)]` on the
  `Area` enum (or a field-level tolerant deserializer) so unknown entries are
  dropped, not fatal; covered by a dedicated test.

## Terminal: fully symmetric (resolved at the acceptance gate)

The **Terminal is a fully symmetric peer** — hideable and soloable like any area —
that keeps its **prominent side-by-side placement** in the center `h_split([Editor,
Terminal])`. The rail governs only visibility and solo; it never re-arranges or
demotes the terminal (the terminal is rift's primary surface, the agent's star).
Derived behaviour: the Terminal rail icon toggles its visibility like the others;
solo shows only the target (soloing a non-Terminal area hides the Terminal too, and
soloing the Terminal shows the Terminal alone). Because a hidden/soloed-away
Terminal is unrendered, the render-coupled grid mitigation is **mandatory**:
re-assert `refresh-client -C` when the Terminal element is (re)built — extending the
#596 dock-toggle observer to visible-set/solo transitions. The always-visible-floor
alternative was considered and rejected in favour of full symmetry; the
grid-resize-on-reshow work is the accepted cost.

## Tracking

- Design doc: this spec.
- Milestone + issues: created at the spec-acceptance gate / after merge.

## Decision log

- 2026-07-10: Spec drafted. Rail-driven visibility set + solo, rift-owned,
  persisted in the Phase-9 window-state store; inactive = not rendered with
  entity-lifetime bindings preserved; Explorer+Editor unified; zoom becomes solo
  routed through the set (superseding #716's direct forward). Grounded on a code
  investigation: the load-bearing risk is the render-coupled tmux grid size, which
  ties the one open decision (Terminal always-visible vs fully symmetric) to the
  grid-resize work. Supersedes the cockpit-chrome rail design via this phase's
  Paper artifact. One open decision carried to the acceptance gate.
- 2026-07-10: Spec-review refinements folded pre-acceptance (VERDICT was
  REQUEST_CHANGES, both blockers surgical): the Terminal decision now derives the
  Terminal rail-icon behaviour + solo cardinality per option (so resolving the one
  open decision leaves nothing implicit); the solo single-source-of-truth names
  both gpui-component zoom states (`DockArea.zoom_view` + `TabPanel.zoomed`) and the
  built-in `ToggleZoom`→`PanelEvent` interception as the reconciliation point;
  persistence requires an all-visible `Default` and `#[serde(other)]` on the `Area`
  enum so an unknown persisted variant is not fatal; and the Explorer+Editor
  center-fill behaviour is stated.
- 2026-07-10: Terminal decision resolved at the acceptance gate — **fully
  symmetric** (a peer, hideable/soloable), keeping its **prominent side-by-side
  placement** (Editor ∥ Terminal center `h_split`); the rail never re-arranges or
  demotes the terminal (the agent's star). The grid-resize-on-reshow work is
  accepted. The design artboard's Section-B mocks are corrected to the real
  side-by-side layout (Explorer | Editor | Terminal), not terminal-under-editor.
- 2026-07-10: Issue #821 implemented. `apply_area_visibility`'s `Area::Terminal`
  no-op arm (#819/#820's deliberate deferral) is filled in: `Explorer+Editor`
  and `Terminal` both now route through one `apply_center_visibility`, which
  reads both areas' live visibility and rebuilds the center as the
  editor|terminal `h_split`, either side alone, or — a state not previously
  reachable, since the Terminal used to be an always-rendered floor — an empty
  zero-panel tab strip when both are hidden at once (e.g. any non-Terminal
  solo), mirroring `apply_diagnostics_visibility`'s existing "hidden = zero
  tabs" contract rather than inventing a placeholder view. `reconcile_visibility`
  (the solo path) now calls the three `apply_*_visibility` functions directly
  instead of looping `Area::ALL` through the dispatcher, since Explorer+Editor
  and Terminal would otherwise both trigger (and redundantly double-build) the
  same center. The grid re-assertion is an explicit `session_view.update(cx,
  |_, cx| cx.notify())` at the end of `apply_center_visibility`, added
  alongside — not folded into — the existing #596 dock-observer: that observer
  only watches the left/right/bottom `Dock` entities, which a pure Terminal/
  Explorer+Editor visibility change never touches, so it could not have covered
  this transition on its own. A code-level investigation of gpui-component's
  own render caching (`session_view.rs`'s `grid_observer` comment: "this view
  only re-renders when it is marked dirty") confirmed a freshly rebuilt
  wrapping `TabPanel` does not, by itself, force the wrapped `SessionView`
  entity to re-render — the explicit notify is required, not defensive
  belt-and-suspenders. The `SessionView`/`TerminalPanel` entities themselves are
  never dropped or recreated by any of this (only the surrounding dock chrome
  is rebuilt), preserving the entity-lifetime binding contract with no
  reconnect. The rail gained a fourth icon (`IconName::SquareTerminal`,
  `ToggleTerminal`), placed directly after Explorer in the rail's `.child(...)`
  order — the existing Diagnostics/Git relative order (a pre-#821 choice) is
  left untouched, out of this issue's scope.
- 2026-07-10 (issue #822): `Area` gained a **field-level tolerant
  deserializer** for `WindowState::visible_areas`/`solo_area` (deserializing
  each entry as `serde_json::Value` first, dropping one that fails to convert
  to `Area`) rather than `#[serde(other)]` on the enum itself — a catch-all
  variant would force every exhaustive `match area { .. }` in `workspace.rs`
  (e.g. `toggle_area`) to grow an arm for a value that can only ever arise
  from stale persisted data, never from a live rail click. The dock
  construction that used to hardcode "left open, right/bottom collapsed" now
  derives each dock's initial open state (and the Explorer+Editor center
  split) directly from the loaded `Visibility`, so `WindowState::default`'s
  all-visible seed takes effect on a fresh install exactly as it does after a
  toggle — the pre-existing construction-time tests asserting a collapsed
  right/bottom by default were updated to assert open (all-visible) instead.
- 2026-07-10 (issue #820 implementation): the "intercept or replace" built-in
  `ToggleZoom` -> `PanelEvent` path resolved to **replace**. Investigation found
  the native per-panel zoom button's `on_click` (gpui-component `tab_panel.rs`)
  calls `TabPanel::on_action_toggle_zoom` as a direct method invocation, not
  through `window.dispatch_action` — there is no capture-phase or event hook
  available to intercept that specific call from outside the pinned dependency.
  `DockArea.zoom_view` is also a single-`AnyView` render-one substrate, which
  cannot represent the combined Explorer+Editor area (two separate `TabPanel`s)
  or the Git area (right dock's source-control + diff split) as one solo
  target. Resolution: each of the five zoomable panels (`FileTree`,
  `EditorView`, `TerminalPanel`, `ProblemsPanel`, `SourceControlPanel`) now
  returns `Panel::zoomable() -> None` (disabling gpui-component's native zoom
  button and making its `ToggleZoom` handler an early-return no-op per its own
  `zoomable().is_none()` guard) and supplies a `toolbar_buttons()` header
  button dispatching a `Solo<Area>` action instead. Solo reuses the *same*
  rift-owned "not rendered" hide/show mechanism as the plain rail toggle
  (`apply_*_visibility`), reconciled across all four areas on every solo
  transition, rather than gpui-component's zoom_view/zoomed fields — those stay
  permanently at their default (`None`/`false`) and are no longer live state.
  The header button carries no live "currently soloed" indicator (the rail,
  already reactive via `Visibility::is_visible`, is the authoritative visual
  state); a follow-up could push a `soloed` flag into each panel if the header
  itself needs to reflect it. The Terminal's own render-level hide (needed so
  soloing a non-Terminal area visibly hides it, not just at the `Visibility`
  state-machine level) remains deferred to issue #821 alongside the Terminal's
  plain rail toggle — `apply_area_visibility`'s `Area::Terminal` arm is still a
  no-op, matching the existing #819 boundary.
- 2026-07-11 (issue #856): The rail's per-area hues resolve to theme tokens,
  not the artboard hexes, wherever a token's live value already matches (or
  the codebase already substitutes it for the same artboard reference):
  Explorer+Editor -> `theme().blue` (`#89b4fa`, exact), Diagnostics ->
  `theme().red` (`#f38ba8`, exact), Git -> `theme().green` (`#a6e3a1`, exact),
  solo -> `theme().magenta` (`#cba6f7`, exact) — all base `ThemeColor` tokens
  under the shipped Catppuccin Mocha theme, confirmed against
  `assets/themes/catppuccin-mocha.json`'s `base.*` values. Terminal has no
  dedicated peach/amber base token (`ThemeColor` only carries red/green/blue/
  yellow/magenta/cyan), so it resolves to `theme().warning` instead of the
  artboard's `#FAB387` — the exact substitution `file_icons::TintRole::Warning`
  already made for the identical artboard peach reference on the `.rs`
  file-type glyph, reused here for consistency rather than reintroducing the
  hex. No area hue is hardcoded. `RailState` gained `solo: Option<Area>`
  (`workspace.rs`'s `Visibility::solo` fed in at the `activity_rail::render`
  call site), and `activity_rail.rs` now imports `crate::workspace::Area` to
  type it and to pick each hue — the one deliberate exception to the module's
  prior "never names `Area`" boundary, mirroring `file_tree.rs`'s existing
  `workspace::{solo_button, SoloExplorerEditor}` import for the same reason.
  The Explorer+Editor icon swapped `IconName::Folder` -> `IconName::PanelLeft`
  and the Git icon swapped `IconName::Github` -> a vendored Lucide
  `git-branch.svg` (`assets/file_icons/git-branch.svg`, `currentColor` stroke),
  rendered through the same `Icon::empty().path(..).text_color(..)` custom-SVG
  path `file_tree.rs` already uses for file-type glyphs. The 2px active-icon
  accent bar is a literal `border_l_2()` + `border_color(tint)` on the
  `Button`, drawn only while the icon renders with a hue (visible or soloed;
  never while muted) — additive to the existing `Button::selected` surface-bg
  treatment, not a replacement for it.
