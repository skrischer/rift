# Spec: Phase 15 — Editor tabs (multiple open files)

> Status: READY
> Created: 2026-07-02
> Completed: —

Turn the single-buffer editor into a tabbed editor that holds multiple open files, each a tab with its own buffer, cursor, dirty state, and diagnostics — with a tab bar (reusing the `gpui-component` `TabBar` the terminal already uses for tmux windows). Part of the v1.0.0 agent cockpit ([roadmap.md](roadmap.md)); a client-side refactor of the editor plus a fan-out of the workspace's per-open-file daemon wiring.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] The editor holds **multiple open files at once**, shown as tabs; a tab bar lists the open files, marks the active one, and shows a **dirty indicator** on files with unsaved changes.
- [ ] Opening a file (from the tree, or later the problems/nav paths) **opens a new tab or switches to the existing one** if that file is already open — it no longer replaces the single buffer.
- [ ] Each tab keeps its **own buffer state**: cursor position, scroll, dirty/base-`mtime`, and inline diagnostics are per-file, preserved when switching tabs.
- [ ] A tab can be **closed**; closing a tab with **unsaved changes prompts for confirmation** before discarding. Closing the active tab activates its **right neighbor** (or the left neighbor if it was the rightmost); closing the last tab returns the editor to its empty state.
- [ ] The **daemon-driven behavior holds per open file, concurrently across tabs**: the external-change (`mtime`) auto-reload/conflict, inline diagnostics, and the live-buffer LSP feed each address the tab holding that path. A file changing on disk updates its tab; diagnostics land on the tab for that file; **each dirty tab drives its own live-buffer feed** (the daemon already holds N live buffers keyed by path), so an unsaved edit in a background tab keeps surfacing its own diagnostics — switching tabs never closes another tab's live buffer.
- [ ] **LSP navigation is routed to the correct tab**: hover / go-to-definition / find-references issued from a tab land back on that tab; a response (which carries only a request id, never a path) can never misroute onto another tab's buffer.
- [ ] Agent-agnostic and remote-first as before: tabs are pure client UI over the existing buffer channel; no new protocol, no agent detection.

## Scope

### In scope

