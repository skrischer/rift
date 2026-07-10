# Spec: Workspace visibility rail — rail-driven area visibility + solo

> Status: DRAFT
> Created: 2026-07-10
> Completed: —

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
| **Terminal area: always-visible floor vs fully symmetric** | OPEN — resolved at the spec-acceptance gate (couples to the render-coupled grid risk; see the Terminal decision below). | 2026-07-10 |

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
      area flips its visibility; solo yields exactly one visible area (subject to
      the Terminal decision); re-toggling an area exits solo by re-adding it.
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
  grid until re-show. Mitigation: the Terminal decision (always-visible avoids it)
  plus a mandatory resize re-assertion on re-show extending the #596 observer to
  visible-set/solo transitions. Verified by the QA item above.
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

## The Terminal decision (resolved at the acceptance gate)

Whether the **Terminal** area is an always-visible floor or a fully symmetric
peer. The chosen option fully determines the Terminal rail icon's behaviour and
the solo cardinality — both derived here so the gate answer leaves nothing open:

- **Always-visible floor (recommended)** — the Terminal is always rendered; it
  cannot be hidden. Derived behaviour: its rail icon **solos the Terminal** (show
  only the Terminal, hide the other three — the safe direction, since the Terminal
  stays rendered), and re-clicking it or toggling any other area from the rail
  restores the previous visible set; soloing any *non-Terminal* area shows **that
  area + the Terminal** (never the area alone). Because the Terminal is never
  unrendered, the render-coupled grid hazard cannot arise. Aligns with
  `docs/vision.md` (the terminal agent is the primary actor; every GUI feature
  exists to keep agent work observable). Smallest, lowest-risk cut.
- **Fully symmetric** — the Terminal is a peer: hideable and soloable like any
  area. Derived behaviour: its rail icon toggles its visibility like the others;
  solo shows only the target (soloing a non-Terminal area hides the Terminal too).
  Requires the render-coupled grid mitigation to actually fire on Terminal re-show
  — re-assert `refresh-client -C` when the Terminal element is (re)built — a real
  chunk of render-path work beyond the #596 dock-toggle observer.

The recommendation is **always-visible floor**; the choice sets the Terminal
icon's behaviour, the solo cardinality, and the size of the grid-resize work.

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
