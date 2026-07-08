# Spec: Explorer design parity

> Status: READY
> Created: 2026-07-08
> Completed: —

Bring the file explorer to full parity with the Paper "Cockpit — IDE" artboard:
an in-panel header band with a live action row, the git-status **letter lane**
right-aligned across rows, the diagnostic dot moved into the trailing decoration
cluster with rollup shown only on collapsed directories, and distinct
loading / empty states — closing the remaining visual gap on the explorer that
Phase 11 (`spec-explorer-panel.md`, shipped) left after it delivered decoration,
reveal, and keyboard navigation.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not
activities. This is a **parity** pass: it refines the explorer chrome and row
anatomy the Phase-11 tree already renders; it does not re-spec decoration,
reveal, keyboard nav, the row cache, or the `#309` ignored-files behavior, all
of which already ship.

- [ ] The explorer renders the design's **header band** above the tree: an
      uppercase, letter-spaced `EXPLORER` label (muted-foreground token) on a
      subtle elevated surface with a bottom hairline, plus a right-aligned
      **action row**. The action row ships exactly the two actions that map to a
      real client capability — **Collapse all / Expand all** (a toggle) and
      **Reveal active file** — and consciously omits the design's *new file*
      and *refresh* glyphs (no dead controls; see Prior decisions).
- [ ] A **workspace-root row** (`RIFT` in the design) renders below the header:
      the leaf of `WorktreeModel::root()`, uppercased, with a disclosure chevron
      that mirrors and drives the collapse-all/expand-all state.
- [ ] Each row's **git status letter** renders in a dedicated **right-aligned,
      fixed-width lane** at the panel's trailing edge, so the letters align into
      a column across every row (design: `app · M`, `Cargo.toml · M` share one
      right margin) — replacing today's inline-after-the-name badge.
- [ ] The **diagnostic severity dot** renders in the trailing cluster,
      immediately left of the git letter (design: `session_view.rs • M`) —
      moved off its current position left of the name — so a row's decoration
      reads as one right-aligned unit.
- [ ] Rolled-up decoration (git letter + diagnostic dot) shows on a directory
      row **only when that directory is collapsed**; an **expanded** directory
      shows no rolled-up badge or dot, because its visible descendants carry
      their own (design: expanded `crates` / `terminal` / `src` are undecorated
      while collapsed `app` shows its rolled-up `M`). Files, and collapsed
      directories, decorate exactly as today. The `compute_rollup` pass is
      unchanged — this is a render-time suppression, not a data change.
- [ ] The selected row matches the design's treatment: a left **accent bar** and
      an emphasized (bold) name on the existing active-row background.
- [ ] The empty panel distinguishes two states, each a quiet, centered,
      muted-foreground placeholder with no action surface: **loading** (no
      snapshot yet — `root()` is `None`) vs **empty root** (a snapshot arrived
      but the tree is empty — `root()` is `Some` and `is_empty()`), replacing
      today's single "No files" text that conflates them.
- [ ] The explorer stays **agent-agnostic** and reads only the existing client
      model — paths, kinds, git status, diagnostics, `ignored`, and `root()`;
      **no new protocol message** is added and the daemon is untouched.

## Scope

### In scope

All changes are client-side in `crates/app` — chiefly `file_tree.rs`'s
`render()` / `render_row()`, with one small `workspace.rs` wiring for the
reveal-active action. The visual contract is the Paper **"Cockpit — IDE"**
artboard's Explorer panel.

- **Header band + action row** (`file_tree.rs` `render()`): a header element
  above the `v_virtual_list`, carrying the `EXPLORER` label and a trailing
  action row. Two live actions:
  - **Collapse all / Expand all** — a pure-client toggle over the existing
    `collapsed` set: collapse inserts every `EntryKind::Dir` path from
    `model.entries()`; expand clears the set. The action reflects its current
    state (all-collapsed → offers Expand).
  - **Reveal active file** — re-reveals the active editor file. `file_tree.rs`
    gains a `FileTreeEvent::RevealActiveRequested`; `workspace.rs`'s existing
    `file_tree` subscription handles it by calling the already-present
    `reveal_open_file_in_tree(cx)` (which owns the active-file path). No new
    protocol, no new cross-crate coupling.
- **Workspace-root row** (`file_tree.rs` `render()`): a synthetic top row
  showing `root()`'s leaf uppercased with a disclosure chevron bound to the
  collapse-all/expand-all state. Absent (or a neutral placeholder) while
  `root()` is `None`.
