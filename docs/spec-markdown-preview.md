# Spec: Markdown preview

> Created: 2026-07-29

A per-tab source/preview toggle for Markdown files: in preview mode the editor tab
renders the buffer's Markdown as formatted output (read-only) instead of the text
editor, reusing the gpui-component Markdown renderer already used for LSP hover
cards. Roadmap Phase 51.

## Outcome

- [ ] A Markdown (`.md` / `.markdown`) tab can be toggled between source (the text editor) and preview (rendered Markdown), and back, with the toggle affordance shown only for Markdown tabs.
- [ ] Preview renders the tab's current buffer content via `gpui_component::text::markdown` (the same renderer used for hover cards, `crates/app/src/editor.rs:204,2718`), read-only.
- [ ] Preview updates live as the buffer changes — an agent editing the `.md` updates the open preview via the existing per-tab `observe_input` signal, matching rift's reactive premise.
- [ ] The mode is per tab: toggling one Markdown tab does not change another tab's mode, and a non-Markdown tab shows no toggle.

## Scope

### In scope

- A per-tab preview mode for Markdown tabs, toggled from an affordance in the editor tab/breadcrumb chrome (and a command-palette action).
- Rendering the current buffer content with the vendored `gpui_component::text::markdown` renderer, read-only.
- Re-rendering the preview when the tab's buffer changes (reusing the existing per-tab live-buffer feed / dirty subscription).

### Out of scope

- A side-by-side split (source + preview simultaneously) — v1 is a toggle; a split is a later refinement.
- Editing in preview, scroll-position sync between source and preview, or synced-scroll split.
- Rendering embedded remote images, Mermaid/diagram extensions, or math — whatever the vendored renderer supports is what ships; no new rendering dependency.
- Markdown for any surface other than an open editor tab (the explorer/diff surfaces are unaffected).

## Constraints

- Reuse the already-vendored `gpui_component::text::markdown` renderer (`crates/app/src/editor.rs:204`); no new dependency (constitution: reuse gpui-component, no premature abstraction).
- The editor is already tab-based with a **per-tab live-buffer feed** and dirty subscription (`editor.rs` module doc: "live-buffer feed is already per-tab (`arm_buffer_feed` runs per index)"), and a breadcrumb bar under the tab strip — the preview mode is per-tab state, and the live re-render rides the existing per-tab change signal (no new watch channel).
- Read-only: preview never mutates the buffer; toggling back to source returns the editable text editor with its buffer intact.
- Agent-agnostic: a rendering mode over file content; no agent awareness. The rendered content is the buffer rift already holds; no daemon/protocol change (preview is client-only).
- The renderer's feature set is whatever gpui-component's `markdown` supports; unsupported syntax degrades to plain text, never an error.

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 51 row: reuse the vendored `Markdown` element behind a source/preview toggle, read-only first.
- [Category 1: GPUI Applications & Components](prior-art.md#category-1-gpui-applications--components) — gpui-component's `Markdown` element (already vendored and used for LSP hover cards).

## Human prerequisites

- none

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Preview is a per-tab TOGGLE, not a side-by-side split, for v1 | Smallest useful surface; a split adds scroll-sync and layout scope deferred behind a real need | 2026-07-29 |
| Reuse `gpui_component::text::markdown` (the hover-card renderer); no new markdown dependency | Already vendored and in use; constitution reuse / no premature abstraction | 2026-07-29 |
| Preview is read-only; editing stays in source mode | A rendered view is not an editing surface; keeps the buffer authority in the text editor | 2026-07-29 |
| Toggle affordance: a button in the editor tab/breadcrumb chrome for Markdown tabs, plus a command-palette action | Discoverable on the tab, keyboard-reachable via the palette; both are cheap and consistent with existing chrome | 2026-07-29 |
| Live-updating preview — the open preview re-renders on buffer changes via the existing per-tab `observe_input` signal | Accepted at the gate; matches rift's reactive premise, and the per-tab feed already exists (low cost) | 2026-07-29 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged (one per implementable step)

## Verification

- [ ] `just ci` equivalent green for `app` (fmt + clippy `-D warnings` + tests); the crate compiles warm-target clean.
- [ ] QA: opening a `.md` file shows a preview toggle; toggling renders formatted Markdown (headings, lists, code blocks, links) read-only; toggling back restores the editable source with the buffer intact.
- [ ] QA: a non-Markdown tab shows no preview toggle.
- [ ] QA: two tabs, one in preview and one in source, keep independent modes.
- [ ] QA: editing the `.md` (or an agent editing it) updates the open preview without a manual re-toggle.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The vendored renderer lacks a needed feature (tables, task lists, images) | Out of scope to extend it; unsupported syntax degrades to plain text; note the limitation, file a follow-up if a gap bites |
| Live re-render on every keystroke in a large file janks | Ride the existing `observe_input` change signal (GPUI coalesces to frame cadence); throttle the re-render if a large file still janks, rather than rendering per keystroke |
| Preview and source diverge on scroll/focus | v1 is a full-tab toggle (not a split), so there is no simultaneous divergence to reconcile |

## Decision log

- 2026-07-29: Scoped from a develop-code read — the gpui-component Markdown renderer is already used for hover cards and the editor is tab-based with a per-tab live-buffer feed, so preview is a per-tab client-only mode reusing both; the only open point is whether the preview live-updates or renders on toggle.
- 2026-07-29: Spec-acceptance gate — accepted. Live-updating preview (via the existing per-tab `observe_input` signal). Spec review (PR #917) returned APPROVE with only non-blocking nits, addressed.
