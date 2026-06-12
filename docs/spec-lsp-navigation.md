# Spec: Phase 5 — LSP navigation

> Status: DRAFT
> Created: 2026-06-12
> Completed: —

rift's editor gains the **pull half of LSP** — hover, go-to-definition, and find-references — over a new request/response navigation path: the editor asks, the daemon routes the request to the running language server, and the jump lands in rift's own editor (`vision.md` Scenario 3: "the app sends the LSP request and rift's editor jumps to the definition — in the GUI, not by remote-controlling a terminal editor"). This delivers the committed sibling sub-spec that `spec-daemon-lsp.md` deferred onto the editor track, now that the editor surface (`spec-editor.md`) exists to consume it.

## Outcome

What is true when this work is done? Observable, end-to-end criteria — not activities.

- [ ] Hovering a symbol in the open file surfaces the server's hover content in a popover at the symbol, with markdown rendered; a symbol the server has nothing for shows no popover (silent no-op, no error surface).
- [ ] "Go to definition" — via ctrl+click and the context menu — jumps to the symbol's definition: a same-file target scrolls and selects the range; a cross-file target inside the worktree opens the file through the existing buffer channel and lands on the range.
- [ ] "Find references" lists the symbol's references (worktree-relative path + line preview) in a transient jump-list; selecting an entry jumps to that location.
- [ ] Every jump can be unwound: back-navigation returns the editor to the position it jumped from (bounded in-memory stack).
- [ ] Positions are correct against **unsaved edits**: a request after a not-yet-saved buffer change resolves against the live buffer the editor already feeds via `didChange` (#189), and the client never deals in UTF-16 offsets — the daemon owns all offset-encoding translation.
- [ ] Responses are correlated to requests: a slow or superseded response never lands on the wrong file or position; concurrent hover requests do not interleave.
- [ ] A definition target **outside the worktree root** (stdlib, registry deps) behaves per the gate decision (see Open decisions) — never a crash, never a silent dead click.
- [ ] Agent-agnostic throughout: no agent or editor-process detection anywhere in the path; any language with a server in the existing table gets navigation with zero language-specific code.

## Scope

### In scope

- **Protocol — navigation request/response message set** (`crates/protocol`): the second request/response family after the buffer channel (#184). Requests carry a worktree-relative path + position and a correlation id; responses carry rift's own location / markup types (no `lsp-types` leakage). Three operations: hover, go-to-definition, find-references. The correlation-id convention follows the buffer channel's; the exact rift position unit (line + column encoding) is pinned in the first protocol issue.
- **`crates/lsp` — request path**: extend the push-only client with typed requests (`textDocument/hover`, `textDocument/definition`, `textDocument/references`) through `async-lsp`'s request support; capability check before dispatch; translation `lsp-types` ↔ rift protocol types including **offset-encoding translation** (UTF-8/UTF-16, negotiated per server — the helix-lsp `util.rs` precedent).
- **Daemon routing**: handle the new `ClientMessage` navigation requests, route to the capable server via the existing registry, respond off the dispatch loop (the loop never blocks on server I/O).
- **Editor UX** (`crates/app`): hover popover (markdown via `gpui-component`), ctrl+click + context-menu go-to-definition, references jump-list, cross-file open through the existing buffer channel, and a bounded back-jump stack.

### Out of scope — the hard not-in-v1 list

> Inherits the editor spec's load-bearing rule (`architecture.md`: "Editors eat roadmaps"). Navigation must not become the wedge that pulls the roadmap onto editor-feature parity.

- **Rename, format, code actions, completion, signature help** — the editor spec's hard not-in-v1 list stands; none of these ride along.
- **Peek-definition view, breadcrumbs, workspace symbol search, semantic tokens, inlay hints** — later editor sub-specs, if ever.
- **References dock panel** — v1 is a transient jump-list; persistent panels are the explorer-panel sub-spec's territory.
- **Multi-server response aggregation** — v1 routes each request to the first capable server (Helix precedent); aggregating references across servers is a later refinement.
- **Multi-root / per-pane contexts** — single root, mirroring every Phase 3 cut.
- **Navigation from terminal panes** — ctrl+click in a terminal stays what it is today (link opening); panes are black boxes.

## Human prerequisites

None. rust-analyzer (the proving server) on the remote `$PATH` is already required by the diagnostics milestone; navigation adds no new secrets, accounts, or external provisioning.

## Constraints

- **Sequencing.** The protocol / `crates/lsp` / daemon slices sequence after the LSP milestone's core (#173–#177) and the buffer-channel protocol precedent (#184). The app slice additionally sequences after the editor surface (#187) and the live-buffer `didChange` feed (#189) — without #189, positions on a dirty buffer would be answered against stale disk state, which is a correctness bug, not a degraded mode. This spec can reach `READY` now; implementation unblocks behind those issues.
- **Request/response is the deliberate exception** to "state flows through channels", established by the buffer channel (`spec-editor.md`). Navigation adds no push messages; the diagnostics push path is untouched.
- **`crates/protocol` stays `lsp-types`-free** — the daemon translates. Position encoding is part of that translation: LSP servers default to UTF-16 offsets; the client and protocol speak only rift's own position type, and `crates/lsp` owns the conversion against the document text it already syncs.
- **`crates/lsp` and the daemon stay `gpui`-free and musl-clean** — unchanged gates (`daemon-musl` CI job).
- **No new dependencies expected.** `async-lsp` already provides typed requests; the hover popover and list UI come from `gpui-component` (existing dep). If the `gpui-component` editor lacks a needed mouse/hover hook, the fallback is direct GPUI mouse-event handling in rift's own editor wrapper — not a new crate.
- **Out-of-root file access** (if the gate decides read-only open): the buffer service's root restriction is a deliberate boundary; any carve-out must be read-only and explicit, never a general write path outside the root.
- `thiserror` in libraries, `anyhow` in the daemon binary; no `.unwrap()` in library code.

## Prior decisions

Decisions already made that the implementor must respect. Rationale included so edge cases can be judged.

| Decision | Rationale | Date |
|---|---|---|
| **Navigation is the committed editor-track sibling of the diagnostics spec** — hover / go-to-definition / find-references, consumed by rift's own editor | Pre-recorded in `spec-daemon-lsp.md` (review-gate decision 2026-06-11) and `docs/roadmap.md` Phase 5; the editor surface (`spec-editor.md`, milestone 14) is the consumer that makes the request/response plumbing non-premature. | 2026-06-11 |
| **Pull = request/response on the protocol**, mirroring the buffer channel; navigation never pushes | Constraint-determined: hover/definition/references are inherently request-shaped; the buffer channel (#184) established the protocol's request/response convention; push stays reserved for state (worktree / git / diagnostics). | 2026-06-12 |
| **First-capable-server routing** — each request goes to the first registered server for the language that advertises the capability; no aggregation in v1 | Precedent-decided: Helix routes pull requests to the first supporting server (`helix-lsp` Registry, `prior-art.md` pattern #8); aggregation is a references-only nicety deferred with multi-server polish. | 2026-06-12 |
| **Offset-encoding translation lives daemon-side in `crates/lsp`**; the client and protocol never see UTF-16 | Constraint-determined: `crates/protocol` is `lsp-types`-free, and only the daemon has the synced document text needed for the conversion. Precedent: `helix-lsp/src/util.rs` offset-encoding conversion (`prior-art.md` Category 7). | 2026-06-12 |
| **Hover content renders as markdown via `gpui-component`** | LSP hover returns `MarkupContent` (markdown); `gpui-component` ships a Markdown component (gallery-demoed, #180) — don't rebuild a renderer. | 2026-06-12 |
| **References UX = transient jump-list, not a dock panel** | Constraint-determined: minimal solution (`CLAUDE.md`); persistent panels are the explorer-panel sub-spec's territory per the editor spec's out-of-scope list. | 2026-06-12 |
| **A bounded back-jump stack is in scope** | A jump with no way back is half a feature — the same "a tree that opens nothing" logic that bundled the file-tree render with the editor. Depth is bounded (in-memory, back-only; no forward stack, no persistence). | 2026-06-12 |
| **v1 trigger surface = Scenario 3's set**: hover popover, ctrl+click, context menu (exact keybinds and hover-trigger mechanics pinned in the app issue) | `vision.md` Scenario 3 names exactly these affordances; anything more is editor polish. | 2026-06-12 |
| **OPEN — resolved at the spec-acceptance gate**: out-of-root definition targets — (a) refuse with a visible notice naming the target path, or (b) read-only open through a bounded buffer-service carve-out | Genuinely open: neither precedent nor constraint settles it. (a) is cheaper but guts go-to-definition into stdlib/deps — a large share of real jumps in Rust; (b) needs a read-only editor mode + an explicit out-of-root read path in the daemon. | — |

## Tracking

The decomposition into steps lives as GitHub issues, not here — one issue per step under the Phase 5 milestone. Created once this spec is `READY` and merged to `develop` (the issue-spec gate resolves the spec path against the default branch).

- Milestone: created at `READY` (LSP navigation)
- Issues: created from this spec once `READY` (one per implementable step)

Each issue references this spec path. A PR may only merge if it closes an issue that traces back here (planning gate).

## Verification

- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --release -p rift-daemon --target x86_64-unknown-linux-musl` still produces a static binary with the navigation path linked
- [ ] Integration test against the existing **stub LSP server** extended with canned hover/definition/references responses: each request round-trips daemon-side — correct rift-typed result for a known position, empty result for an unknown one
- [ ] **Correlation test**: a delayed response to a superseded request is dropped, not applied — driven by a stub server answering out of order
- [ ] **Offset-encoding test**: positions on a line containing multi-byte characters (e.g. `ä`, CJK, emoji) map correctly in both directions for a UTF-16 server — the canonical off-by-N trap
- [ ] In the editor: ctrl+click on a symbol jumps same-file (scroll + select) and cross-file (opens via the buffer channel, lands on the range); the context-menu path does the same
- [ ] Go-to-definition after an **unsaved** edit that moved the symbol resolves against the live buffer (correct target, not the stale disk position)
- [ ] Find-references lists multiple files; selecting an entry jumps; back-navigation returns to the pre-jump position (and unwinds across multiple jumps)
- [ ] Hover on a symbol shows the popover with rendered markdown; hover on whitespace/no-result shows nothing
- [ ] Out-of-root definition target behaves exactly per the gate decision (notice or read-only open), verified with a stdlib/dependency jump in a Rust fixture
- [ ] A `grep` confirms no agent/editor-process detection in the navigation path and no `lsp-types` types in `crates/protocol`

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| The `gpui-component` editor lacks the mouse/hover hooks for ctrl+click and hover popovers | Spike the hook surface early in the app issue; fallback is handling GPUI mouse events in rift's own editor wrapper (the component renders, rift owns interaction) — pre-vetted as no-new-crate. |
| Stale-response races: the user moves on (new file, new edit) before a response arrives | Correlation id + drop-stale discipline, mirrored from the buffer channel; the correlation test in Verification is the regression net. |
| UTF-16 ↔ UTF-8 position mapping bugs (the classic LSP off-by-N on non-ASCII lines) | Conversion isolated in `crates/lsp` against the synced document text; dedicated multi-byte fixture test; `helix-lsp/src/util.rs` is the reference implementation. |
| A server lacks a capability (e.g. no references support) or is still indexing | Capability check before dispatch → silent no-op surfaced as "no result" (log daemon-side); requests during indexing return empty — never block the UI, never error-modal. |
| Scope creep toward editor power features (peek, rename, symbols…) | The hard not-in-v1 list above is load-bearing, mirroring the editor spec; every extension is a deliberate later sub-spec. |
| The back-jump stack interacts badly with buffer reloads/conflicts (positions drift) | Bounded v1 semantics: the stack stores path + position, best-effort after external changes — a clamped landing beats a refused jump; exact clamping pinned in the app issue. |

## Decision log

Decisions made during implementation. Added as work progresses.

- 2026-06-12: Spec created from `/loopkit:plan` (loop mode — roadmap Phase 5). Recorded as precedent / constraint-decided: the request/response navigation family (second after the buffer channel), first-capable-server routing, daemon-side offset-encoding translation, markdown hover via `gpui-component`, transient references jump-list, the bounded back-jump stack, and the Scenario-3 trigger surface. The one genuinely-open decision — out-of-root definition targets (refuse vs. read-only open) — is flagged for the spec-acceptance gate.
