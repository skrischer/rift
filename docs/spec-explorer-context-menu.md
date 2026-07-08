# Spec: Explorer context menu

> Status: READY
> Created: 2026-07-08
> Completed: —

Add a right-click context menu over the explorer's entry rows, reusing
`gpui-component`'s `ContextMenu`/`PopupMenu` widget, that ships the artboard's
**State D** top group — the actions that already map to a real client
capability: **Open**, **Reveal in tree**, **Copy path**, **Copy relative path**,
**Reveal in terminal**, **Collapse all** — and nothing from the write-actions
group below it (New file / New folder / Rename / Cut·Move / Delete are Phase 30's
surface, absent here, not dead controls).

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. This realizes the **State D (Context menu)** column of the
"Explorer — Redesign" Paper artboard (file `rift`), whose **top group** is the
six client-capable actions; the **WRITE ACTIONS / FILE-OPS** group below the
separator is Phase 30 and is not shipped here.

- [ ] **Right-clicking any explorer entry row** opens a context menu anchored at
      the cursor, built with `gpui-component`'s `ContextMenu`/`PopupMenu`
      (reused, never forked — the same widget `editor.rs` already uses for its
      right-click menu). The menu lists **exactly** the artboard State D top
      group in order: **Open**, **Reveal in tree**, **Copy path**, **Copy
      relative path**, **Reveal in terminal**, **Collapse all**. The
      write-actions group (New file, New folder, Rename, Cut · Move, Delete) and
      its separator are **absent** — not rendered dimmed-and-dead — until
      Phase 30 docks them into this same menu.
- [ ] **Open** opens the right-clicked entry the same way `Enter` / a click
      already does — a file emits the existing `FileTreeEvent::OpenFile`, a
      directory toggles its expansion — by reusing the shipped `OpenSelected`
      action. (Row-scoped: right-clicking a row selects it first, so the menu's
      unit actions operate on the selected target, matching the existing
      keyboard-action architecture.)
- [ ] **Reveal in tree** selects the target row and scrolls it into view via the
      shipped `FileTree::reveal` path (expanding any collapsed ancestors) — the
      same reveal capability the header's Reveal-active button drives, surfaced
      on the row.
- [ ] **Copy path** writes the target's **absolute** path to the system
      clipboard; **Copy relative path** writes its **root-relative** path (the
      model key). Both via GPUI's `cx.write_to_clipboard(ClipboardItem::new_string(..))`.
