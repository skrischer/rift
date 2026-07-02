# Spec: Phase 16 — Command palette

> Status: DRAFT
> Created: 2026-07-02
> Completed: —

A keyboard-driven command palette — a modal fuzzy-filter list of rift's commands, opened with a shortcut, that dispatches the selected action. Built on `gpui-component`'s `searchable_list` inside a `Root` dialog; commands come from a small in-app registry over rift's registered GPUI actions. Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)).

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] A shortcut (`Ctrl+Shift+P` / `Cmd+Shift+P`) opens a **command palette**: a modal overlay with a filter input and a list of commands, each showing its display name and (where bound) its keybinding.
- [ ] Typing **filters** the list (subsequence/fuzzy match via `searchable_list`'s delegate); the top match is selectable with Enter, the list navigable with arrows; Escape dismisses.
- [ ] Selecting a command **dispatches its action** (e.g. Save, Go to Definition, Find References, and the dock/panel toggles) into the focused context, then closes the palette.
- [ ] The palette lists commands from a **single in-app command registry** (display name → dispatchable action), seeded with rift's existing `rift`-namespace actions plus the shell's panel/dock toggles — adding a command later is one registry entry.
- [ ] Opening, filtering, and dismissing the palette never disturb terminal/editor state; it is a transient overlay.
- [ ] Agent-agnostic: commands are app actions; no agent detection, no command that inspects agent output.

## Scope

### In scope

- **Command palette view** (`crates/app`): a modal built on `gpui-component`'s `searchable_list` (its `SearchableListDelegate`/`SearchableListState`) hosted in the existing `Root` dialog/overlay layer, bound to a `Ctrl/Cmd+Shift+P` action. Arrow/Enter/Escape navigation; the delegate filters the command list by the query.
- **Command registry** (`crates/app`): a small in-app list mapping a human display name (and optional keybinding hint) to a dispatchable action, seeded with the existing `rift`-namespace actions (Save, Go to Definition, Hover, Find References, …) and the Phase 10 dock/panel toggles (toggle explorer, focus terminal, etc.). Selecting an entry dispatches the action.
- **Dispatch into the right context**: the selected action dispatches so the focused surface handles it (the same actions the keybindings already dispatch); the palette closes on dispatch.

### Out of scope

- **Fuzzy file quick-open** (`Ctrl+P`-style open-any-file) — explicitly post-v1.0.0 per `roadmap.md` / `vision.md` (generic-editor depth scoped out of v1). This phase is a **command** palette (actions), not a file finder.
- **`nucleo` or any new fuzzy-matching dependency** — the command set is small (dozens of entries); `searchable_list`'s built-in delegate filtering suffices. `nucleo` is only justified by the deferred large-scale quick-open, which is out of scope ("as few dependencies as possible").
- **Command arguments / multi-step palettes** (a command that then prompts for input) — v1 dispatches parameterless actions.
- **Recently-used / frecency ordering, command categories/grouping** — a later refinement; v1 is a flat filtered list.
- **A settings/keybinding editor** — Phase 17 (theme & settings) territory.
- **New protocol / daemon change** — the palette is pure client UI dispatching existing client actions.

## Human prerequisites

None. Client-side UI over existing actions; no new dependency, no protocol change, no secrets.

## Constraints

- **Reuse `gpui-component` `searchable_list` + `Root` dialog** (constitution: don't rebuild primitives): the palette is a `searchable_list` delegate in the `Root` overlay layer (`crates/app/src/main.rs` already wraps the app in `Root`; the gallery already demos dialogs). No custom modal, list, or matcher.
- **No new dependency**: `searchable_list` provides the filtering; a small subsequence match in the delegate covers the command set. `nucleo` is deliberately not added (its scale is for file quick-open, which is out of scope).
- **Command registry is the extension seam**: a single list of `(display name, keybinding hint, action)` entries; new commands are added there, not by scattering palette knowledge. Seeded from the existing `#[action(namespace = rift)]` actions and Phase 10's dock/panel toggles.
- **Dispatch uses the existing action path**: selecting a command dispatches the same GPUI action a keybinding would — no parallel command-execution mechanism.
- **Depends on Phase 10 (dock shell)**: the dock/panel-toggle commands (and the shell that hosts the overlay) come from Phase 10. Editor/nav commands (Phases 4/5) already exist. Milestone depends on Phase 100.
- **Agent-agnostic, transient overlay** (constitution): no agent detection; opening/closing leaves terminal/editor state untouched.
- **No `.unwrap()` in library code**; no `todo!()`; an empty query lists all commands; no match shows an empty list, not an error.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-16 index row anchors this spec.

- **`zed` `crates/command_palette/src/command_palette.rs` — reference** (GPL-3.0, study-only): the fuzzy picker over registered actions, keybinding display, and dispatch-into-focused-context. rift takes the shape; instead of Zed's action registry it uses a small in-app command registry over `rift`-namespace actions.
- **`gpui-component` `searchable_list` — reuse** (already vendored): `SearchableListDelegate`/`SearchableListState` provide the filter-list-in-a-box the palette needs; the `Root` dialog layer hosts the modal (already used by the gallery). No `nucleo` (prior-art lists it, but for large-scale quick-open, which is out of scope here).
- rift-local grounding: rift's actions are `#[action(namespace = rift, no_json)]` (`crates/app/src/editor.rs`, `crates/terminal/src/lib.rs`); `Root` wraps the app (`crates/app/src/main.rs`); `gpui-component` ships `searchable_list` + `dialog`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Command palette (actions), not file quick-open** | `roadmap.md`/`vision.md` scope fuzzy quick-open to post-v1.0.0; this phase is the command palette only. | 2026-07-02 |
| **Reuse `gpui-component` `searchable_list` + `Root` dialog; no custom modal/list/matcher** | Constitution: don't rebuild primitives; `searchable_list` is exactly a filterable list-in-a-box, `Root` already hosts overlays. | 2026-07-02 |
| **No `nucleo` / new fuzzy dependency** | Minimal-dependency: the command set is small; `searchable_list`'s delegate filtering suffices. `nucleo`'s scale is for the deferred quick-open. | 2026-07-02 |
| **A single in-app command registry (name → action) is the source of commands and the extension seam** | GPUI actions are types without human names; a registry maps display names to dispatchable actions and keeps palette knowledge in one place. Seeded from `rift`-namespace actions + Phase 10 toggles. | 2026-07-02 |
| **Selecting dispatches the existing GPUI action into the focused context** | No parallel execution path; the palette is a discoverability layer over the same actions the keybindings drive. | 2026-07-02 |
| **v1 is a flat filtered list of parameterless commands** | Minimal-solution: frecency ordering, categories, and argument-taking commands are later refinements. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 16 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 160 — Command palette)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] `Ctrl/Cmd+Shift+P` opens the palette; typing filters; arrows navigate; Enter dispatches the selected command; Escape dismisses
- [ ] The listed commands come from the registry (existing `rift` actions + Phase 10 toggles); selecting one runs the same action its keybinding would
- [ ] Empty query lists all commands; a non-matching query shows an empty list (no error); dismissing leaves terminal/editor state untouched
- [ ] Registry test: the command list is well-formed (unique display names; each maps to a dispatchable action); the delegate's filter returns expected matches for a sample query
- [ ] `grep` confirms no agent detection and no new protocol variants; `cargo tree` shows no `nucleo`/new fuzzy dependency added
- [ ] Milestone QA (dev channel): open the palette, find and run a few commands (save, go-to-definition, toggle a panel) by typing

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `searchable_list`'s delegate API is more involved than a plain list | It is the vendored, intended primitive for exactly this; the gallery/story exercises it — read that usage first. Fallback: a plain `list` + `input` filter still avoids a new dependency. |
| GPUI action dispatch from a modal doesn't reach the previously-focused surface | Dispatch to the focused context the palette restores on close (the same target keybindings hit); a QA item confirms save/goto/toggle actually fire. |
| The command set is thin until later phases add actions | Acceptable: it grows with each phase; the registry makes additions trivial. The v1 set (editor/nav + Phase 10 toggles) is already useful. |
| Keybinding hints drift from actual bindings | Show the hint from the same binding source where feasible; a stale hint is cosmetic, not functional. |
| PR size | Small phase; decompose: (1) the command registry + seed entries; (2) the palette modal (searchable_list in Root) + shortcut + dispatch. ~200-line issues. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 16). Grounded on `gpui-component`'s `searchable_list` (delegate-based filter list) + `Root` dialog (already wrapping the app), rift's `#[action(namespace = rift)]` actions, and the absence of `nucleo` as a dependency. Constraint/precedent-determined: command palette (not quick-open); reuse `searchable_list`+`Root`; no new fuzzy dependency; a single command registry over `rift` actions + Phase 10 toggles; dispatch the existing action; flat parameterless list for v1. No genuinely-open decisions — the gate is acceptance + human-prerequisites (none) only.
