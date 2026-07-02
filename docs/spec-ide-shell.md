# Spec: Phase 10 — IDE shell (dock + resizable panels)

> Status: READY
> Created: 2026-07-02
> Completed: —

Replace the fixed three-column flex layout with a real dockable IDE shell built on `gpui-component`'s `DockArea`: the explorer, editor, and terminal become resizable, zoomable, toggleable dock panels arranged in named zones (left / center / right / bottom), so the reactive signal panels of phases 12–14 slot in without re-architecting the layout. This is the foundation phase of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The app root renders a `gpui-component` `DockArea` (inside the existing `Root`) instead of the hand-written three-column flex in `workspace.rs`; the fixed `EXPLORER_WIDTH` constant and the flex composition are gone.
- [ ] The three existing surfaces are dock panels: the file-tree explorer, the code editor, and the terminal (`SessionView`) each render as a `Panel` with a title and correct focus.
- [ ] Zones are established for the cockpit: explorer in the **left** dock, editor + terminal in the **center**, and the **right** and **bottom** docks exist as first-class (initially empty/collapsed) zones that phases 12 (source-control), 13 (problems) and 14 (status) dock into with no layout rewrite.
- [ ] Panels resize by dragging the handles between zones and between center splits (native `DockArea`/`StackPanel` resize); the layout no longer has a hard-coded pixel column.
- [ ] The left dock (explorer) can be toggled hidden/shown, and a panel can be zoomed to fill the shell and restored — both via the `DockArea`'s built-in controls.
- [ ] **Agent keystrokes are preserved**: with the terminal panel active, keyboard input still reaches the tmux pane exactly as before; focus follows the active panel.
- [ ] **The daemon-driven behavior is unchanged**: opening a file from the tree, editor write-back, the concurrent-write (`mtime`) signal, inline diagnostics, and LSP navigation all still work — the panel refactor preserves the cross-panel wiring and the daemon-stream bridges that live in the app root today.
- [ ] tmux structure stays inside the terminal panel: tmux windows/panes render via the terminal's own internal chrome (`SessionView`'s tab bar), **not** as dock tabs/panels — the dock composes IDE surfaces, tmux owns multiplexing.

## Scope

### In scope

- **Adopt `gpui-component`'s `DockArea`** (`crates/app`): construct it in the app root, render it under `Root` (with the app root view stacking any top chrome above it in a `flex_col`, mirroring the upstream `examples/dock` shell), replace the `workspace.rs` three-column flex.
- **Panel adapters for the three surfaces**: `FileTree` and `EditorView` implement `Panel` directly (both are app-crate views); the terminal is wrapped in an app-side `TerminalPanel` newtype that implements `Panel` and delegates render/focus to the inner `Entity<SessionView>` — so `rift-terminal` gains no *new public dock surface* and no crate-boundary API change (it already depends on `gpui-component`, but never on the dock system).
- **Default layout**: build the zone tree imperatively at startup — explorer (left), an editor|terminal split (center), right + bottom docks present but initially collapsed/empty — via `DockItem::split`/`tabs`/`tab` and `set_left_dock`/`set_center`/`set_right_dock`/`set_bottom_dock`.
- **Preserve the app-root wiring**: the daemon-stream bridge spawn loops and the cross-panel coordination currently in `WorkspaceView` (open-file event → editor, `mtime` concurrent-write signal, diagnostics push, nav replies) survive the refactor — the root keeps entity handles to the editor/tree panels while also handing them to the `DockArea`.
- **Resize, toggle, zoom, focus**: native resize between zones and center splits; toggle the explorer dock; zoom/restore a panel; focus follows the active panel with terminal keystrokes intact.

### Out of scope