- [ ] **Reveal in terminal** opens a fresh terminal rooted at the target's
      directory (a file's parent, a directory itself) by dispatching a
      **structural tmux command** (`new-window -c <absolute-dir>`) on the
      **existing** tmux command channel — the same channel the pane-header split
      / new-window controls already use. It **never** sends keystrokes into a
      running pane and **never** inspects a pane's process, so it does not
      disturb whatever agent is running and stays agent-agnostic.
- [ ] **Collapse all** collapses every directory, reusing the shipped
      `collapse_all` — the row-menu surface of the header/root-row collapse
      affordance.
- [ ] The menu is **pointer-invoked only** and adds **no keyboard binding**: it
      opens on a right mouse-down via the widget's own handler, and its actions
      are dispatched by the `PopupMenu` and handled by `on_action` on the tree
      root **within `FILE_TREE_KEY_CONTEXT`**. No action is bound to a key in
      `main.rs`, so the agent-first key scoping and terminal keystroke delivery
      are unchanged.
- [ ] The explorer stays **agent-agnostic** and reads only the existing client
      model — paths, kinds, `root()`; the menu items are **label-only** (no SVG
      icons — the product binary still does not embed `gpui-component`'s icon
      assets), themed by `gpui-component` (Catppuccin Mocha) with **no hardcoded
      hex**. **No new protocol message** is added (reveal-in-terminal reuses the
      shipped `ClientMessage::TmuxCommand`), and the daemon is untouched.

## Scope

### In scope

Client-side, chiefly in `crates/app/src/file_tree.rs` (the menu shell and the
five client-only actions), plus one small cross-crate wire for reveal-in-terminal
(`crates/app/src/workspace.rs` + a new public method on
`rift_terminal::SessionView`). The binding visual reference is the Paper
**"Explorer — Redesign"** artboard's **State D (Context menu)** column, top
group.

- **Row context menu (`file_tree.rs` `render_row`).** Wrap each entry row with
  `gpui-component`'s `ContextMenuExt::context_menu(..)`; the builder returns a
  `PopupMenu` carrying the six top-group items as `.menu(label, action)` entries.
  Right-clicking a row first **selects** it (a `on_mouse_down(MouseButton::Right)`
  listener sets `selected` + marks the cache dirty), so the menu's unit actions
  operate on `self.selected`. The menu is scoped to entry rows only (not the
  header, the `RIFT` root row, or the empty placeholder).
- **New actions + handlers (`file_tree.rs`).** Define the unit actions the
  top-group items dispatch — `RevealInTree`, `CopyAbsolutePath`,
  `CopyRelativePath`, `RevealInTerminal`, `CollapseAll` — namespaced `rift`,
  `no_json`, mirroring the shipped `SelectUp`/… actions; handle each with
  `on_action` on the tree root (inside `FILE_TREE_KEY_CONTEXT`). **Open reuses
  the shipped `OpenSelected`** action (no new action). **Collapse all** calls the
  shipped `collapse_all`; **Reveal in tree** calls the shipped `reveal`.
- **Clipboard actions (`file_tree.rs`).** `CopyRelativePath` writes `row.path`;
  `CopyAbsolutePath` writes the absolute path derived by joining `model.root()`
  with `row.path` (pure string math, trailing-slash-safe, unit-tested). Both use
  `cx.write_to_clipboard(ClipboardItem::new_string(..))`.
- **Reveal-in-terminal wire (`file_tree.rs` → `workspace.rs` → `SessionView`).**
  `file_tree.rs` gains a `FileTreeEvent::RevealInTerminalRequested { dir }`
  variant carrying the target's **absolute directory**; `workspace.rs`'s existing
  `file_tree` subscription routes it (mirroring how `RevealActiveRequested` is
  routed) to a new **public** `SessionView` method that enqueues
  `new-window -c <dir>` onto its existing `tmux_command_tx` — the same channel
  and `ClientMessage::TmuxCommand` path the shipped split / new-window controls
  use.

### Out of scope — each its own phase or already shipped

- **Write actions — Phase 30.** New file, New folder, Rename, Cut · Move, and
  Delete — the artboard State D **bottom group** (dimmed there for review
  legibility, tagged FILE-OPS PHASE) — need a daemon write path (new `protocol`
  messages, daemon `std::fs` handlers) that does not exist yet. Phase 29 ships
  **none** of them and **no separator/group placeholder** (a control with no
  capability is a dead control). Phase 30 docks the second group, with its
  separator, into this exact menu.
- **File-type / action icons in the menu — Phase 28.** The artboard's leading
  glyphs (open-in-new, reticle, copy, terminal, collapse) need
  `gpui-component`'s SVG icon assets, which the product binary does not embed
  (documented in `file_tree.rs`; the same gap Phase 27's header works around).
  Menu items are **label-only** here (as `editor.rs`'s context menu already is);
  Phase 28's icon-asset embedding adds the glyphs.
- **Search / filter / multi-select — Phase 31.** No multi-select-aware batch
  menu; the menu targets a single row.
- **The header / root-row / empty-area context menus.** The menu is scoped to
  entry rows; collapse-all already lives on the header and the `RIFT` chevron.
- **Decoration, rollup, reveal, keyboard navigation, the row cache, the
  empty-state split, collapse-all itself** — all shipped by Phase 11 / 25 / 27
  and reused unchanged. Phase 29 only adds the right-click surface that dispatches
  into them.
- **Protocol / daemon / explorer-crate changes.** Client + terminal wiring only;
  `crates/protocol`, `crates/daemon`, and `crates/explorer` are untouched.
- **Source-control panel, status bar, editor chrome, settings** — their own
  specs.

## Human prerequisites

None. Client + terminal-crate wiring only: no new dependency (the
`ContextMenu`/`PopupMenu` widget and the `ClipboardItem` API are already
vendored; the tmux command channel is already shipped end-to-end), no protocol
addition, no daemon change, no secrets or provisioning. The "Explorer — Redesign"
artboard's State D column is the visual reference; the Catppuccin-Mocha theme
tokens it pulls from are already vendored via `gpui-component`.

## Constraints

- **Reuse `gpui-component`'s `ContextMenu`/`PopupMenu`, never fork it**
  (constitution). This is the exact widget `crates/app/src/editor.rs` already
  uses — `use gpui_component::menu::PopupMenu;` +
  `.context_menu(|menu: PopupMenu, _, _| menu.menu("Go to Definition", Box::new(GoToDefinition))…)`
  — so the pattern (menu items dispatch `Box<dyn Action>` handled by `on_action`
  on an ancestor element) is already proven in this codebase. The widget opens
  itself on a **right mouse-down** over the row's hitbox; no keybinding is
  involved.
- **Pointer + a scoped context-key only — the agent-first key scoping is
  untouched.** The menu is opened by the pointer; the top-group actions are
  dispatched by the `PopupMenu` and handled by `on_action` handlers on the tree
  root, which lives inside `FILE_TREE_KEY_CONTEXT`. **No new key binding is
  registered in `main.rs`**, so no keystroke can be intercepted from the terminal
  panel (GPUI dispatches actions along the focused element's context chain, and
  the terminal panel is a focus-tracked sibling, not an ancestor, of the tree).
  **Open reuses the shipped `OpenSelected`**, which is already `Enter`-bound
  scoped to the tree context — no global binding is added or changed.
- **Row-scoped via selection, not data-carrying actions.** Right-clicking a row
  sets `self.selected` (via a `on_mouse_down(MouseButton::Right)` listener that
  runs before the menu's deferred build), so the unit menu actions read
  `self.selected` — identical to how every shipped keyboard action already
  targets the selection. This keeps the action set unit structs (no payload
  plumbing) and gives the expected UX (the right-clicked row highlights).
- **Reveal-in-terminal is agent-agnostic and injection-free.** It dispatches a
  **structural** tmux command (`new-window -c <absolute-dir>`) — opening a fresh
  shell rooted at the folder — on the **existing** `tmux_command_tx` /
  `ClientMessage::TmuxCommand` path that the shipped pane-split and new-window
  controls already use (`session_view.rs`). It **never** `send-keys` into an
  existing pane and **never** reads a pane's running process, so it neither
  disturbs a running agent nor detects one. **No new protocol message.**
- **Clipboard via the GPUI `App` API.** `cx.write_to_clipboard(ClipboardItem::new_string(text))`
  (present on the pinned gpui rev). The absolute path is `model.root()` joined
  with `row.path` (trailing-slash-safe; a top-level file's parent is the root
  itself); the relative path is `row.path` verbatim. Both derivations are pure
  string functions with unit coverage; when a row exists `root()` is always
  `Some` (rows arrive with the snapshot that sets the root), so the loading /
  empty placeholder — which shows no rows — never reaches the menu.
- **Menu items are label-only** (no `IconName`): the shipping `rift` binary does
  not embed `gpui-component`'s SVG icon assets (only the dev-only `gallery`
  binary enables them — documented in `file_tree.rs`), so an icon menu item would
  render blank. The artboard's leading glyphs land with Phase 28's icon
  embedding; `editor.rs`'s context menu is already label-only for the same
  reason.
- **Theme tokens only** (constitution): the `PopupMenu` is themed by
  `gpui-component` (Catppuccin Mocha); Phase 29 introduces **no hardcoded hex**.
- **No dead controls** (constitution, Phase-25/27 precedent): the write-actions
  group is **absent**, not dimmed-and-inert. Phase 30 adds the separator + the
  second group into this same menu when the daemon write path exists.
- **Crate boundaries are contracts** (constitution): the reveal-in-terminal
  method is a new **public** API on `rift_terminal::SessionView` (through
  `lib.rs`); `workspace.rs` — which already owns the `SessionView` entity and the
  `file_tree` subscription — is the only bridge (`SessionView` cannot reach back
  into `rift-app`). The `FileTreeEvent::RevealInTerminalRequested` variant is
  intra-`app`.
- **No `.unwrap()` in library code**; no `todo!()` in merged code; an action
  whose target the model does not carry is a no-op (matching the tree's "render /
  act only on what the model carries" discipline).
- **Headless-testable seams.** The absolute-path / relative-path derivation, the
  reveal-in-terminal directory + command-string builder, and the top-group action
  set (exactly six items, write actions absent) are pure functions / string math
  with unit coverage; the visual menu (anchoring, theming, dismissal) is validated
  at the milestone QA gate against the artboard's State D column.

## Prior art

Consulted the "Explorer overhaul — prior-art index (Phases 27–31)" in
`prior-art.md` (Phase 29 row), the shipped `editor.rs` context menu, and the
shipped Phase-11/25/27 tree.

- **Paper "Explorer — Redesign" artboard (file `rift`), State D (Context menu).**
  The binding visual contract: a single popover with a **top group** of six
  client-capable items (Open — with an `Enter` hint, Reveal in tree, Copy path,
  Copy relative path, Reveal in terminal, Collapse all), a **separator**, then a
  **WRITE ACTIONS** group tagged **FILE-OPS PHASE** (New file, New folder, Rename
  — `F2`, Cut · Move, Delete — `⌫`, drawn dimmed for review legibility). Phase 29
  ships the **top group only**; the separator + write group are Phase 30's.
- **`longbridge/gpui-component` `ContextMenu` / `PopupMenu`** (Apache-2.0,
  already vendored; prior-art index Phase 29 — **reuse**). The widget opens on
  right-click over the element's hitbox, focuses the menu, and dispatches each
  item's `Box<dyn Action>`; `crates/app/src/editor.rs` already ships this exact
  pattern (`.context_menu(|menu: PopupMenu| menu.menu("Go to Definition",
  Box::new(GoToDefinition))…)` handled by `on_action` on the editor div).
- **`zed` `crates/project_panel`** (GPL-3.0, study-only): the standard
  file-explorer right-click action taxonomy — split the **client-capable** group
  (open, reveal, copy path, open-in-terminal, collapse-all) from the **write**
  group (create/rename/delete/move); patterns, not code.
- **rift-local grounding.** `crates/app/src/file_tree.rs` (the shipped
  `OpenSelected` / `reveal` / `collapse_all` / `toggle_collapse_all` actions and
  methods, `FILE_TREE_KEY_CONTEXT`, and the `FileTreeEvent::RevealActiveRequested`
  precedent for routing an intra-crate event to `workspace.rs`);
  `crates/app/src/editor.rs` (the shipped `ContextMenuExt`/`PopupMenu` usage);
  `crates/terminal/src/session_view.rs` (`tmux_command_tx`, `split_command`, the
  `new-window` control — the existing structural-tmux-command channel);
  `crates/protocol/src/lib.rs` (`ClientMessage::TmuxCommand`, already shipped);
  `crates/app/src/worktree.rs` (`root()` absolute, `entries()`).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Phase 29 ships the artboard State D **top group only** (six client-capable actions); the write-actions group + its separator are Phase 30** | The write actions map to a daemon write path that does not exist yet; shipping them (even dimmed) is a dead control (constitution; Phase-25/27 precedent). The artboard documents them tagged FILE-OPS PHASE; Phase 30 docks them into this same menu. | 2026-07-08 |
| **Reuse `gpui-component`'s `ContextMenu`/`PopupMenu`; do not fork or hand-roll a menu** | Constitution (reuse widgets). The widget is vendored and already shipped in `editor.rs`; it opens on pointer right-click and dispatches `Box<dyn Action>` handled on an ancestor — the exact shape the tree needs. | 2026-07-08 |
| **Pointer-invoked menu; menu actions dispatched within `FILE_TREE_KEY_CONTEXT`, no new key binding** | The task's crux: the menu must not break the agent-first key scoping or terminal keystroke delivery. The widget opens on right mouse-down (pointer); the actions are `on_action` handlers on the tree root (scoped context), never bound to a key in `main.rs`, so nothing can be stolen from the terminal. | 2026-07-08 |
| **Right-click selects the row; menu uses unit actions over `self.selected`** | Matches the shipped keyboard-action architecture (every action targets the selection) and IDE UX (right-click highlights the row). Avoids data-carrying-action plumbing and keeps the new actions unit structs like `SelectUp`/`OpenSelected`. The select runs on `on_mouse_down(Right)`, before the widget's deferred menu build, so the target is set by dispatch time. | 2026-07-08 |
| **Open reuses the shipped `OpenSelected`; Collapse all reuses `collapse_all`; Reveal in tree reuses `reveal`** | These capabilities already exist and already operate on the selection; the menu is a new *surface* onto them, not new behavior. Reusing `OpenSelected` also lets the menu show its existing `Enter` binding hint (the artboard's `⏎`). | 2026-07-08 |
| **Copy path = absolute (`root()` ⋈ `row.path`); Copy relative path = `row.path`; via `cx.write_to_clipboard`** | The two artboard items are exactly absolute vs. root-relative. `row.path` is already the root-relative model key; the absolute path joins it onto the daemon-side `root()`. GPUI's clipboard API is a pure client capability — no daemon, no protocol. | 2026-07-08 |
| **Reveal in terminal = a structural `new-window -c <dir>` on the existing tmux command channel, never `send-keys` into a pane** | Agent-agnostic + injection-free: the active pane runs an agent, so cd-ing it would corrupt the agent's stdin. Opening a **new** shell rooted at the folder via the shipped `ClientMessage::TmuxCommand` path (the same one the split / new-window controls use) touches no existing pane and reads no process — no agent detection. No new protocol message. | 2026-07-08 |
| **Menu items are label-only (no SVG icons)** | The product binary does not embed `gpui-component`'s icon assets (documented in `file_tree.rs`); an icon menu item renders blank. `editor.rs`'s context menu is already label-only. Phase 28's icon embedding adds the artboard's leading glyphs. | 2026-07-08 |
| **Menu scoped to entry rows only** | The header and `RIFT` root row already carry collapse-all; the empty / loading placeholder has no rows. Keeping the menu on entry rows is the minimal, artboard-faithful surface. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Phase 29 — Explorer context menu (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] Right-clicking an explorer entry row opens a `gpui-component` context menu
      listing **exactly** Open, Reveal in tree, Copy path, Copy relative path,
      Reveal in terminal, Collapse all — in that order. A `grep` confirms **no**
      "New file" / "New folder" / "Rename" / "Cut" / "Delete" menu string exists
      (write actions absent, not dimmed). Asserted at the QA gate against the
      artboard's State D top group.
- [ ] **Open** on a file emits `FileTreeEvent::OpenFile` for the right-clicked
      row (reusing `OpenSelected`); on a directory it toggles expansion. **Reveal
      in tree** selects + scrolls the target into view via `reveal`. **Collapse
      all** collapses every `EntryKind::Dir` (reusing `collapse_all`). Asserted
      headlessly where the shipped action tests reach (right-click selects, then
      the action targets the selection).
- [ ] **Copy path** writes the absolute path and **Copy relative path** the
      root-relative path to the clipboard; the absolute/relative derivations are
      unit-tested (including a root of `"/"` and a top-level file whose parent is
      the root).
- [ ] **Reveal in terminal** emits `RevealInTerminalRequested` with the target's
      absolute directory (a file's parent, a directory itself) and the
      `SessionView` method enqueues `new-window -c <dir>` onto the existing tmux
      command channel; the command-string builder is unit-tested. A `grep`
      confirms no `send-keys` / keystroke injection is used for this action.
- [ ] The menu is **pointer-invoked**; a `grep` confirms `main.rs` registers **no
      new key binding** for the menu actions, and the actions are handled by
      `on_action` on the tree root within `FILE_TREE_KEY_CONTEXT`. Terminal
      keystroke delivery is unchanged.
- [ ] `grep` confirms **no new protocol message** (no change under
      `crates/protocol`, `crates/daemon`, `crates/explorer`), no hardcoded hex in
      the new code, no `IconName` menu item, and no agent detection.
- [ ] Milestone QA (dev channel): right-clicking a file and a directory shows the
      State D top group; Open / Reveal in tree / Copy path / Copy relative path /
      Collapse all behave as labeled; Reveal in terminal opens a new shell rooted
      at the folder **without disturbing the running agent pane**; the write
      actions are visibly absent (deferred to Phase 30).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The menu action targets the wrong row (selection race with the deferred menu build) | Right-click sets `self.selected` synchronously in the `on_mouse_down(Right)` bubble phase; the widget defers the menu build to the next frame and dispatches only on item click — the target is set well before. Mirrors the shipped `editor.rs` context menu; a headless test right-clicks row B while A is selected and asserts the action targets B. |
| Reveal in terminal disturbs or corrupts the running agent | It is a structural `new-window -c <dir>` on the tmux command channel — a fresh shell in a new window; it never `send-keys` into an existing pane and never reads a pane's process. Documented as a prior decision; the QA gate confirms the agent pane is untouched. |
| A menu key binding leaks into the global keymap and steals a terminal keystroke | No action is bound to a key in `main.rs`; the menu is pointer-invoked and its actions are `on_action` handlers scoped to the tree's `FILE_TREE_KEY_CONTEXT`. A `grep` gate asserts no new `bind` for these actions. Open reuses the already-scoped `Enter` binding. |
| Absolute-path derivation breaks at the filesystem root (`root()` == `"/"`) or a top-level file | The join trims a trailing slash and treats a top-level entry's parent as the root itself; both cases are unit-tested. Rows only exist when `root()` is `Some`, so the menu never runs against a `None` root. |
| Two issues both touch `file_tree.rs` → rebase churn | Issue 2 (reveal-in-terminal) depends on and is sequenced after Issue 1 (the menu shell), and its `file_tree.rs` slice is a single event-emit + menu item; the substance is in `rift-terminal` + `workspace.rs`. Disjoint seams, no concurrent churn. |
| The `PopupMenu` steals focus while open and the tree/terminal loses key handling | The menu is transient — it takes focus while open and returns it on dismiss (`DismissEvent`), introducing no persistent keymap or focus change. The widget's shipped use in `editor.rs` proves the dispatch reaches the ancestor `on_action` handlers. |
| Cloning the artboard too literally ships the dimmed write-actions group as dead UI | Prior decision + Outcome make the write group **absent**; the menu ships exactly six items. A `grep` for the omitted labels and the QA gate confirm nothing dead was added. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 29 — Explorer
  context menu). Grounded on the shipped `file_tree.rs` (the `OpenSelected` /
  `reveal` / `collapse_all` actions, `FILE_TREE_KEY_CONTEXT`, and the
  `FileTreeEvent::RevealActiveRequested` routing precedent), the shipped
  `editor.rs` `ContextMenuExt`/`PopupMenu` usage, `session_view.rs`'s existing
  `tmux_command_tx` / `new-window` channel, and the confirmed shipped
  `ClientMessage::TmuxCommand`. Visual contract is the "Explorer — Redesign"
  artboard's **State D (Context menu)** column, **top group** — the six
  client-capable actions (Open, Reveal in tree, Copy path, Copy relative path,
  Reveal in terminal, Collapse all). The write-actions group (New file / New
  folder / Rename / Cut · Move / Delete), tagged FILE-OPS PHASE in the artboard,
  is **Phase 30** and ships as **absent, not dimmed** — Phase 30 docks it, with
  its separator, into this same menu. Genuinely-open detail settled at authoring:
  reveal-in-terminal has **no existing public path**, so it is specified as a new
  `SessionView` method enqueuing a structural `new-window -c <dir>` on the
  existing tmux command channel — agent-agnostic, injection-free, no new protocol.
  Scope held to client rendering + one intra-crate event routed to a thin new
  `rift-terminal` public method; no protocol / daemon / dependency change.
