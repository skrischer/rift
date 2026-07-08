# Spec: Explorer search & filter

> Status: READY
> Created: 2026-07-08
> Completed: —

Realize the "Explorer — Redesign" artboard's **State B (Search)** column: an
in-panel fuzzy-narrowing filter bar that lands in the header→tree seam Phase 27
reserved, a summoned jump-to-file quick-open, and the discrete multi-select
treatment the Phase-27 baseline documented but did not implement — all
client-side over the already-streamed worktree model, with one named new
dependency (`nucleo-matcher`).

## Outcome

What is true when this work is done. Observable, end-to-end criteria — not
activities. This is the **search / filter / multi-select** phase of the explorer
overhaul (27–31): it makes the header's search/filter toggle (**ABSENT** in
Phase 27) live, fills the render() seam Phase 27 kept between the header band and
the tree, and reaches the artboard's **State B** narrowing + match emphasis plus
the discrete multi-select fill. It builds **on** Phase 27's shipped A-Default
baseline and does not re-spec the header band, row anatomy, density, decoration,
rollup, reveal, or single-selection accent bar it already ships.

- [ ] Toggling the header's **search/filter** control (the fixed-width action
      slot Phase 27 reserved, now live) reveals a fuzzy **filter bar** in the
      render() seam between the header band and the workspace-root row; toggling
      it off (or clearing + `Esc`) restores the full tree unchanged. Phase 27's
      header→tree stack is filled, not re-laid-out.
- [ ] Typing in the filter **narrows the visible tree** to files whose
      root-relative path fuzzy-matches the query (ranked by `nucleo-matcher`),
      keeping each match's **ancestor directories** so the hierarchy still reads,
      and force-expanding those ancestors regardless of their collapse state so
      no match hides. An empty query is identical to no filter (the full tree,
      honoring collapse state).
- [ ] Each narrowed row **emphasizes the matched substring** — the artboard's
      State B highlight chip, rendered via a theme token (not a hardcoded hex) on
      exactly the matched characters `nucleo-matcher` reports; unmatched
      characters keep the row's normal foreground. Directory rows (ancestors of a
      match) render without emphasis.
- [ ] A summoned **jump-to-file quick-open** lists every file in the streamed
      worktree ranked by the same fuzzy match, over a `Root` dialog overlay (the
      command-palette modal pattern already wired into `WorkspaceView`);
      confirming a row **opens and reveals** it via the existing
      `FileTreeEvent::OpenFile` + `reveal` path — no new open path, no new
      protocol. Its summon shortcut is terminal-safe (see Constraints).
- [ ] The tree supports **discrete multi-select**: `Ctrl/Cmd+Click` toggles a
      path in/out of a selection set, `Shift+Click` selects a contiguous range,
      and `Shift+Up`/`Shift+Down` extend the set from the keyboard. Multi-selected
      rows render the artboard's **discrete flat-surface fill (no accent bar)**;
      the single active/cursor row keeps Phase 27's inset accent bar. `Enter`
      (or activating the selection) opens **every** selected file as an editor
      tab through the existing `OpenFile` path — the live, self-contained
      consumer that keeps the multi-selected state reachable, not dead UI.