- **Right-aligned git letter lane** (`file_tree.rs` `render_row()`): move the
  git-status letter out of the inline `.children(git_badge)` slot into a
  fixed-width, `flex_shrink_0`, right-aligned trailing lane so letters column-
  align across rows. Colors keep the shipped mapping (conflicted → danger,
  changed → warning, untracked → success).
- **Trailing diagnostic dot** (`file_tree.rs` `render_row()`): render the
  severity dot in the trailing cluster immediately left of the git lane, not in
  its current position left of the name. Severity → color mapping unchanged.
- **Collapsed-only rollup rendering** (`file_tree.rs` `render_row()`): gate the
  rolled-up git letter/dot on a directory row behind
  `self.collapsed.contains(path)`; render nothing rolled-up for an expanded
  directory. Files always show their own decoration.
- **Selection accent** (`file_tree.rs` `render_row()`): a left accent bar and
  bold name on the selected row, on top of the existing active background.
- **Loading vs empty states** (`file_tree.rs` `render()`): branch the
  empty-content path on `model.root()` — `None` → a loading placeholder,
  `Some` + `is_empty()` → an empty-root placeholder. Both centered, muted, and
  passive.

### Out of scope

- **File / folder icons.** The design uses folder and file-type glyph icons; the
  tree deliberately renders text-glyph disclosure markers because the product
  binary does not embed `gpui-component`'s SVG icon assets (documented in
  `file_tree.rs`). Icon-asset embedding is a separate concern, not one of the
  named parity items, and is not opened here.
- **File operations (create / rename / delete / move).** The design's *new file*
  header glyph maps to a write capability deferred at Phase-11 planning to a
  dedicated daemon-file-ops phase (new protocol variants + daemon handlers).
  Omitted here as a dead control until that phase exists.
- **A refresh / re-scan action.** The tree is push-reactive — the daemon streams
  every filesystem, git, and diagnostic change — so a manual re-scan is
  redundant and would need a new protocol request. Omitted; a stream-recovery
  affordance, if ever wanted, belongs to the connection-robustness surface
  (Phase 20), not the explorer.
- **Decoration, roll-up computation, reveal, keyboard navigation, the row
  cache, and the `#309` ignored-files behavior** — all shipped by Phase 11 and
  unchanged here (`compute_rollup`, `reveal`, the action set, `refresh_row_cache`
  stay as-is; this pass only changes how their results are *arranged and gated*
  in `render_row`).
- **Protocol / daemon / explorer-crate changes.** Purely client rendering plus
  one intra-crate event wire.
- **Source-control panel, status bar, and editor chrome** — their own specs
  (`spec-source-control*.md`, `spec-editor-chrome.md`).

## Human prerequisites

None. Client-side rendering parity only: no new dependency, no protocol
addition, no daemon change, no secrets or provisioning. The Paper "Cockpit —
IDE" artboard is the visual reference; theme tokens are already vendored.

## Constraints

