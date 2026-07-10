# Spec: v1.0 hardening & polish

> Status: READY
> Created: 2026-07-08
> Completed: —

Close the correctness, data-safety, and table-stakes-polish gaps a v1.0 daily
driver must not ship without: surface the daemon-unavailable degraded mode,
give the buffer channel real error replies (no more silent 10 s timeouts),
cap editor reads, never lose unsaved edits on quit, and finish the two
half-built editor/terminal search affordances.

## Outcome

What is true when this work is done (done-criteria, not a progress tracker):

- [ ] Starting a daemon-mode session with no daemon available shows a
  persistent, dismissible on-screen indication that IDE features are disabled —
  the app no longer degrades silently (only a `warn!`) to the legacy tmux path
  with empty Explorer / Source-Control / Problems panels and a non-functional
  editor.
- [ ] A refused `OpenFile` / `SaveFile` (binary, non-UTF-8, unreadable path,
  too-large file, or write failure) produces an **immediate** typed error reply
  the editor renders at once, naming the reason; recovery no longer depends on
  the 10 s `OPEN_TIMEOUT` / `SAVE_TIMEOUT` fallback.
- [ ] Opening a file above the daemon's read-size cap yields a `TooLarge`
  reply the editor renders as a read-only placeholder state instead of shipping
  the whole file in one unbounded message.
- [ ] Quitting the app with any dirty editor tab prompts an aggregated
  confirm/discard dialog before the window closes; confirmed-discard or a
  clean workspace closes as before.
- [ ] The editor has in-file find/replace and go-to-line, operating on the
  loaded buffer with no new protocol.
- [ ] A terminal pane has a working scrollback search mode (query entry, match
  navigation) that lights up the already-present but dead
  `search_current` / `search_match` render fields.
- [ ] `PROTOCOL_VERSION` is bumped and `PROTOCOL_FINGERPRINT` re-pinned for the
  new buffer-error replies; `docs/protocol.md` records the change.

## Scope

### In scope