- [ ] **Keyboard-first and agent-first**: the filter toggle, filter navigation,
      and multi-select extension keys stay scoped to `FILE_TREE_KEY_CONTEXT` and
      never intercept a keystroke bound for a focused terminal pane; quick-open's
      global summon uses a terminal-safe `Ctrl/Cmd+Shift` chord (mirroring the
      command palette's `Ctrl+Shift+P` precedent). No terminal keystroke delivery
      regresses.
- [ ] The explorer stays **agent-agnostic** and reads only the existing client
      model — paths and kinds from `WorktreeModel::entries()`. **No new protocol
      message, no daemon change, no `explorer`-crate change**; the only new
      dependency is `nucleo-matcher` (MPL-2.0, already allow-listed by
      `cargo deny`). The change is confined to `crates/app`.

## Scope

### In scope

Client-side only in `crates/app`. The binding visual reference is the Paper
**"Explorer — Redesign"** artboard's **State B (Search)** column (file `rift`),
plus its ANATOMY & TOKENS legend for the match-emphasis and multi-select-fill
tokens.

- **Fuzzy match substrate** (`crates/app`, a small pure module): a headless
  function that, given a query and a candidate path, returns whether it matches
  and — for a match — a rank score and the **matched character indices**
  (`nucleo-matcher`'s `Pattern`/`Matcher` `indices` API). Both the filter bar and
  quick-open consume it. Tested with valid and malformed input (empty query,
  non-ASCII, out-of-order, no-match), per the parser-testing convention.
- **In-panel filter bar** (`file_tree.rs` `render()` seam + `render_header`):
  the Phase-27-reserved search/filter action slot becomes a live toggle over a
  view-local `filter_query`/`filter_active` state; when active, a
  `gpui-component` `input` (`InputState` + `Input`, the widget
  `connection_screen.rs`/`editor.rs` already use) renders in the header→tree seam.
  `Esc` clears + closes; a non-matching query yields an empty tree body (a quiet
  "No matches" placeholder), not an error.
- **Filter narrowing** (`file_tree.rs` `visible_rows`/`row_cache`): when a query
  is active, the visible-row derivation matches **files** over the full
  `entries()` set, then adds each match's ancestor directories and force-expands
  them (ignoring `collapsed`) so matches are always shown; rows carry the matched
  indices for emphasis. When no query is active the derivation is exactly today's
  collapse-aware pass — unchanged. The cache-dirty discipline extends to
  filter-query changes (a query edit marks the row cache dirty, same seam as a
  collapse).
- **Match emphasis** (`file_tree.rs` `render_row`): render the matched substring
  characters with the artboard's State B highlight token; unmatched characters
  keep the normal foreground. Reuses the existing name element; adds only a
  span-splitting on the carried match indices. Theme token only.
- **Jump-to-file quick-open** (`crates/app`, a new module modeled on
  `command_palette.rs`): a `Root` dialog overlay hosting a `gpui-component`
  `list` + `input`, its rows the streamed worktree's files ranked by the fuzzy
  substrate; arrows navigate, `Enter` opens+reveals the selection via the
  existing `FileTreeEvent::OpenFile`/`reveal` path, `Esc` dismisses. Summoned by a
  new global `#[action(namespace = rift)]` bound in `main.rs` to a terminal-safe
  `Ctrl/Cmd+Shift` chord and hosted in `WorkspaceView` beside the command palette.
- **Discrete multi-select** (`file_tree.rs` selection state + `render_row` +
  keyboard actions): a `selection: HashSet<String>` (the multi-set) alongside the
  existing single `selected` cursor; `Ctrl/Cmd+Click` toggles a path,
  `Shift+Click` ranges from the cursor, `Shift+Up`/`Shift+Down` extend. Multi-set
  rows render the discrete flat fill (theme token, no accent bar); the cursor row
  keeps Phase 27's accent bar. Activating a multi-selection emits `OpenFile` per
  file (multi-open into tabs). Two new scoped actions (`ExtendSelectionUp`,
  `ExtendSelectionDown`) bound in `main.rs` under `FILE_TREE_KEY_CONTEXT`.

### Out of scope — deferred or owned elsewhere

- **A daemon-side project file index (jwalk) for quick-open.** v1 quick-open
  narrows the **already-streamed tree** (`WorktreeModel::entries()` already
  mirrors the whole daemon worktree), so no `protocol` message, daemon handler,
  or `jwalk` dependency is added. A daemon-side index is a future refinement only
  if the streamed tree proves insufficient (e.g. to reach `.gitignore`d paths the
  daemon does not stream today, #309) — explicitly not this phase. (Prior
  decisions.)
- **File operations on the multi-selection (delete / move / rename).** Bulk write
  actions are Phase 30's daemon-file-ops surface (new `protocol` messages + daemon
  `std::fs`). Phase 31 ships multi-select with the **open-many** consumer only;
  when the context menu (Phase 29) and file ops (Phase 30) land in the overhaul
  sequence, they act on this same selection set — Phase 31 neither re-specs nor
  hard-depends on them.
- **File-type icons in the filter/quick-open rows (Phase 28), the tree context
  menu (Phase 29), and inline rename (Phase 30).** The filter and quick-open rows
  reuse the same text-glyph markers Phase 27 ships; icons drop in with Phase 28.
- **Phase 27's shipped baseline** — the header band, row anatomy/density, the
  reserved icon slot, trailing decoration cluster + git lane, rollup, reveal,
  single-selection accent bar, loading/empty split, and the existing arrow-key
  navigation — is **unchanged**; this phase only adds the filter bar into the
  reserved seam, the match emphasis and multi-set fill into `render_row`, the
  filter branch into `visible_rows`, and the quick-open module. The
  `compute_rollup`/`refresh_row_cache` derivation and the `OpenFile`/reveal wire
  are reused as-is.
- **Protocol / daemon / explorer-crate changes.** Purely client rendering +
  interaction over the existing model; the only new dependency is
  `nucleo-matcher`.
- **Frecency / recently-opened ordering, search history, regex/glob modes, and
  content (grep) search.** Out of the fuzzy-name-narrowing v1; later refinements
  if wanted. The source-control panel, status bar, and editor chrome are their
  own specs.

## Human prerequisites

None. Client-side only. The one new dependency, `nucleo-matcher`
(`helix-editor/nucleo`, MPL-2.0), is named in this spec, so its install is
autonomous under the loopkit grant, and MPL-2.0 is already allow-listed in
`deny.toml` so `cargo deny check licenses` passes with no config change. The
"Explorer — Redesign" artboard is authored and is the visual reference; its
Catppuccin-Mocha tokens are already vendored via `gpui-component`.

## Constraints

- **New dependency named and license-clean.** `nucleo-matcher` (from
  `helix-editor/nucleo`, MPL-2.0) is the fuzzy matcher; MPL-2.0 is already in
  `deny.toml`'s `allow` list, so `cargo deny check licenses` stays green. The
  **low-level synchronous** `nucleo-matcher` (`Matcher` + `Pattern` with
  `indices`) is used, not the threaded `nucleo` driver: matching runs over the
  already-in-memory streamed tree, so no background worker, injector, or extra
  threads are introduced. No other dependency is added.
- **Reads the existing model, no new protocol** (constitution: `protocol` is a
  deliberate API surface). Both surfaces match over `WorktreeModel::entries()`,
  which already mirrors the whole daemon worktree; nothing new crosses the wire,
  and the daemon and `explorer` crate are untouched.
- **Theme tokens only** (Catppuccin Mocha via `gpui-component`), never hardcoded
  hex. The State B match-emphasis highlight and the discrete multi-select fill
  each map to an existing theme role (a highlight/accent-tint role for the
  matched substring; a neutral list/selection surface for the flat multi-set
  fill, distinct from the single-selection `list_active` + `accent` bar). Layout
  dimensions (the filter-bar height, input padding) are plain layout pixels, as
  the shipped tree already uses.
- **Keyboard scoping must not break agent-first key delivery** (constitution;
  Phase 25/27 precedent). The filter toggle, `Esc`, and the multi-select
  extension keys (`Shift+Up`/`Shift+Down`) are bound under
  `FILE_TREE_KEY_CONTEXT`, so GPUI dispatches them only along the focused tree's
  context chain — the terminal panel is a focus-tracked sibling, not an ancestor,
  and never loses a keystroke. Quick-open's summon is a **global** binding like
  `OpenCommandPalette`, but on a `Ctrl/Cmd+Shift` chord the terminal does not
  consume (the same reason `Ctrl+Shift+P` is safe); a bare `Ctrl+P` (claimed by
  terminal readline) is **not** used. While the filter `input` is focused,
  character keys go to the input; the tree's arrow/`Enter` navigation still fires
  via its scoped actions (the input does not bind them), so typing and navigating
  coexist without leaking to the terminal.
- **Filter narrowing is a render-time derivation, not a model mutation.** The
  active query reshapes `visible_rows`' output (match set + forced ancestor
  expansion) but never touches `entries()`, `collapsed`, `compute_rollup`, or the
  git/diagnostic maps; clearing the query returns the exact prior tree. A query
  change marks `cache_dirty` through the same discipline `toggle_dir` uses, so the
  derived row cache can never drift. Force-expansion is scoped to the filtered
  pass — the user's real `collapsed` set is preserved and reinstated on clear.