- **Reads the existing model, no new protocol** (constitution: `protocol` is a
  deliberate API surface). Every value the parity pass needs — paths, kinds,
  git status, diagnostics, `ignored`, and `root()` — is already on
  `WorktreeModel`. The one new type is an intra-crate `FileTreeEvent` variant
  (`RevealActiveRequested`), not a wire message.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`); never hardcoded
  hex. The git/severity/hover/active tokens the tree already uses
  (`danger`, `warning`, `success`, `info`, `muted_foreground`, `list_hover`,
  `list_active`, `foreground`) carry over; the header band's elevated surface,
  hairline, and the selection accent bar reuse existing surface/border/accent
  role tokens — no new palette values.
- **Agent-agnostic** (constitution): decoration and chrome derive only from
  filesystem / git / LSP / model signals; no agent detection, no output parsing.
  The header actions touch only view-local state (`collapsed`) and the existing
  reveal path.
- **Rollup suppression is render-time, not a data change**: `compute_rollup`
  still rolls status onto every ancestor (a collapsed directory needs it); only
  `render_row` decides not to *draw* it for an expanded directory. This keeps
  the "cache is derived state, never mutated independently" invariant intact and
  means a collapse toggle (which already marks the cache dirty) flips the
  decoration correctly on the next render.
- **The in-panel header band is the design's panel header**, distinct from the
  dock-tab identity `Panel::title` returns ("Explorer"); adding the band does
  not remove or duplicate the dock chrome — the band is the explorer's own
  header the way the "Cockpit — IDE" left dock shows it.
- **Collapse-all operates over `EntryKind::Dir` entries** from the model, not a
  hardcoded depth; expand clears the `collapsed` set. Both mark the row cache
  dirty through the existing `toggle_dir`/cache-invalidation discipline so the
  visible-row set and the root-row chevron stay consistent.
- **Keyboard focus and the agent-first key scoping are unchanged**: the header
  actions are pointer targets; they add no key bindings and do not touch the
  `FILE_TREE_KEY_CONTEXT` action set, so terminal keystroke delivery is
  untouched.
- **No `.unwrap()` in library code**; no `todo!()` in merged code; a decoration
  or state the model does not carry is simply not rendered (no placeholder
  glyph), matching the tree's existing "render only what the model carries"
  discipline.
- **Headless-testable**: the collapse-all/expand-all set operation, the
  root-leaf derivation, the collapsed-only rollup-suppression predicate, and the
  loading-vs-empty state selection are pure functions / model reads with unit
  coverage; the visual arrangement (lane alignment, accent bar, band) is
  validated at the milestone QA gate.

## Prior art

Consulted `prior-art.md` (v1.0 polish index, Phases 19–26) and the shipped
Phase-11 tree.

- **Paper "Cockpit — IDE" artboard, Explorer panel** — the binding visual
  contract: the `EXPLORER` header band with a three-glyph action row, the `RIFT`
  workspace-root row with a chevron, the right-aligned git-letter column, the
  trailing `name • M` diagnostic-dot-plus-letter cluster, rollup shown on the
  collapsed `app` but not the expanded `crates`/`terminal`/`src`, and the
  selected-row accent bar.
- **`zed` `crates/project_panel`** (GPL-3.0, study-only): the collapse-all /
  expand-all action and the trailing decoration column are standard IDE-explorer
  anatomy mirrored here (patterns, not code) — the same reference Phase 11 used.
- **rift-local grounding**: `crates/app/src/file_tree.rs` (the shipped tree —
  inline git badge, dot left of the name, unconditional ancestor rollup, single
  "No files" empty branch, no header) and `crates/app/src/worktree.rs`
  (`root()` distinguishes not-yet-loaded from empty; `entries()` enumerates dirs
  for collapse-all). `workspace.rs` already owns `reveal_open_file_in_tree`.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Header action row ships Collapse-all/Expand-all + Reveal-active only; *new file* and *refresh* are omitted** | No dead controls (constitution; the `spec-editor-chrome.md` precedent omitted LSP Rename for the same reason). *New file* needs the deferred daemon-file-ops write capability; *refresh* is redundant under the push-reactive stream and would need a new protocol request. Both map to no current client capability, so they are consciously left out until their owning surface exists. | 2026-07-08 |
| **Reveal-active is an intra-crate `FileTreeEvent`, handled by `workspace.rs`'s existing reveal path** | `workspace.rs` already owns the active-file path and `reveal_open_file_in_tree`; the panel button re-triggers it via a new event variant. No new protocol, no new coupling — the minimal wire for a real capability. | 2026-07-08 |
| **Git letter moves to a right-aligned fixed-width lane; the diagnostic dot moves into the trailing cluster left of it** | Design parity: the artboard aligns git letters into a right-edge column and reads decoration as one trailing unit (`name • M`). Today's inline-after-name badge and left-of-name dot do not column-align and read as scattered. | 2026-07-08 |
| **Rolled-up decoration renders on a directory only when it is collapsed; expanded directories show no rollup** | Design parity: the artboard decorates the collapsed `app` but leaves the expanded `crates`/`terminal`/`src` undecorated even though they contain changed files — an expanded directory's descendants carry their own decoration, so an ancestor badge is redundant noise. Implemented as a render-time gate on `self.collapsed`, leaving `compute_rollup` (still needed when collapsed) untouched. | 2026-07-08 |
| **Workspace-root row with a chevron bound to collapse-all/expand-all** | Design parity: the `RIFT` band is the design's root affordance and the visual home of the collapse toggle; its chevron mirrors the collapse-all state. Uses `root()`'s leaf; neutral while `root()` is `None`. | 2026-07-08 |
| **Loading vs empty split on `root()`** | `WorktreeModel::root()` is `None` until the first snapshot completes and `Some` afterward — the exact, already-modeled signal to separate "connecting / no snapshot yet" from "connected, empty root". Today's single "No files" branch conflates them, which reads as an error during normal startup. | 2026-07-08 |
| **Icons stay text glyphs; no SVG asset embedding** | The product binary does not embed `gpui-component`'s icon assets (documented in `file_tree.rs`); icon fidelity is a separate concern and is not one of the named parity items. Text-glyph disclosure markers stay. | 2026-07-08 |
| **Client-only, no daemon / protocol change** | Every needed value is already streamed and folded onto the client model; this is pure rendering parity plus one intra-crate event — matching Phase 11's "reads the existing model" ethos. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
step under the Phase 250 milestone. Created once this spec is `READY` and merged
to `develop`.

- Milestone: created at `READY` (Phase 250 — Explorer design parity)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; `app-check` compiles the app.
- [ ] The explorer renders a header band (`EXPLORER` label + action row) above
      the tree and a workspace-root row from `root()`; the dock-tab title is
      unaffected.
- [ ] **Collapse-all** collapses every directory (top-level entries remain,
      nested subtrees hidden) and toggles to **Expand-all** (clears `collapsed`);
      the root-row chevron reflects the state. Asserted headlessly over
      `collapsed` / `visible_rows`.
- [ ] **Reveal active file** from the header re-reveals the active editor file
      (expands ancestors, selects, scrolls into view) via the workspace reveal
      path; a no-op when no file is open.
- [ ] Git-status letters render in a right-aligned fixed-width lane that
      column-aligns across rows; the diagnostic dot renders immediately left of
      the letter in the trailing cluster.
- [ ] An **expanded** directory over a changed/errored descendant shows **no**
      rolled-up letter or dot; **collapsing** it surfaces the rolled-up
      decoration; a file always shows its own. Asserted headlessly (a `Dir` row's
      rendered decoration is gated on `collapsed`), with the seeded rollup model
      from the Phase-11 tests reused.
- [ ] The selected row shows a left accent bar and a bold name.
- [ ] With no snapshot yet (`root()` `None`) the panel shows the **loading**
      placeholder; after a snapshot of an **empty** root (`root()` `Some`,
      `is_empty()`) it shows the **empty-root** placeholder; a populated root
      shows the tree. Asserted headlessly via the model accessors.
- [ ] `grep` confirms no agent detection introduced and no new protocol message
      added (no change under `crates/protocol`, `crates/daemon`,
      `crates/explorer`).
- [ ] Milestone QA (dev channel): the explorer reads like the "Cockpit — IDE"
      artboard — header band and actions present and live, git letters aligned in
      their lane, the diagnostic dot in the trailing cluster, expanded
      directories clean while collapsed ones carry their rollup, selection accent
      legible, and loading vs empty states distinct.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Rollup suppression on expanded directories accidentally hides a *file's* own decoration, or fails to reappear on collapse | Gate strictly on `row.kind == Dir && !self.collapsed.contains(path)`; files are never suppressed. A collapse toggle already marks the cache dirty, so the next render re-evaluates; a headless test collapses/expands a decorated ancestor and asserts the decoration appears only when collapsed. |
| The header band duplicates or fights the dock-tab title | The band is the panel's own header (design), `Panel::title` stays the dock identity; they are different surfaces. Noted as a constraint; QA confirms no visual redundancy. |
| Reveal-active wiring couples the panel to the workspace | Reuse the existing `reveal_open_file_in_tree` path via a single new `FileTreeEvent` variant handled by the already-present subscription — no new protocol, no new field ownership; mirrors how `OpenFile` is already routed. |
| Three issues all edit `file_tree.rs` `render()` / `render_row()` → rebase churn | The header/root row (issue 1) restructures `render()`'s root layout; the row decoration (issue 2) is confined to `render_row()` (disjoint); the state split (issue 3) edits the empty branch inside `render()` and so depends on issue 1. Sequence per the DAG; the seams are otherwise disjoint. |
| Collapse-all over a very large tree | Inserting every `Dir` path is O(dirs) once per click over the already-enumerated `entries()`; the row cache rebuild it triggers is the same bounded pass the tree already runs on any collapse. No per-frame cost. |
| Loading placeholder shows for a non-repo / genuinely rootless host and reads as a hang | The split keys on `root()`, which the daemon sets on the first completed snapshot for any root (repo or not); an empty non-repo root reaches `Some` + `is_empty()` and shows the empty-root state, not the loading one. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 25 — Explorer
  design parity). Grounded on the shipped Phase-11 tree (`file_tree.rs`:
  inline git badge, dot left of the name, unconditional ancestor rollup, single
  "No files" branch, no header) and the Paper "Cockpit — IDE" artboard. Parity
  gaps confirmed against the design: missing header band + action row, missing
  workspace-root row, git letter not column-aligned, diagnostic dot on the wrong
  side of the name, rollup drawn on expanded ancestors the design leaves clean,
  and a loading/empty conflation `root()` already lets us split. Scope held to
  client rendering + one intra-crate reveal event: no protocol, daemon, or
  explorer-crate change. Genuinely-open decisions settled at authoring: *new
  file* and *refresh* header glyphs omitted as dead controls (file-ops deferred;
  reactive stream makes refresh redundant), leaving Collapse-all/Expand-all and
  Reveal-active as the live actions; icon-asset embedding scoped out.
