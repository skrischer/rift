# Spec: Phase 16 — Command palette

> Status: READY
> Created: 2026-07-02
> Completed: —

A keyboard-driven command palette — a modal subsequence-filter list of rift's commands, opened with a shortcut, that dispatches the selected action. Built on `gpui-component`'s public `list` + `input` primitives in the `Root` overlay; commands come from a small in-app registry over rift's registered GPUI actions. Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] A shortcut (`Ctrl+Shift+P` / `Cmd+Shift+P`) opens a **command palette**: a modal overlay with a filter input and a list of commands, each showing its display name and (where bound) its keybinding.
- [ ] Typing **filters** the list (a small subsequence match); the top match is selectable with Enter, the list navigable with arrows; Escape dismisses.
- [ ] Selecting a command **dispatches its action** (e.g. Save, Go to Definition, Find References, and the shell panel/dock toggles) into the focused context, then closes the palette.
- [ ] The palette lists commands from a **single in-app command registry** (display name → dispatchable parameterless action), seeded with rift's existing `rift`-namespace editor/nav actions plus the shell command actions this phase defines — adding a command later is one registry entry.
- [ ] Opening, filtering, and dismissing the palette never disturb terminal/editor state; it is a transient overlay.
- [ ] Agent-agnostic: commands are app actions; no agent detection, no command that inspects agent output.

## Scope

### In scope