- **Multi-select is additive to single-selection, not a replacement.** The
  existing `selected` cursor and its accent-bar treatment, arrow navigation, and
  `OpenFile`-on-click stay; the multi-set is a parallel `HashSet` the new
  modifier-clicks and extension keys populate, pruned against the visible set the
  same lazy way `selected` is. A plain click still sets the single cursor and
  clears the multi-set (standard tree behavior).
- **Reuse `gpui-component` widgets, never fork them** (constitution): the filter
  bar is `gpui-component`'s `Input`; quick-open composes its public `list` +
  `input` over the `Root` overlay exactly as `command_palette.rs` does (the
  overlay layers are already wired into `WorkspaceView`, so no new overlay
  plumbing is needed, unlike Phase 16). No widget is forked.
- **Agent-agnostic** (constitution): matching and selection derive only from file
  paths/kinds in the model — no agent detection, no output parsing. Quick-open and
  the filter touch only view-local state plus the existing reveal/open path.
- **No `.unwrap()` in library code**; no `todo!()` in merged code; an empty query
  lists the full tree / all files, a no-match query shows a quiet placeholder (not
  an error), and a path the model does not carry is simply not matched.
- **Headless-testable seams.** The fuzzy match (match/no-match, ordering, match
  indices), the filter-narrowing derivation (match set + ancestor inclusion +
  forced expansion + restore-on-clear), the multi-select set operations (toggle,
  range, keyboard extend, prune, open-many target list), and the quick-open
  candidate/ranking list are pure functions / model reads with unit coverage; the
  visual arrangement (highlight token, flat multi-fill, filter-bar placement) is
  validated at the milestone QA gate against the artboard's State B column.