- **Layout persistence across restarts** — remembering open panels, zone sizes, and the active tab — is **deferred** (gate decision, 2026-07-02). The `DockArea` makes this cheap (`dump()`/`load()` + `DockEvent::LayoutChanged`), and Phase 9 (`spec-window-state-persistence.md`) explicitly reserved panel-layout persistence for a follow-on behind a versioned schema. Phase 10 rebuilds the default layout each launch; persistence is a follow-on micro-step that reuses the window-state pattern (per-channel keyed file, debounced atomic save) against `DockAreaState`.
- **Explorer tree decoration + file operations** (Phase 11) — the file tree becomes *a dock panel* here with no behavior change; git/diagnostic decoration on the tree, create/rename/delete/move, reveal, and keyboard nav are Phase 11.
- **The signal panels themselves** — source-control + diff (Phase 12), problems (Phase 13), status bar (Phase 14): Phase 10 only establishes the zones they dock into.
- **Editor tabs / multiple open files** (Phase 15) — the editor stays single-buffer; it becomes *a* panel, its multi-file tab model is a later phase.
- **Command palette** (Phase 16) and **theme & settings** (Phase 17).
- **A custom client-side-decorated window titlebar** (window controls, menu bar) — the upstream example ships one, but cross-platform CSD (Windows caption buttons, X11) is real work with no Phase-10 payoff; the `DockArea` fills the window under the current OS chrome. A top strip is added when Phase 14's status bar / Phase 16's palette need it.
- **Floating/tiled panes, drag-panel-to-new-window, multi-window** — the `DockArea` supports tiles; rift's v1 uses the docked (non-floating) arrangement only.
- **Multi-worktree UI** (Scenario 2) — post-v1.0.0 per the roadmap.

## Human prerequisites

None. Pure client-side GPUI composition against an already-vendored dependency (`gpui-component`, pinned in `Cargo.toml`); no new dependency, no secrets, no provisioning, no protocol/daemon change.

## Constraints

