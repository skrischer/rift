# Spec: Real editor minimap

> Status: COMPLETED (2026-08-06)
> Created: 2026-08-01

Replace the 32px line-length marks-strip with a scaled code miniature plus a
viewport indicator and drag-to-scroll. Roadmap Phase 55.

## Outcome

- [ ] The editor's right-edge strip renders a scaled miniature of the whole document that reads as a recognizable code silhouette — line indentation and length structure, not just single length bars — replacing today's per-bucket line-length bars (`editor.rs:3557` `paint_minimap`).
- [ ] A viewport indicator (the slab already computed at `editor.rs:3520`/`:3619`) shows the visible region over the miniature.
- [ ] Dragging the miniature scrolls the document (not only the current click-to-jump); the drag follows the pointer continuously.
- [ ] The miniature is derived off the render hot path (cached, recomputed on text change like today's `minimap_samples`, `editor.rs:953`), so large files (up to the 2 MB buffer cap, `daemon/src/buffer.rs:123`) do not re-shape text per frame.
- [ ] Diagnostics remain marked on the miniature (the existing per-line diagnostic marks, `editor.rs:3603`).

## Scope

### In scope

- A denser, structure-bearing miniature painted by the existing hand-drawn `canvas` + `paint_quad` path (`editor.rs:3247-3272`, `paint_minimap` `:3557`): per line, a block whose horizontal extent reflects **leading indentation + line length** (the indentation silhouette is what makes a minimap recognizable), shaded from the theme palette (`cx.theme().highlight_theme`, reachable per `editor.rs:2504`), with diagnostics overlaid as today.
- Drag-to-scroll: extend the existing minimap mouse-down (`editor.rs:3266` → `minimap_jump` `:2056`) into a drag, mirroring the terminal border-drag idiom (`crates/terminal/src/session_view.rs:2623-2668`) — a full-window occluding overlay capturing `on_mouse_move`/`on_mouse_up`, mapping pointer-Y → line ratio → scroll target each move.
- Reuse the viewport slab (`minimap_slab_fracs` `:3520`, painted `:3619`) as the viewport indicator.
- Keep the sample cache model: recompute the miniature representation once per text change (`recompute_minimap_samples` `:953`), not per frame; bound the work to the downsample cap (`MINIMAP_SAMPLES = 1024`, `:357`). **The cache must carry indentation through the bucket**: today `sample_line_lengths` (`:3495`) collapses many source lines into one bucket taking the *max* length, which discards per-line indentation; the extended cache stores a per-bucket box (e.g. min leading-indent + max length) so the indentation silhouette survives bucketing on large files rather than degrading back to length bars.

### Out of scope

- Forking or extending the longbridge gpui-component pin (git dep, rev `9ad30e6…`, not our fork — `Cargo.toml:75`). The pin appears to expose **no scroll-offset setter and no per-token/span accessor** (inferred from the app's use sites — the pin source is not vendored in the checkout, so path B's first step is to CONFIRM the API is genuinely absent against the rev; if such an API already exists, a caret-free drag / token color is cheap and B is not really a fork). A token-glyph-accurate miniature and a caret-free drag otherwise require extending that dependency. Whether to make that commitment is the gate decision; the deferred richer path is recorded, not built here unless the gate elects it.
- A glyph-scaled miniature (rendering shrunk actual characters) — out regardless of the gate; the realistic target is per-line shaded blocks, not scaled text.
- Token-semantic coloring beyond what the theme palette + diagnostics already give the app, unless the gate elects the pin extension (the InputState exposes no per-span token accessor to the app; only `tree-sitter-rust` is compiled, so an app-side pass would be Rust-only and duplicate the widget's own highlighting).
- Any change to the editor text model, LSP, or protocol — client-side rendering only.

## Constraints

- **The gpui-component editor pin is the binding limit.** It exposes only `visible_row_range()`, `line_len(row)`, `line_height()`, `range_to_bounds()`, and `set_cursor_position()` (`editor.rs` use sites `:2850-2862`). There is **no scroll-offset setter** — the only public scroll seam is `set_cursor_position`, which scrolls the caret into view and therefore moves the caret (`editor.rs:2050-2052`). And **no per-token/span accessor** — the app sees `line_len` + the theme palette + per-line diagnostics, nothing finer.
- The pin is **longbridge upstream**, not a rift fork; extending it means maintaining a fork or landing an upstream change — a real dependency commitment, not a local edit.
- The miniature must stay off the render hot path: cache it, recompute on text change only (the current `minimap_samples` `Rc<[u32]>` cache, `editor.rs:607`/`:953`, is the model to preserve/extend).
- Files run up to the 2 MB buffer cap (`daemon/src/buffer.rs:123`) → tens of thousands of lines; the whole text arrives at once (`FileContent`, `protocol/src/lib.rs:572`). The downsample cap (`MINIMAP_SAMPLES = 1024`) bounds per-frame paint work; a heavy recompute belongs in `spawn_blocking` if it exceeds a cheap synchronous pass.
- Agent-agnostic, client-only; no new dependency for the in-API path; reuse the existing `canvas`/`paint_quad` and the `Rc<Cell<Bounds>>` mouse-math idiom (`minimap_bounds` `:659`).

## Prior art

- [QA-seeded phases — prior-art index (Phases 48–56)](prior-art.md#qa-seeded-phases--prior-art-index-phases-4856) — the Phase 55 row: a scaled code miniature + viewport slab + drag-to-scroll replacing the 32px marks-strip (line-length bars, click-to-jump only); virtualize/cache the miniature (`spawn_blocking`) for large files. References Zed `crates/editor` minimap and the gpui-component code-editor story.

## Human prerequisites

- none — client-side editor rendering only.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| The miniature is per-line shaded blocks reflecting indentation + length, not scaled glyphs | Glyph-scaling is expensive and needs the widget's own shaping; the indentation silhouette is the recognizable minimap signal and is cheaply derivable from `line_len` + leading whitespace | 2026-08-01 |
| Keep the once-per-text-change cache model (extend `minimap_samples`), recompute off the render path; heavy passes go to `spawn_blocking` | Mirrors today's proven cost control; files reach the 2 MB cap | 2026-08-01 |
| Reuse the terminal border-drag overlay idiom for drag-to-scroll | It is the established in-repo drag pattern (occluding overlay + move/up), avoiding a new interaction primitive | 2026-08-01 |
| Client-side only; no editor-model / protocol change | The minimap is pure rendering over data the editor already holds | 2026-08-01 |
| v1 stays **in-API (path A)**: indent+length+diagnostic-shaded blocks with the indentation silhouette, and drag-to-scroll via `set_cursor_position` (accepting that the caret follows the drag). No gpui-component fork; token color and caret-free drag are deferred to a possible later pin extension | Accepted at the gate. Ships now with no dependency commitment; the indentation silhouette + drag are a real upgrade over today's length bars; the caret-follow drag wart and absent token color are the accepted v1 cost | 2026-08-01 |

## Tracking

- Milestone: created at the spec-acceptance gate
- Issues: created from this spec once merged — the miniature render (shaded blocks + cache) and the drag-to-scroll interaction; plus, if the gate elects path B, a gpui-component pin-extension step (scroll setter ± token accessor) the other two depend on. Dependency edges in the issue bodies.

## Verification

- [ ] `just ci` equivalent green for `app` (fmt + clippy `-D warnings` + tests); the crate compiles warm-target clean.
- [ ] Unit test: the miniature representation derives from a document with known indentation/length into the expected per-bucket block boxes (indent offset + extent; per-line for small files, per-bucket min-indent/max-len for files past the downsample cap); the cache recomputes only on text change.
- [ ] QA: the strip reads as a code silhouette (indentation structure visible), not flat length bars; diagnostics still mark their lines.
- [ ] QA: dragging the miniature scrolls the document continuously; the viewport slab tracks the visible region. (Path A: note the caret follows; Path B: the caret does not move.)
- [ ] QA: a large file (near the 2 MB cap) opens and scrolls without per-frame jank (recompute is off the render path).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| `set_cursor_position` drag moves the caret (path A wart) | Accepted and documented for path A; path B (pin scroll setter) removes it. Surfaced at the gate so the choice is deliberate |
| A gpui-component fork (path B) becomes a maintenance burden / diverges from upstream | Prefer upstreaming the accessor; pin-note discipline already governs the rev (`Cargo.toml:63-75`); scope the pin change to the minimum (a scroll-offset getter/setter, optional token accessor) |
| Recomputing the miniature janks large files | Keep the once-per-change cache; move a heavy pass to `spawn_blocking`; bound by `MINIMAP_SAMPLES` |
| The miniature is barely better than today's bars | The indentation offset (a block starts at its leading indent, not left-aligned length-only) is the recognizable-minimap signal and the real path-A upgrade — the sample cap stays 1024, so density is unchanged; drag-to-scroll is the second upgrade; token color (path B) is the further step |

## Decision log

- 2026-08-01: Scoped from `crates/app/src/editor.rs` (Explore agent). The strip is hand-drawn (`canvas` + `paint_quad`, `paint_minimap` `:3557`); the viewport slab and click-to-jump already exist. The binding limit is the longbridge gpui-component pin: no scroll-offset setter (only `set_cursor_position`, which moves the caret) and no per-token accessor — so a caret-free drag and a token-colored miniature both require extending an upstream dependency. The single open item — in-API v1 vs. a pin-extension commitment — carried to the gate.
- 2026-08-01: Spec-acceptance gate (PR #935) — accepted. Review returned APPROVE (verified the strip is hand-drawn, the terminal border-drag is a reusable idiom, and the app-side evidence for the pin having no scroll setter / token accessor — flagged unverifiable from the checkout, so the implementer confirms against the rev) with non-blocking findings folded: the bucket cache must carry a per-bucket min-indent/max-len box so the indentation silhouette survives downsampling; "denser sampling" corrected (cap stays 1024, the indentation offset is the upgrade); path B's first step is confirming the pin API is absent. Gate decision: **in-API v1 (path A)** — indentation-silhouette blocks + drag via `set_cursor_position` (caret follows); no gpui-component fork; token color + caret-free drag deferred.