- **Daemon-unavailable UX** (`crates/app`). Today `main.rs`'s daemon-terminal
  branch falls back to the legacy tmux path with only a `warn!` when
  `use_daemon_terminal()` is true but `provision_daemon` returned `None`
  (`crates/app/src/main.rs`, the `None => warn!("daemon terminal selected but
  no daemon available; falling back …")` arm). In that mode the reactive layer
  is dead — empty Explorer / Source-Control / Problems and a non-functional
  editor — with no on-screen signal. Surface a persistent, dismissible
  empty-state/banner ("Daemon unavailable — IDE features disabled") through the
  existing `Root` notification layer
  (`Root::render_notification_layer`, `crates/app/src/workspace.rs`) or the
  connection-screen status banner (`crates/app/src/connection_screen.rs`).

  > **Superseded (2026-07-10, #285):** the legacy `tmux -CC` fallback was
  > removed — the daemon is now the sole terminal source — so there is no
  > "degraded but running" mode to banner. A daemon-unavailable connect now
  > fails cleanly to the connection-screen status banner (the fallback
  > surface this spec already anticipates under Risks). The Root-notification
  > degraded-mode banner (#619) is retired with the legacy path.

- **Buffer-channel error replies** (`crates/protocol`, `crates/daemon`,
  `crates/app`). Add `OpenError` / `SaveError` daemon→client variants carrying a
  typed reason (`Binary`, `NotUtf8`, `PermissionDenied`, `NotFound`,
  `TooLarge`, `Io`). Today `request_reply` in `crates/daemon/src/lib.rs` logs
  the `BufferError` and returns `None` — "a refused request simply produces no
  reply, and the editor falls back to its own timeout". Map each
  `buffer::BufferError` (and the new `TooLarge`) to the matching reply so the
  daemon answers immediately; the editor renders the specific reason at once
  instead of after `OPEN_TIMEOUT` / `SAVE_TIMEOUT`
  (`crates/app/src/editor.rs`).

- **Editor read-size cap** (`crates/daemon`, `crates/app`).
  `buffer::read_file` (`crates/daemon/src/buffer.rs`) reads the whole file via
  `tokio::fs::read` with no ceiling and the daemon ships it in one message —
  while the diff path already caps at ~2 MB / ~20k lines
  (`FileDiffPayload::TooLarge`). Add a byte-size cap in `read_file` returning a
  `TooLarge` outcome that maps onto the `OpenError { reason: TooLarge }` reply
  above; the editor renders it as a read-only / placeholder state.

- **Unsaved-changes guard on window close** (`crates/app`).
  `on_window_should_close` in `crates/app/src/workspace.rs` unconditionally
  returns `true`, so dirty editor tabs are lost on app quit even though per-tab
  close already prompts (`EditorView::close_tab` →
  `confirm_close_tab` → `AlertDialog`, `crates/app/src/editor.rs`). If any tab
  is dirty, return `false` and open the existing `AlertDialog` confirm/discard
  flow (aggregated across all dirty tabs) before allowing the close.

- **Editor in-file find/replace + go-to-line** (`crates/app`). None exists
  today; Scenario 3 sells "fix it in place". Add find/replace and a
  go-to-line action operating on the loaded buffer, reusing the `gpui-component`
  `Input` widget's search facility or a light theme-token overlay. No new
  protocol — this is purely client-side over the already-loaded buffer.

- **Terminal scrollback search** (`crates/terminal`).
  `crates/terminal/src/pane_view.rs` hardcodes `search_current: false` /
  `search_match: false` on every rendered cell and aliases
  `search_match_bg` / `search_current_bg` to `selection_bg`, with no
  `SearchState` anywhere — half-wired dead fields. Implement a scrollback search
  mode (query state + match navigation) that populates those render fields, with
  distinct theme-derived highlight colors for match vs current-match.

### Out of scope

- **Daemon binary distribution / embedding.** Embedding the musl daemon in the
  app (`include_bytes!`) so the reactive layer works with `RIFT_DAEMON_BINARY`
  unset is a packaging / build-pipeline change, not a hardening fix. rift's
  primary user builds locally with `RIFT_DAEMON_BINARY` set, so the
  daemon-unavailable banner above is the correct v1.0 treatment; auto-embedding
  is deferred to a **post-1.0 packaging phase**. Recommend adding a "post-1.0"
  row to `docs/roadmap.md` when that phase is planned. (This spec does not edit
  the roadmap.)
- **Binary / non-UTF-8 file viewers.** The daemon still refuses non-UTF-8
  content; a pluggable binary viewer stays a future sub-spec (`spec-editor.md`).
  This spec only makes the *refusal* observable, it does not add a viewer.
- **New editor find semantics beyond the loaded buffer** — project-wide
  find/replace across files is not in scope; find operates on the open buffer
  only.

## Constraints

- **Protocol change is deliberate and version-bumped.** Adding `OpenError` /
  `SaveError` is a message-set change: bump `PROTOCOL_VERSION` (currently `7`,
  `crates/protocol/src/lib.rs`) to `8`, re-pin `PROTOCOL_FINGERPRINT` (the
  fingerprint test fails until re-pinned), and add a version-8 line to
  `docs/protocol.md`'s History and the buffer-channel section. Keep the change
  **additive** — existing consumers keep compiling and deserializing. Strict
  equality means client and daemon must rebuild together; no cross-version
  translation.
- **Reason enum is rift's own type**, mirroring how `DiagnosticSeverity` /
  `SymbolKind` mirror LSP types — no third-party error type crosses the wire.
  `NotFound` / `PermissionDenied` / `Io` derive from the daemon's
  `BufferError::Io { source }` by inspecting `std::io::ErrorKind`; `NotUtf8`
  from `BufferError::NotUtf8`; `TooLarge` from the new read cap. A path escape
  is a client-side impossibility (the editor only sends worktree-relative or
  out-of-root-nav paths), so it maps to a generic `Io` reason rather than a
  distinct variant.
- **Agent-agnostic.** No new code path detects or special-cases an agent.
  Terminal scrollback search operates on the raw `alacritty_terminal::Term`
  grid; it never parses pane content for agent-specific structure. All IDE
  signals still derive only from PTY bytes and filesystem events.
- **Theme tokens only.** The daemon-unavailable banner, the find/replace
  overlay, the go-to-line affordance, and the terminal search-match highlight
  colors all read `cx.theme()` tokens — no hardcoded hex. The terminal's
  `search_match_bg` / `search_current_bg` must become distinct theme-derived
  colors, not the current alias to `selection_bg`.
- **Reuse gpui-component widgets, never fork.** The banner reuses the `Root`
  notification layer / `Notification`; the window-close guard reuses the same
  `AlertDialog` the per-tab close flow already uses; find/replace reuses the
  `gpui-component` `Input` search facility where it exists. Do not rebuild
  primitives.
- **Crate boundaries are contracts.** `protocol` is the shared language; the
  reason enum and reply variants live there. The daemon maps `BufferError`
  onto them; the app editor renders them. No crate reaches across another's
  internals.
- **No `.unwrap()` in library code.** `crates/daemon` and `crates/protocol`
  keep their existing discipline (the daemon is a binary → `anyhow`;
  `BufferError` is a hand-written enum with `Display`, no `thiserror`
  dependency). `.expect("reason")` only for true invariants.
- **No new dependency.** `alacritty_terminal` already provides the search
  primitives the terminal scrollback search needs; the buffer-error and
  find/replace work adds no crate. A dependency is out of budget unless a
  follow-up spec names it.
- **Read cap is a size ceiling, not a line ceiling.** The buffer read cap is a
  byte-size cap (the file is read whole and shipped in one message today), sized
  in the same spirit as the diff channel's ~2 MB per-side ceiling; the exact
  value is an implementation decision recorded in the decision log.

## Prior decisions

| Decision | Rationale | Date |
|---|---|---|
| Add `OpenError`/`SaveError` as new daemon→client variants rather than overloading `FileContent`/`SaveResult` | Keeps success and failure replies structurally distinct; additive, so existing deserializers keep working; matches the request/response shape of the buffer channel | 2026-07-08 |
| Fold `TooLarge` into the same `OpenError` reason enum rather than a separate message | One reply shape for every open refusal; the editor's failure-render path handles all reasons uniformly, mirroring the diff channel's single `FileDiff` with a tagged payload | 2026-07-08 |
| Daemon-unavailable is surfaced, not auto-fixed | rift's primary user builds locally with `RIFT_DAEMON_BINARY` set; embedding the musl daemon is a packaging phase, so v1.0 makes the degraded mode visible instead of hiding it | 2026-07-08 |
| Window-close guard reuses the per-tab `AlertDialog`, aggregated | The confirm/discard pattern already exists for per-tab close (#354); the window-close case is the same decision at workspace scope — no new dialog primitive | 2026-07-08 |
| Terminal search reuses `alacritty_terminal`'s grid search primitives | The `Term` already backs the render; no new dependency, and the half-wired `search_*` render fields are the intended sink | 2026-07-08 |
| Buffer channel stays path-keyed for correlation (no `NavRequestId`) | Error replies key by `path` exactly like `FileContent`/`SaveResult`; at most one open/save is inflight per path, so path correlation is sufficient (protocol.md) | 2026-07-08 |

## Tracking

The decomposition into steps lives as GitHub issues, not in this file — one
issue per implementable step, grouped under a milestone. This spec owns the
design; the issues own progress. Dependency order: the `protocol` variant lands
first, then the `daemon` mapping + read cap, then the `app` editor rendering;
the daemon-unavailable banner, the window-close guard, editor find/replace, and
terminal search are independent of the protocol chain.

- Milestone: Phase 270 — v1.0 hardening & polish (created when this spec is `READY`)
- Issues: created from this spec once it is `READY` (one per implementable step)

Each issue references this spec path in its body. A PR may only merge if it
closes an issue that traces back here (enforced by the planning gate).

## Verification

- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy --workspace -- -D warnings` passes
- [ ] `cargo test --workspace` passes (including `rift-protocol`'s fingerprint
  test against the re-pinned `PROTOCOL_FINGERPRINT`)
- [ ] Launching daemon mode with `RIFT_DAEMON_BINARY` unset (or a failing
  provision) shows the persistent, dismissible "Daemon unavailable — IDE
  features disabled" indication and never degrades silently.
- [ ] Opening a binary / non-UTF-8 file surfaces a specific error state in the
  editor **immediately** (no 10 s wait); opening a missing / unreadable path
  likewise. A daemon-side test asserts `request_reply` returns the matching
  `OpenError` reason for each `BufferError`.
- [ ] Opening a file above the read cap yields `OpenError { reason: TooLarge }`
  and the editor renders a read-only placeholder; a `buffer::read_file` test
  asserts the cap is enforced (a file one byte over the ceiling is refused).
- [ ] A save that fails on disk surfaces `SaveError` with the reason in the
  editor at once (no `SAVE_TIMEOUT` wait).
- [ ] Quitting with a dirty tab opens the aggregated confirm/discard dialog and
  cancels the close on "Cancel"; discarding or a clean workspace closes
  normally.
- [ ] Editor find/replace finds and replaces within the loaded buffer;
  go-to-line moves the cursor to the requested line.
- [ ] Terminal scrollback search enters a query, highlights matches with the
  distinct theme colors, navigates between them, and marks the current match —
  the `search_current` / `search_match` fields are driven live.
- [ ] `docs/protocol.md` documents the version-8 buffer-error replies and the
  History line is added.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Version bump forces a client+daemon rebuild together (strict equality) | Expected and correct — both build from one repo; the fingerprint test guarantees the bump is not forgotten. Redeploy the daemon on version skew (the existing stale-daemon restart path). |
| The read-size cap value is arbitrary and could truncate legitimately large source files | Size it generously (diff-channel spirit, ~2 MB); record the chosen value in the decision log; `TooLarge` is a read-only placeholder, not data loss — the file is never mangled. |
| Aggregated window-close dialog could deadlock the close if the confirm callback mis-routes tabs | Reuse the proven `close_tab` / `confirm_close_tab` flow; the close handler returns `false` and only proceeds once the user confirms — the existing per-tab pattern already handles index shifts. |
| Terminal search must handle wrapped lines and viewport scroll-to-match correctly | Drive it from `alacritty_terminal`'s own grid search rather than a hand-rolled scan; the render fields already exist, so only state + navigation are new. |
| Banner placement (notification layer vs connection screen) could feel wrong | Both surfaces exist; pick the `Root` notification layer for a persistent dismissible banner (matches the "degraded but running" state), fall back to the connection-screen status banner if the session never reaches the workspace. |

## Decision log

Decisions made during implementation. Claude Code adds entries here as work
progresses.

- 2026-07-08: Spec authored from a gap audit against the v1.0 daily-driver
  surface. Findings verified against `crates/app/src/main.rs`,
  `crates/app/src/workspace.rs`, `crates/app/src/editor.rs`,
  `crates/daemon/src/lib.rs`, `crates/daemon/src/buffer.rs`,
  `crates/protocol/src/lib.rs`, and `crates/terminal/src/pane_view.rs`.
- 2026-07-10: Daemon-unavailable UX superseded by #285 — the legacy fallback
  was removed, so daemon-unavailable now fails cleanly to the connection
  screen instead of degrading to legacy with a Root banner (#619 retired).
  The connection-screen status-banner surface this spec already anticipated
  becomes the sole daemon-unavailable treatment.
