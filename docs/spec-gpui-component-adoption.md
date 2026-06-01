# Spec: gpui-component adoption

> Status: READY
> Created: 2026-06-01
> Completed: —

Adopt `longbridge/gpui-component` as rift's UI primitive layer and migrate the Phase 2d chrome (tab bar, statusbar, theme) onto it, instead of hand-rolling further UI.

## Outcome

- [ ] `gpui-component` is a workspace dependency, building cleanly alongside `gpui` and `termy_terminal_ui` on a single shared GPUI git revision
- [ ] The app renders inside gpui-component's `Root`/theme context; a theme is applied app-wide
- [ ] The window tab bar is rendered with gpui-component's tab/dock component, replacing the hand-rolled tab bar in `session_view.rs`, with no regression in window switching (click) behavior
- [ ] The statusbar is rebuilt on gpui-component primitives, ready to host the Phase 2d data displays (git branch, command, session/window name, connection status)
- [ ] `cargo deny check licenses` passes (gpui-component is Apache-2.0)

## Scope

### In scope

- Add `gpui-component` (and `gpui-component-assets` if required) as a git dependency, pinned alongside a single GPUI revision
- Converge `gpui`, `termy_terminal_ui`, and `gpui-component` on one GPUI git rev (the central compatibility work)
- Wire gpui-component `Root` + `Theme` at the app root
- Migrate the window tab bar to gpui-component
- Rebuild the statusbar container using gpui-component primitives (the data wiring for individual fields stays in the Phase 2d issues)

### Out of scope

- File explorer / `VirtualList` (Phase 3)
- Dock splits / resizable panels for the terminal grid (Phase 3 — terminal layout stays driven by tmux for now)
- Code editor, LSP, diff views (later phases)
- Replacing the terminal widget itself — `termy_terminal_ui` stays as the terminal renderer

## Constraints

- `gpui` is a git dependency: `zed-industries/zed` rev `83de8a25e0ef71a8d762a148459bc863adaeb7e3` (v0.2.2)
- `termy_terminal_ui` is a git dependency: `termy-org/termy` rev `297bf90`, which transitively depends on a GPUI rev
- Cargo cannot link two incompatible `gpui` versions — all three crates MUST resolve to one GPUI rev, or the project will not compile (GPUI types do not unify across versions)
- GPUI is pre-1.0 and ships from git; expect breaking-change churn. Pin exact revs everywhere.
- License: gpui-component is Apache-2.0 (compatible with rift's GPL-3.0). Must pass `cargo deny check licenses`.
- Minimal-dependency policy: adopting gpui-component is justified because it replaces hand-rolled tab/dock/list/theme primitives rift would otherwise maintain (see prior-art.md).

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Adopt gpui-component as the UI primitive layer | Top-priority dependency in prior-art.md; ships dock, virtualized list/table, theme, scrollbar — avoids rebuilding primitives | 2026-06-01 |
| Adopt now and migrate 2d UI (not defer to Phase 3) | User decision; prevents accumulating more custom UI code | 2026-06-01 |
| Keep `termy_terminal_ui` as terminal renderer | Production-grade, already integrated; gpui-component does not replace the terminal widget | 2026-06-01 |
| GPUI-rev compatibility is a hard gate before migration | Two GPUI versions cannot interoperate; convergence must be proven first | 2026-06-01 |

## Tracking

Step decomposition lives as GitHub issues under the milestone.

- Milestone: [gpui-component adoption](https://github.com/skrischer/rift/milestone/2)
- Issues: created from the task outline below

Provisional step outline (becomes issues, not kept here once created):
1. GPUI-rev compatibility spike — prove `gpui` + `termy_terminal_ui` + `gpui-component` build on one shared rev. **Gate: if no shared rev exists, spec becomes BLOCKED pending an upstream bump.**
2. Add gpui-component dependency + wire `Root`/`Theme` at app root
3. Migrate window tab bar to gpui-component, preserving click-to-switch
4. Rebuild statusbar container on gpui-component primitives

## Verification

- [ ] `cargo build --workspace` succeeds with exactly one `gpui` entry in `Cargo.lock`
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo deny check licenses` passes
- [ ] Tab bar renders via gpui-component; clicking a tab switches windows (no regression vs current behavior)
- [ ] App renders inside gpui-component theme context; statusbar visible and themed
- [ ] No second `gpui` version pulled in transitively (verified in `Cargo.lock`)

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| gpui-component pins a GPUI rev incompatible with rift's `83de8a25e0` and/or termy's rev | Compatibility spike is step 1. Options if mismatched: (a) bump rift+termy to gpui-component's rev and re-verify termy builds; (b) pin gpui-component to rift's rev via a fork; (c) BLOCK and raise upstream. Do not proceed to migration until one GPUI rev builds everywhere. |
| GPUI pre-1.0 churn breaks the build later | Pin exact git revs; bump deliberately, never floating |
| Scope creep into Dock splits / file explorer | Explicitly out of scope; those are Phase 3 with their own specs |
| Tab bar migration regresses window switching | Verification requires click-to-switch parity before close |

## Decision log

- 2026-06-01: Spec created. GPUI-rev convergence identified as the make-or-break constraint; compatibility spike mandated as step 1.