- **Command palette view** (`crates/app`): a modal — a `gpui-component` `input` (filter) above a `gpui-component` `list` of matching commands — hosted in the `Root` overlay layer, bound to a `Ctrl/Cmd+Shift+P` action. Arrow/Enter/Escape navigation; a small subsequence match filters the command list by the query. `gpui-component`'s `searchable_list` is **not** reused directly — its `SearchableListState` is `pub(crate)` and only surfaces through the `Select`/`ComboBox` dropdown chrome (wrong UX for a keyboard-summoned modal); the public `list` + `input` + `dialog` primitives are the right building blocks. A short spike confirms the modal-list approach before the palette issue.
- **Wire the `Root` overlay layers into the shell**: `crates/app/src/main.rs` wraps the app in `Root`, but `Root::render_dialog_layer`/`render_sheet_layer`/`render_notification_layer` are only rendered in the gallery today — `WorkspaceView::render` must render them for any modal (incl. this palette) to appear in the shipped app. In scope.
- **Command registry** (`crates/app`): a small in-app list mapping a human display name (and optional keybinding hint) to a dispatchable **parameterless** action. Seeded with the existing `rift`-namespace editor/nav actions (Save, Go to Definition, Hover, Find References — argument-taking actions like the terminal's `SelectWindow(usize)` are deliberately omitted). A curated registry (not auto-discovery of every bindable action) is deliberate: the palette shows a chosen, human-named set, not everything.
- **Shell command actions** (gate decision, 2026-07-02: **in scope**): Phase 10 delivers dock/panel toggling via the `DockArea`'s built-in mouse controls, **not** as dispatchable `#[action]` types (see `spec-ide-shell.md`), so those commands do not exist yet. Phase 16 **defines them** — a small set of `#[action(namespace = rift)]` types (toggle explorer / problems / source-control panels, focus terminal, zoom active panel) wired to the Phase 10 `DockArea` entity — since they are the palette's most valuable content and no other phase owns them.
- **Dispatch into the right context**: the selected action dispatches so the focused surface handles it (the same actions the keybindings already dispatch); the palette closes on dispatch.

### Out of scope

- **Fuzzy file quick-open** (`Ctrl+P`-style open-any-file) — explicitly post-v1.0.0 per `roadmap.md` / `vision.md` (generic-editor depth scoped out of v1). This phase is a **command** palette (actions), not a file finder.
- **`nucleo` or any new fuzzy-matching dependency** — the command set is small (dozens of entries); a small subsequence match over the `list` suffices. `nucleo` is only justified by the deferred large-scale file quick-open, which is out of scope ("as few dependencies as possible").
- **Command arguments / multi-step palettes** (a command that then prompts for input) — v1 dispatches parameterless actions.
- **Recently-used / frecency ordering, command categories/grouping** — a later refinement; v1 is a flat filtered list.
- **A settings/keybinding editor** — Phase 17 (theme & settings) territory.
- **New protocol / daemon change** — the palette is pure client UI dispatching existing client actions.

## Human prerequisites

None. Client-side UI over existing actions; no new dependency, no protocol change, no secrets.

## Constraints

- **Reuse `gpui-component` `list` + `input` + `Root` overlay** (constitution: don't rebuild primitives): the palette composes public primitives — an `input` filter over a `list` of commands in the `Root` overlay. It does **not** reuse `searchable_list` (its `SearchableListState` is `pub(crate)`, surfacing only through the `Select`/`ComboBox` dropdown chrome — wrong UX for a modal). The overlay layers must be wired into `WorkspaceView::render` first (only the gallery renders them today).
- **No new dependency — a deliberate override of prior-art**: `prior-art.md`'s Phase-16 row recommended pairing with `nucleo` for the matcher; this spec **overrides** that for v1 — the command set is small (dozens of entries), so a small subsequence match suffices, honoring "as few dependencies as possible." `nucleo` remains the right choice when large-scale fuzzy *file* quick-open (post-v1.0.0) lands. (`prior-art.md`'s row is updated to match.)
- **Command registry is the extension seam**: a single list of `(display name, keybinding hint, action)` entries; new commands are added there, not by scattering palette knowledge. Seeded from the existing `#[action(namespace = rift)]` editor/nav actions and the shell command actions this phase defines.
- **Dispatch uses the existing action path**: selecting a command dispatches the same GPUI action a keybinding would — no parallel command-execution mechanism.
- **Depends on Phase 10 (dock shell)**: the shell hosts the overlay, and (if the shell command actions are in scope) the toggle/zoom actions dispatch to Phase 10's `DockArea` entity. Editor/nav commands (Phases 4/5) already exist. Milestone depends on Phase 100.
- **Agent-agnostic, transient overlay** (constitution): no agent detection; opening/closing leaves terminal/editor state untouched.
- **No `.unwrap()` in library code**; no `todo!()`; an empty query lists all commands; no match shows an empty list, not an error.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-16 index row anchors this spec.

- **`zed` `crates/command_palette/src/command_palette.rs` — reference** (GPL-3.0, study-only): the fuzzy picker over registered actions, keybinding display, and dispatch-into-focused-context. rift takes the shape; instead of Zed's action registry it uses a small in-app command registry over `rift`-namespace actions.
- **`gpui-component` `list` (`List`/`ListState`/`ListDelegate`) + `input` — reuse** (already vendored, fully public with their own `Render` impls): these compose the palette modal directly. `searchable_list` is deliberately **not** used — its `SearchableListState` is `pub(crate)` and only reachable through `Select`/`ComboBox`'s dropdown trigger chrome, the wrong UX for a keyboard-summoned modal. The `Root` overlay layer hosts the modal (its layers are demoed in the gallery but must be wired into `WorkspaceView`). No `nucleo` (prior-art lists it, but it pairs with the deferred large-scale file quick-open, out of scope here).
- rift-local grounding: rift's actions are `#[action(namespace = rift, no_json)]` (`crates/app/src/editor.rs`, `crates/terminal/src/lib.rs`); `Root` wraps the app (`crates/app/src/main.rs`) but its overlay layers render only in the gallery today; `gpui-component` ships public `list` + `input` + `dialog`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Command palette (actions), not file quick-open** | `roadmap.md`/`vision.md` scope fuzzy quick-open to post-v1.0.0; this phase is the command palette only. | 2026-07-02 |
| **Compose the modal from public `list` + `input` + `Root` overlay; do NOT reuse `searchable_list`** | `searchable_list`'s `SearchableListState` is `pub(crate)` and only reachable via `Select`/`ComboBox` dropdown chrome (wrong UX for a modal); the public primitives are the right build. Wiring the `Root` overlay layers into `WorkspaceView` is a prerequisite (only the gallery renders them today). | 2026-07-02 |
| **No `nucleo` / new fuzzy dependency — explicit override of prior-art** | `prior-art.md`'s Phase-16 row recommended `nucleo`; overridden for v1 because the command set is small (subsequence match suffices) and "as few dependencies as possible" governs. `nucleo` belongs to the deferred large-scale *file* quick-open. The prior-art row is corrected to match. | 2026-07-02 |
| **A single curated in-app command registry (name → parameterless action); not auto-discovery** | GPUI actions are types without human names; a curated registry maps chosen display names to dispatchable actions (argument-taking ones like `SelectWindow(usize)` omitted) and keeps palette knowledge in one place. Curation is deliberate — show a chosen set, not everything bindable. | 2026-07-02 |
| **Selecting dispatches the existing GPUI action into the focused context** | No parallel execution path; the palette is a discoverability layer over the same actions the keybindings drive. | 2026-07-02 |
| **v1 is a flat filtered list of parameterless commands** | Minimal-solution: frecency ordering, categories, and argument-taking commands are later refinements. | 2026-07-02 |
| **Shell command actions (dock/panel toggle, focus, zoom) in scope** | Gate decision: Phase 10 delivers toggling via mouse controls, not `#[action]` types, so those commands do not exist — Phase 16 defines them (wired to the `DockArea`), since they are the palette's most valuable content and no other phase owns them. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 16 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 160 — Command palette)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] `Ctrl/Cmd+Shift+P` opens the palette; typing filters; arrows navigate; Enter dispatches the selected command; Escape dismisses
- [ ] The listed commands come from the registry (existing `rift` editor/nav actions, plus the shell command actions if in scope); selecting one runs the same action its keybinding would; the `Root` overlay layers render in `WorkspaceView` (the modal actually appears)
- [ ] Empty query lists all commands; a non-matching query shows an empty list (no error); dismissing leaves terminal/editor state untouched
- [ ] Registry test: the command list is well-formed (unique display names; each maps to a dispatchable parameterless action); the subsequence filter returns expected matches for a sample query
- [ ] `grep` confirms no agent detection and no new protocol variants; `cargo tree` shows no `nucleo`/new fuzzy dependency added
- [ ] Milestone QA (dev channel): open the palette, find and run a few commands (save, go-to-definition, toggle a panel) by typing

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The modal-list build is more work than "reuse `searchable_list`" (its state is `pub(crate)`) | Resolved in design: compose the public `list` + `input` in the `Root` overlay; a short spike confirms the approach before the palette issue. |
| `Root` overlay layers are not rendered in `WorkspaceView` today (only the gallery) | Explicit in-scope: wire `render_dialog_layer`/`render_sheet_layer`/`render_notification_layer` into `WorkspaceView::render`; without it no modal appears. |
| GPUI action dispatch from a modal doesn't reach the previously-focused surface | Dispatch to the focused context the palette restores on close (the same target keybindings hit); a QA item confirms save/goto/toggle actually fire. |
| Shell command actions couple Phase 16 to Phase 10's `DockArea` API | If in scope (gate), the toggle/zoom actions dispatch to the `DockArea` entity Phase 10 owns; the milestone already depends on Phase 100, and a spike validates the dispatch path. |
| Keybinding hints drift from actual bindings | Show the hint from the same binding source where feasible; a stale hint is cosmetic, not functional. |
| PR size (understated at 2 issues given the overlay wiring + shell actions) | Decompose into 3: (1) command registry + (if in scope) the shell command actions wired to `DockArea`; (2) wire `Root` overlay layers into `WorkspaceView` + the palette modal (`list`+`input`) + shortcut; (3) dispatch + subsequence-filter + navigation polish. ~200-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec-acceptance gate. Human prerequisites confirmed none. The one genuinely-open item resolved by the developer: the **shell command actions (dock/panel toggle, focus, zoom) are in scope** — Phase 16 defines them wired to the Phase 10 `DockArea`. Also folded the residual Constraints wording nit (registry seed) to match. Spec flipped `DRAFT → READY` and accepted for merge.
- 2026-07-02: Review gate (fresh-context Agent review) — `REQUEST_CHANGES`, three blocking findings addressed. (1) The `nucleo` decision silently contradicted `prior-art.md`'s Phase-16 row (which recommends `nucleo` for the palette matcher); reframed as an **explicit override** with rationale, and the prior-art row corrected. (2) `searchable_list` is **not** directly reusable — `SearchableListState` is `pub(crate)`, reachable only via `Select`/`ComboBox` dropdown chrome; the design now composes the public `list` + `input` + `Root` overlay, and **wiring the `Root` overlay layers into `WorkspaceView`** (only the gallery renders them today) is explicit in scope. (3) Phase 10 delivers dock/panel toggling via mouse controls, **not** `#[action]` types, so those commands do not exist — Phase 16 defining the shell command actions is now a genuinely-open **gate** item (recommended: in scope, since no other phase owns them). Non-blocking folded in: argument-taking actions (`SelectWindow(usize)`) omitted from the curated registry; the curated-vs-auto-discovery choice stated as deliberate; PR-sizing bumped to 3 issues. `nucleo` absence confirmed accurate.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 16). Grounded on `gpui-component`'s `list`/`input`/`searchable_list`/`dialog`, `Root` (wraps the app), rift's `#[action(namespace = rift)]` actions, and the absence of `nucleo`. Constraint/precedent-determined: command palette (not quick-open); compose public `list`+`input` in `Root` (not `searchable_list`); no new fuzzy dependency (override prior-art); a curated command registry; dispatch the existing action; flat parameterless list for v1. One genuinely-open item carried to the gate: whether the shell command actions (dock/panel toggle, focus, zoom) are in Phase 16's scope.