## Prior art

Consulted the "Explorer overhaul — prior-art index (Phases 27–31)" in
`prior-art.md`, the shipped Phase-27 redesign (`spec-explorer-redesign.md`), the
shipped command palette (`spec-command-palette.md` + `command_palette.rs`), and
`file_tree.rs`/`worktree.rs`.

- **Paper "Explorer — Redesign" artboard, State B (Search) column** — the binding
  visual contract for the filter bar, the match-emphasis highlight chip, and the
  discrete multi-select flat fill (no accent bar). This phase realizes column B;
  Phase 27 shipped column A and reserved the header search slot + the header→tree
  seam this bar fills.
- **`helix-editor/nucleo` (`nucleo-matcher`, MPL-2.0)** — the fuzzy matcher the
  prior-art Phase-31 row recommends **reuse** of, and the exact matcher
  `spec-command-palette.md` deferred to "large-scale fuzzy *file* quick-open"
  (which this phase is). Its `Pattern` reports matched-character **indices**, the
  substrate the State B substring emphasis needs and a plain subsequence
  bool-match cannot supply.
- **`Canop/broot` incremental narrowing UX** (Category 5 #5, study-only): the
  command-palette-style live filter over a file tree — the interaction rift's
  filter bar mirrors (narrow-in-place, top match actionable).
- **`zed` `crates/file_finder`** (GPL-3.0, study-only): the summoned fuzzy
  file-picker shape quick-open follows (flat ranked list, open-on-confirm) —
  pattern, not code.
- **rift-local grounding**: `command_palette.rs` (the `Root`-overlay `list` +
  `input` modal + `filter_commands`/`is_subsequence` matcher this phase's
  quick-open generalizes with real ranking); `file_tree.rs` (the `visible_rows`
  derivation, `row_cache`/`cache_dirty` discipline, `render_header`'s reserved
  action slot, `render_row`'s name element + accent-bar selection, and the scoped
  `FILE_TREE_KEY_CONTEXT` action set); `worktree.rs` (`entries()` mirrors the
  whole daemon worktree — the quick-open/filter corpus); `connection_screen.rs`
  and `editor.rs` (`InputState`/`Input` usage patterns for the filter bar).

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so
edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Adopt `nucleo-matcher` (`helix-editor/nucleo`, MPL-2.0) as the fuzzy matcher; do not extend the command-palette subsequence match** | The prior-art Phase-31 row's verdict is **reuse** `nucleo`, and `spec-command-palette.md` explicitly deferred `nucleo` to exactly this "large-scale fuzzy *file* quick-open". The artboard's State B substring emphasis needs the **matched-character indices** `nucleo-matcher`'s `Pattern` returns; `command_palette.rs`'s `is_subsequence` returns only a bool and would have to be rebuilt into a bespoke position-tracking, path-aware ranked matcher — reimplementing what `nucleo` does well, against "reuse over rebuild" / "no premature abstraction". MPL-2.0 is already allow-listed, so `cargo deny` stays green. | 2026-07-08 |
| **Use the low-level synchronous `nucleo-matcher`, not the threaded `nucleo` driver** | Matching runs over the already-in-memory streamed tree (bounded, synchronous), so the `Matcher` + `Pattern` API suffices; the threaded `Nucleo` injector/worker would add concurrency with no benefit at this corpus size. Minimal surface, "as few dependencies/threads as possible". | 2026-07-08 |
| **Quick-open (and the filter) narrow the already-streamed tree; no daemon-side jwalk index, no new protocol** | `WorktreeModel::entries()` already mirrors the whole daemon worktree, so a client-side match over it covers every streamed project file with zero wire cost — keeping Phase 31 client-only, exactly like Phases 25/27. A daemon-side index (jwalk) is only justified to reach paths the daemon does not stream (e.g. `.gitignore`d files, #309), which is not this phase's need; deferring it avoids a `protocol`/daemon change and a second dependency. | 2026-07-08 |
| **The filter bar is an in-panel narrowing bar in Phase 27's reserved seam; quick-open is a separate summoned `Root`-overlay modal** | The roadmap row names both "in-panel fuzzy narrowing" and "jump-to-file quick-open" — distinct affordances. The filter narrows the **tree in place** (keeps hierarchy + decoration + multi-select), realizing State B; quick-open is a **flat ranked jump** over the whole file set, reusing the command-palette modal pattern already wired into `WorkspaceView`. Both share the one fuzzy substrate. | 2026-07-08 |
| **Filtered narrowing force-expands matches' ancestors but never mutates the user's `collapsed` set** | A match inside a collapsed directory must still show, but clearing the filter must return the tree exactly as the user left it. So the filtered `visible_rows` pass ignores `collapsed` for ancestors-of-matches only, as a render-time derivation; the real `collapsed` set is untouched and reinstated on clear. Mirrors Phase 27's "re-arrange, never mutate the derived data" discipline. | 2026-07-08 |
| **Match emphasis is a theme-token span on the matched indices, not a new color** | Constitution: theme tokens only. The artboard quotes a State B highlight hex for review legibility; it maps to an existing highlight/accent-tint role. `render_row` splits the name on the carried match indices and tints only those characters — no hardcoded literal, no new palette value. | 2026-07-08 |
| **Discrete multi-select is a parallel selection set with a flat fill (no accent bar); its live consumer is open-many** | The artboard draws multi-select as a discrete flat-surface fill distinct from single-selection's accent bar, so the set is a parallel `HashSet` beside the existing `selected` cursor. To avoid a dead visual (constitution), the reachable v1 consumer is **open-many**: activating the set emits `OpenFile` per file, opening each as an editor tab through the existing path (the editor already hosts files in a `TabPanel`). The context menu (Phase 29) and file ops (Phase 30) act on the same set when present, but Phase 31 does not depend on them. | 2026-07-08 |
| **Quick-open's summon is a terminal-safe `Ctrl/Cmd+Shift` chord; filter/multi-select keys stay scoped to `FILE_TREE_KEY_CONTEXT`** | Agent-first is non-negotiable. Quick-open is globally reachable like `OpenCommandPalette`, so it uses a `Ctrl/Cmd+Shift` chord the terminal does not consume (the `Ctrl+Shift+P` precedent), never a bare `Ctrl+P` (terminal readline claims it). Every in-panel key (filter toggle, `Esc`, `Shift+Up/Down`) is scoped to the tree's key context, so a focused terminal pane never loses a keystroke. | 2026-07-08 |
| **Client-only, no daemon / protocol change; the only new dependency is `nucleo-matcher`** | Every value needed is already streamed onto the client model; this is rendering + interaction plus a matcher. Matches Phases 25/27's "reads the existing model" ethos and confines the change to `crates/app`. | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per
implementable step, grouped under the milestone. This spec owns the design; the
issues own progress. Created once this spec is `READY` and merged to `develop`.

- Milestone: Phase 31 — Explorer search & filter (created at `READY`)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue
that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes; `cargo test --workspace`
      passes; CI `app-check` compiles the app.
- [ ] `cargo deny check licenses` passes with `nucleo-matcher` added; `cargo tree`
      shows `nucleo-matcher` present and the threaded `nucleo` driver **not**
      pulled in unless it is a transitive requirement.
- [ ] Toggling the header search control opens the filter bar in the header→tree
      seam; typing narrows the tree to matching files plus their ancestor
      directories, with matches inside collapsed directories shown; clearing +
      `Esc` restores the exact prior tree (collapse state intact). Asserted
      headlessly over the filtered `visible_rows` derivation (match set, ancestor
      inclusion, forced expansion, restore-on-clear); the visual placement is a
      QA-gate item.
- [ ] The matched substring is emphasized on exactly the matched characters via a
      theme token; a `grep` confirms no hardcoded color literal was introduced.
      The fuzzy substrate is unit-tested for match/no-match, ranking order, and
      the reported indices, with valid and malformed input (empty, non-ASCII,
      out-of-order, no-match).
- [ ] The jump-to-file quick-open summon (the terminal-safe chord) opens a
      `Root`-overlay ranked file list; arrows navigate; `Enter` opens **and
      reveals** the selected file (expands ancestors, selects, scrolls into view)
      via the existing `OpenFile`/`reveal` path; `Esc` dismisses leaving
      terminal/editor state untouched. Candidate list + ranking asserted
      headlessly; the modal appearance is a QA-gate item.
- [ ] `Ctrl/Cmd+Click` toggles a path in the multi-set, `Shift+Click` ranges from
      the cursor, `Shift+Up`/`Shift+Down` extend it; multi-selected rows show the
      discrete flat fill (no accent bar) while the cursor row keeps the accent
      bar; activating the selection opens every selected file as a tab. The set
      operations, prune-against-visible, and open-many target list are asserted
      headlessly.
- [ ] Agent-first: a `grep`/binding audit confirms the filter toggle, `Esc`, and
      `Shift+Up`/`Shift+Down` are bound under `FILE_TREE_KEY_CONTEXT` and
      quick-open's summon is a `Ctrl/Cmd+Shift` chord (not a bare `Ctrl+P`); at QA
      a focused terminal pane keeps receiving those keystrokes.
- [ ] `grep` confirms no agent detection introduced, no new protocol message, and
      no change under `crates/protocol`, `crates/daemon`, or `crates/explorer` —
      the change is confined to `crates/app`.
- [ ] Milestone QA (dev channel): the explorer reads like the "Explorer —
      Redesign" artboard's **State B** column — the live search toggle + filter
      bar in the reserved seam, narrowed rows with the highlighted match
      substring, the discrete multi-select flat fill distinct from the single
      accent bar — and quick-open jumps to any file by fuzzy name, while icons,
      context menu, and file ops remain deferred to their phases.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The filter narrowing drifts or corrupts the row cache / real collapse state | Narrowing is a render-time derivation over `entries()`; the user's `collapsed` set is never mutated (force-expansion is local to the filtered pass), and a query change marks `cache_dirty` through the existing seam, so clearing returns the exact prior tree. A headless test filters, asserts the match+ancestor set, clears, and asserts the tree equals a fresh unfiltered build. |
| `nucleo-matcher`'s match indices are byte- vs char-based, mis-splitting the highlight on non-ASCII names | The substrate normalizes the reported indices to the char boundaries `render_row` splits on and is unit-tested with a non-ASCII candidate; the emphasis span uses char indices, never raw byte offsets. |
| Quick-open's global chord steals a terminal keystroke (agent-first regression) | It uses a `Ctrl/Cmd+Shift` chord the terminal does not consume (the `Ctrl+Shift+P` precedent), never a bare `Ctrl+P`; a QA item confirms the focused terminal still receives the chord's base key. |
| Multi-select's open-many reads as contrived / a dead visual if the editor cannot show multiple files | The editor already hosts open files in a `TabPanel`, so open-many opens each selected file as a tab — a real capability. QA opens a multi-selection and confirms each file opens; the context menu (29) / file ops (30) later consume the same set. |
| Adding `nucleo-matcher` fails `cargo deny` or pulls a heavier transitive tree | MPL-2.0 is already allow-listed; the isolated matcher issue (issue 1) adds the dependency behind a pure, tested module and runs `cargo deny check licenses` before any UI slice depends on it, de-risking the add first. |
| Two issues both edit `file_tree.rs` (`render_row`, selection, `visible_rows`) → rebase churn | Sequence the `file_tree.rs` slices: the fuzzy substrate (issue 1, mostly a new module) lands first; the filter bar + narrowing + emphasis (issue 2) is the `visible_rows`/`render_header`/`render_row` slice; multi-select (issue 3) follows issue 2 so the shared `render_row` + selection edits land once in order. Quick-open (issue 4) is a separate module depending only on issue 1, disjoint from the `file_tree.rs` slices. |
| The filter `input` swallows the tree's arrow/`Enter` navigation | The `input` binds only text editing; the tree's `SelectUp`/`SelectDown`/`OpenSelected` stay scoped actions on the tree root, so navigation keeps firing while the input holds text focus. A QA item types a query then navigates the narrowed results by keyboard. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-07-08: Spec created from `/loopkit:plan` (roadmap Phase 31 — Explorer
  search & filter, the search/filter/multi-select phase of the explorer overhaul
  27–31). Builds on the shipped Phase-27 A-Default baseline
  (`spec-explorer-redesign.md`): fills the reserved header search slot + the
  header→tree seam, and realizes the artboard's **State B** narrowing + match
  emphasis plus the discrete multi-select fill Phase 27 documented but did not
  implement. Genuinely-open decisions settled at authoring: **matcher** —
  adopt `nucleo-matcher` (MPL-2.0, prior-art's recommended reuse and the exact
  matcher the command palette deferred here), not an extended subsequence match,
  because State B's substring emphasis needs match indices; **quick-open corpus**
  — narrow the already-streamed tree (`entries()` mirrors the whole daemon
  worktree), no daemon-side jwalk index, keeping the phase client-only with no
  protocol/daemon change; **multi-select consumer** — open-many via the existing
  `OpenFile`/`TabPanel` path so the state is reachable, with the context menu
  (29) / file ops (30) as forward-compatible consumers, not dependencies;
  **keyboard scoping** — in-panel keys under `FILE_TREE_KEY_CONTEXT`, quick-open
  on a terminal-safe `Ctrl/Cmd+Shift` chord, preserving agent-first delivery. The
  only new dependency is `nucleo-matcher`; the change is confined to `crates/app`.