- **Reuse, don't rebuild**: the constitution mandates `gpui-component` for dock/splits/tabs ("don't rebuild primitives"). Phase 10 uses `DockArea`/`Panel`/`DockItem`/`Dock`/`TabPanel`/`StackPanel` as-is; no hand-rolled resizable layout, no custom split tree.
- **Pinned rev**: `gpui-component` @ `9ad30e631e15f9bbba049717767bf4cd98e4f179` (already in `Cargo.toml`); the dock API in this spec is verified against that exact checkout. No dependency bump — the shell must work on the vendored rev.
- **Agent-first layout** ([vision.md](vision.md), [constitution.md](constitution.md)): the terminal (where the agent runs) is the primary actor and stays a prominent, first-class surface — it is **not** demoted to a thin bottom strip. **Default arrangement (gate decision, 2026-07-02): explorer in the left dock, editor and terminal as a horizontal split in the center (terminal at full prominence, preserving today's side-by-side UX), right + bottom docks collapsed.**
- **Agent-agnostic**: the shell composes surfaces; no code path inspects agent output or special-cases an agent. Terminal keystroke delivery to the active tmux pane must be byte-identical to today.
- **Crate boundaries**: `rift-terminal` must not learn about the dock. The `Panel` impl for the terminal lives in `crates/app` as a newtype wrapper around `Entity<SessionView>`; `rift-terminal` already depends on `gpui-component` but gains no new public dock surface.
- **Behavior preservation is the bar**: this is a layout refactor, not a feature change to the surfaces. Every daemon-driven behavior (`docs/spec-editor.md`, `docs/spec-lsp-navigation.md` outcomes) works identically after the refactor — the app-check build and the milestone QA gate validate this.
- **No `.unwrap()` in library code**; `thiserror` in libs, `anyhow`/`.expect("invariant")` only in the binary; no `todo!()`/`unimplemented!()` in merged code (no stub panels for unimplemented zones — an empty zone is a real collapsed `Dock`, not a placeholder view).
- **Headless-testable where it can be**: the panel-tree construction and any layout-state (de)serialization are unit-testable; the interactive dock behavior (drag-resize, zoom, focus) is validated at the visual milestone QA gate (`app-check` compiles per PR).

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-10 index row and Category 1 anchor this spec.

- **`longbridge/gpui-component` `crates/ui/src/dock/` — reuse (direct dependency, already vendored).** The dock API was read at the pinned rev: `DockArea::new(id, version, window, cx)` is the shell entity, rendered under `Root`; `Panel` is a low-cost trait (only `panel_name` required, plus `title`/`dump` in practice; supertraits `EventEmitter<PanelEvent>` + `Render` + `Focusable`); `DockItem::{split,tabs,tab,panel}` compose the tree; `set_{left,right,bottom}_dock` / `set_center` place zones with initial sizes; native resize handles live in `Dock`/`StackPanel`; `dump()`/`load()` + `DockEvent::LayoutChanged` provide optional JSON layout persistence; `register_panel` is required **only** on the persistence rehydrate path. **The template rift mirrors is `crates/story/examples/dock.rs`** (title bar over `DockArea` in a `flex_col`, default-layout builder, `LayoutChanged`-driven save). Per-view adaptation cost measured at ~10–40 lines each.
- **`zed` `crates/workspace/src/dock.rs` — reference only** (GPL-3.0, tightly coupled to Zed's `Project`/`Workspace`): the dock-zone + pane-tree + `Item` lifecycle model, studied for the zone taxonomy, not copied.
- rift's own local precedent: the archived component gallery (`crates/app/src/gallery`) exercised only `resizable`, deliberately dropping the dockable `StoryContainer` — so the full `Panel`/`DockArea` system is genuinely new to the product.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Adopt `gpui-component`'s `DockArea`/`Panel` system; no hand-rolled layout** | Precedent + constraint: the constitution names `gpui-component` for dock/splits/tabs ("don't rebuild primitives"); the pinned-rev source read confirms drag-resize, zoom, tabs, and split trees are all provided, per-view cost ~10–40 lines. A custom resizable layout would rebuild exactly what the vendored dep ships. | 2026-07-02 |
| **`FileTree` + `EditorView` implement `Panel` directly; the terminal is an app-side `TerminalPanel` newtype wrapping `Entity<SessionView>`** | Constraint-determined (constitution: "crate boundaries are contracts"): the tree/editor are app-crate views already; wrapping the terminal keeps `rift-terminal` free of the dock system and adds no public dock API to that crate. `Panel` is just a `gpui-component` trait both crates already have access to. | 2026-07-02 |
| **tmux windows/panes are NOT mapped to dock panels/tabs; the terminal is a single dock panel with its own internal tmux chrome** | Vision (tmux is the engine): multiplexing structure belongs to tmux and is rendered by `SessionView`; the dock composes IDE surfaces. Mapping tmux panes to dock nodes would duplicate/fight tmux's own layout. | 2026-07-02 |
| **Right + bottom docks are established as real (collapsed) zones in Phase 10, before their panels exist** | Roadmap sequence: Phase 10 is the foundation "panels 12–13 dock into"; creating the zones now (as empty collapsed `Dock`s, not placeholder views) means phases 12–14 add a panel, not re-architect the shell. No `todo!()` placeholder views. | 2026-07-02 |
| **Custom CSD window titlebar deferred; `DockArea` fills the window under current OS chrome** | Minimal-solution: cross-platform client-side decorations (Windows caption buttons, X11) are real work with no Phase-10 payoff. A top strip is introduced when Phase 14 (status) / Phase 16 (palette) need it. | 2026-07-02 |
| **Default layout: explorer (left) + editor\|terminal horizontal split (center) + right/bottom collapsed** | Gate decision: vision fixes the terminal-prominent principle (agent-first); the user chose the center editor\|terminal split — it preserves today's side-by-side UX and keeps the terminal at full prominence — over a traditional bottom-strip terminal or a terminal-hero arrangement. | 2026-07-02 |
| **Layout persistence deferred to a follow-on micro-step** | Gate decision: Phase 10 rebuilds the default layout each launch; persistence follows in its own step reusing the window-state pattern (per-channel file, debounced atomic save against `DockAreaState`). Keeps Phase 10 minimal; Phase 9 reserved layout persistence behind a versioned schema. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 10 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (Phase 10 — IDE shell)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; the CI `app-check` job compiles `rift-app` with the `DockArea` shell
- [ ] The app renders the explorer, editor, and terminal as dock panels in the agreed default layout; the old three-column flex and `EXPLORER_WIDTH` are gone (grep confirms)
- [ ] Dragging a zone/split handle resizes panels; the explorer dock toggles hidden/shown; a panel zooms to fill the shell and restores
- [ ] With the terminal panel active, typing reaches the tmux pane (agent keystrokes byte-identical to pre-refactor); switching focus between panels works
- [ ] Full daemon-driven regression still passes end-to-end: open a file from the tree, edit + save (write-back), external-change `mtime` conflict/auto-reload, inline diagnostics appear/clear, hover / go-to-definition / find-references — all unchanged
- [ ] tmux windows/panes render inside the terminal panel's own chrome, not as dock tabs
- [ ] `grep` confirms no agent detection introduced and `rift-terminal` gained no dock/`Panel` dependency
- [ ] Panel-tree construction (and layout-state (de)serialization, if persistence is in scope) has unit coverage
- [ ] Milestone QA (dev channel): the shell feels like an IDE — resize, toggle, zoom are smooth; the terminal stays prominent; nothing regressed from the daily-driver flow

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The daemon-stream bridge + cross-panel wiring (mtime, diagnostics, nav) breaks when the surfaces move under the `DockArea` | The bridges and coordination stay in the app root, which keeps entity handles to the editor/tree panels while also handing them to the dock; the daemon regression checklist in Verification is the gate. Split the refactor so behavior-preservation is one reviewable step. |
| Terminal keystroke delivery regresses (focus now routed by the dock, not the old terminal-delegated focus) | Explicit outcome + QA item; validate on the Windows host early (primary loop). The `Panel`/`Focusable` focus handle for the terminal delegates to `SessionView`'s existing handle. |
| `gpui-component`'s dock has rough edges on the pinned rev (X11 vs Windows resize, zoom) | The upstream `examples/dock` runs the full shell on this rev; validate resize/zoom on both targets at QA. If a specific control is broken on the rev, scope it out and note it — do not bump the pinned rev in this phase (that is the separate gpui-rev-bump track). |
| Scope creep into persistence / status bar / editor tabs | Hard out-of-scope lines above; the zones are the only forward-reach Phase 10 makes. |
| PR size: a shell refactor touches the app root broadly | Split into scaffold → panel adapters → default-layout+wiring → resize/toggle/zoom(/persistence) issues, each ~200–400 lines; the ~400-line split rule applies (scaffolding exception if needed). |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec-acceptance gate. Human prerequisites confirmed none. Both genuinely-open items resolved by the developer: (1) default layout = explorer left + editor|terminal center split + right/bottom collapsed (agent-first, preserves today's side-by-side UX); (2) layout persistence deferred to a follow-on micro-step (Phase 10 rebuilds the default each launch). Spec flipped `DRAFT → READY` and accepted for merge.
- 2026-07-02: Review gate (fresh-context Agent review) — `APPROVE`, no blocking findings. Non-blocking fixes folded in: Phase 11 added to the out-of-scope enumeration (the tree becomes a panel here, its decoration/file-ops are Phase 11); the in-scope terminal-wrapper bullet tightened to "no *new public dock surface*" (rift-terminal already depends on gpui-component, never on the dock); the newtype crate-boundary citation repointed from "CLAUDE.md rule 5" (which is specifically about `protocol`) to the constitution's general "crate boundaries are contracts." Template path `crates/story/examples/dock.rs` confirmed against the pinned checkout.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 10, the first of the v1.0.0 cockpit block). The dock API was resolved against a read-only clone of `gpui-component` at the pinned rev `9ad30e63` (`crates/ui/src/dock/`, `crates/story/examples/dock.rs`); findings recorded in Prior art. Constraint/precedent-determined: adopt `DockArea`/`Panel`; tree/editor impl `Panel` directly + terminal newtype wrapper; tmux structure stays inside the terminal panel; right/bottom zones established now as collapsed docks; custom CSD titlebar deferred. Two genuinely-open items carried to the gate: the default layout arrangement (terminal placement) and whether layout persistence is in Phase 10.