- **Multi-buffer editor** (`crates/app/src/editor.rs`): restructure `EditorView` from one open file to an ordered set of open buffers, each owning its `gpui-component` `InputState` (code-editor mode) plus the existing per-file bookkeeping made **per-tab**: path, base-`mtime`, dirty, diagnostics set, cursor/scroll, the `read_only` out-of-root flag (#195/#301), and the per-tab navigation UI state (hover content, jump list, back-stack). An active-tab index selects which buffer renders. Nav **request ids** stay a single editor-scoped monotonic counter (see the wiring fan-out below), not per-tab.
- **Tab bar**: a `gpui-component` `TabBar` above the editor content (the same `TabBar`/`Tab` pattern `SessionView` uses for tmux windows), showing each open file's name, a dirty dot, and a close affordance; clicking a tab activates it, clicking close closes it (with the dirty-confirm).
- **Open/switch semantics**: opening a path that is already open switches to its tab; a new path opens a new tab and activates it. The workspace's open-file event drives this instead of replacing the buffer.
- **Workspace wiring fan-out** (`crates/app/src/workspace.rs`): the per-open-file signals now address the tab holding the path. The `mtime` concurrent-write signal and the diagnostics push route per open path (to the tab holding it, ignored if none). **Each dirty tab drives its own live-buffer feed** — `BufferChanged` per tab independently (the daemon keys live buffers by path, `crates/lsp/src/document.rs`, so N can be live at once); `BufferClosed` fires **only on actual tab close or on save**, never on a mere tab switch (closing on switch would re-sync a still-dirty background tab's diagnostics from stale on-disk text — a regression against "diagnostics preserved per tab"). **Nav responses route by owned id**: request ids come from a single editor-scoped monotonic counter, each request records the issuing tab, and a `HoverResponse`/`DefinitionResponse`/`ReferencesResponse` (which carries only the id, no path) applies to the tab that currently owns that id — so a stale response can never land on the wrong tab.
- **Dirty-close confirmation**: a confirm dialog (`gpui-component` dialog via `Root`) before discarding an unsaved buffer.

### Out of scope

- **Split editors / multiple editor panes side by side** — post-v1.0.0; v1 is one tabbed editor panel.
- **Persisting open tabs across restarts** — deferred with the rest of workspace-layout persistence (Phase 9 / Phase 10 reserved it behind a versioned schema); a fresh launch opens no tabs.
- **Preview tabs** (VS Code-style transient tabs) — **not in v1** (gate decision, 2026-07-02: **every open is a persistent tab**, no pin/replace state machine). A preview-tab mode can be a later refinement.
- **Tab drag-reorder / drag-to-split** — a later refinement; v1 tabs are fixed-order (open order), closable, switchable.
- **Editor gutter change-bars, minimap, breadcrumbs** — separate editor-track items.
- **New protocol / daemon change** — tabs are client-side over the existing buffer channel (`OpenFile`/`SaveFile`/`BufferChanged`/`BufferClosed`).

## Human prerequisites

None. Client-side editor refactor over the existing buffer channel; no new dependency, no protocol change, no secrets.

## Constraints

- **Reuses the existing buffer channel and per-file logic, no new protocol**: `OpenFile`/`FileContent`, `SaveFile`/`SaveResult`/`SaveConflict`, the `mtime` signal (#188), inline diagnostics (#189), and the live-buffer feed already exist — Phase 15 multiplies the per-file state across tabs and routes each signal to the right buffer; it does not change the protocol.
- **The wiring fan-out is the load-bearing change**: today `workspace.rs` addresses one open path (`notify_editor_of_open_path_mtime`, `push_open_file_diagnostics`). With tabs these become per-open-path — diagnostics/mtime for a path route to the tab holding it (or are ignored if not open). The live-buffer feed is **per dirty tab** (not per active buffer): the daemon's `document.rs` already keys `open`/`live` by path, so multiple tabs are live concurrently; `BufferClosed` fires only on actual close or save, never on tab switch. Nav responses (id-only, no path) route to the tab owning the id, from a single editor-scoped id counter. This must preserve every current single-file behavior as the one-tab subset.
- **Reuses `gpui-component` `TabBar`/`Tab`** (already used by `SessionView`): no new tab primitive.
- **Dirty state is per tab** and drives both the tab indicator and the close-confirm; the existing save/conflict machinery (#188) is unchanged per buffer.
- **Focus and keystrokes**: the active tab's `InputState` holds focus; tab switching moves focus to the newly active buffer. Terminal keystroke delivery (agent-first) is unaffected (the editor is one panel among the dock).
- **Depends on Phase 10 (dock shell)**: the editor is a dock panel (Phase 10); the tab bar lives inside that panel, and Phase 10's daemon-wiring-preservation is what this phase fans out. Milestone depends on Phase 100.
- **Agent-agnostic** (constitution): no agent detection; tabs are file buffers.
- **No `.unwrap()` in library code**; no `todo!()`; a signal for a path with no open tab is ignored, not an error.

## Prior art

Consulted [prior-art.md](prior-art.md); the Phase-15 index row anchors this spec.

- **`gpui-component` `Tab`/`TabBar` — reuse** (already vendored, already used by `SessionView` for tmux windows): the tab-bar rendering, active index, and close affordance. Same pattern, new consumer.
- **`zed` `crates/workspace` (`Item`/`Pane`) — reference** (GPL-3.0, study-only): the item open/close/dirty lifecycle and "activate a neighbor on close" behavior. rift takes the lifecycle shape, not the code (Zed's `Item` is coupled to its `Workspace`).
- rift-local grounding: `EditorView` (`crates/app/src/editor.rs`, single-buffer: one `InputState` + per-file bookkeeping) is the refactor target; `SessionView` (`crates/terminal/src/session_view.rs`) already renders `TabBar::new(...).selected_index(...)` with `Tab::new().label().suffix(close)` — the pattern to mirror; `workspace.rs` holds the per-open-file daemon wiring to fan out.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Multi-buffer editor: each tab owns its `InputState` + per-file bookkeeping; an active index selects the rendered buffer** | The single-buffer state (cursor/dirty/diagnostics/mtime) must become per-file to preserve behavior per tab; one `InputState` per open file is the direct model. | 2026-07-02 |
| **Reuse `gpui-component` `TabBar`/`Tab` (the `SessionView` pattern)** | Constraint (constitution): don't rebuild primitives; the terminal already tabs tmux windows with this exact component. | 2026-07-02 |
| **Open an already-open path switches to its tab; a new path opens+activates a tab** | Standard editor behavior; avoids duplicate tabs for one file. | 2026-07-02 |
| **Workspace per-open-file signals fan out per tab; no new protocol** | Constraint: `mtime`/diagnostics/live-buffer signals exist and are per-path; tabs multiply the targets, routing each to the tab holding the path (or ignoring it). | 2026-07-02 |
| **Each dirty tab keeps its own live buffer; `BufferClosed` only on close/save, never on tab switch** | The daemon already holds N live buffers keyed by path (`crates/lsp/src/document.rs`); closing a background tab's live buffer on switch would re-sync its diagnostics from stale disk text — a regression against "diagnostics preserved per tab." | 2026-07-02 |
| **Nav request ids: a single editor-scoped monotonic counter; responses route to the id-owning tab** | Nav responses carry only an id, no path (`HoverResponse`/`DefinitionResponse`/`ReferencesResponse`); per-tab counters could coincide and misroute a stale response. One counter + per-request tab ownership makes routing unambiguous. | 2026-07-02 |
| **Closing the active tab activates the right neighbor (else the left)** | A concrete, predictable rule (not left to the implementer); simplest deterministic behavior. | 2026-07-02 |
| **Closing a dirty tab confirms before discarding; closing activates a neighbor; last close → empty** | Safe default (no silent data loss); `gpui-component`'s dialog via `Root` provides the confirm. | 2026-07-02 |
| **Open tabs are not persisted across restarts** | Deferred with workspace-layout persistence (Phase 9/10 reserved it behind a versioned schema); minimal-solution. | 2026-07-02 |
| **Every open is a persistent tab (no preview tabs in v1)** | Gate decision: simplest model, no pin/replace state machine; a preview-tab mode is a later refinement. | 2026-07-02 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 15 milestone. Created once this spec is `READY` and merged to `develop`.

- Milestone: created at `READY` (Phase 150 — Editor tabs)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes; `app-check` compiles the app
- [ ] Opening multiple files shows multiple tabs; the active tab renders its buffer; switching tabs preserves each buffer's cursor/scroll/dirty/diagnostics
- [ ] Opening an already-open file switches to its tab (no duplicate); a new file opens+activates a tab
- [ ] A dirty tab shows the dirty indicator; closing it prompts to confirm; confirming discards, cancelling keeps it; closing a clean tab is immediate; closing the active tab activates a neighbor; closing the last tab empties the editor
- [ ] Per-tab daemon behavior: a file changing on disk auto-reloads/conflicts on its tab; diagnostics land on the tab for that file; save/write-back works per tab
- [ ] Concurrent live buffers: with tab A dirty, switching to tab B keeps A's live buffer (A's diagnostics still reflect its unsaved edits, not disk); `BufferClosed` for A fires only when A is closed or saved — asserted with two dirty tabs
- [ ] Nav routing: hover / go-to-definition / find-references issued from tab A land on tab A even after switching to B; a stale response for a superseded id is ignored, never applied to another tab (test with overlapping ids across tabs)
- [ ] Unit tests: the open-set model (open/switch/close/activate-right-neighbor, dirty tracking) as pure logic over a seeded set
- [ ] `grep` confirms no agent detection and no new protocol variants
- [ ] Milestone QA (dev channel): open several files the agent touched, switch/close tabs, edit and save across tabs — everything behaves per file

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The single-buffer→multi-buffer refactor of a ~1789-line editor is large and risky | Split: (1) the open-set/nav-id model + per-tab state (likely 2 PRs — converting every scalar field, incl. `read_only`/nav ids/back-stack, to per-tab touches most of the file, straining the ~400-line guide); (2) tab bar + switch/close rendering the active buffer; (3) fan out the workspace per-open-file wiring (mtime/diagnostics + per-tab live buffers + nav-id routing); (4) dirty-close confirm + right-neighbor activation polish. Each preserves existing single-file behavior as the one-tab subset. |
| The workspace wiring fan-out silently drops a signal to the wrong/most-recent tab | Route strictly by path (mtime/diagnostics) or owned id (nav): a signal for path P addresses the tab holding P (or is ignored); nav responses address the id-owning tab; tests assert correct routing with several open. |
| Live-buffer feed desyncs across tab switches | Resolved in design: each dirty tab drives its own live buffer (the daemon holds N live buffers keyed by path, `crates/lsp/src/document.rs`); `BufferClosed` fires only on close/save, never on switch — verified against the daemon's actual multi-buffer contract, not assumed. |
| Focus lost/confused on tab switch | The active tab's `InputState` takes focus on switch; a QA item. |
| Preview-tab decision reopens scope | Gate decision; the recommended simple model needs no pin state machine. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-02: Spec-acceptance gate. Human prerequisites confirmed none. The one genuinely-open item resolved by the developer: **every open is a persistent tab** (no preview tabs in v1). Spec flipped `DRAFT → READY` and accepted for merge.
- 2026-07-02: Review gate (fresh-context Agent review) — `REQUEST_CHANGES`, two blocking findings addressed. (1) The live-buffer feed was wrongly specified to "follow the active buffer" (carried over from single-buffer `begin_open`), which would close a background dirty tab's live buffer on switch and re-sync its diagnostics from stale disk text — a regression. Corrected: the daemon already holds N live buffers keyed by path (`crates/lsp/src/document.rs`), so **each dirty tab drives its own live-buffer feed**; `BufferClosed` fires only on close/save, never on switch. (2) Nav routing across tabs was unaddressed: nav responses carry only an id, no path, so per-tab id counters could coincide and misroute. Corrected: a **single editor-scoped monotonic nav-id counter**, each request records its issuing tab, responses apply to the id-owning tab. Also named the dropped `read_only` (#195/#301) per-tab bit, pinned right-neighbor activation on close, added concurrent-live-buffer + nav-routing verification, and noted the open-set/state refactor likely needs 2 PRs. Reviewer verified: the daemon is genuinely multi-buffer-capable; `TabBar` reuse is real (`SessionView`); no new protocol; the `Root` confirm-dialog is available; Phase-10 dependency consistent.
- 2026-07-02: Spec created from `/loopkit:plan` (roadmap Phase 15). Grounded on `EditorView` (single-buffer: one `InputState` + per-file bookkeeping), the `SessionView` `TabBar` pattern, and the `workspace.rs` per-open-file daemon wiring (mtime/diagnostics/live-buffer) that must fan out. Constraint/precedent-determined: multi-buffer with one `InputState` per tab; reuse `gpui-component` `TabBar`; open-or-switch semantics; wiring fan-out with no new protocol; dirty-close confirm; tabs not persisted. One genuinely-open item carried to the gate: preview tabs vs persistent tabs.
