use serde::{Deserialize, Serialize};
use std::time::SystemTime;

mod frame;

pub use frame::{encode_frame, FrameDecoder, FrameError, MAX_FRAME_LEN};

/// Wire protocol version negotiated during the client/daemon handshake.
///
/// Independent of the crate's semver. The policy is **strict equality**
/// (`docs/protocol.md` — Versioning policy): client and daemon must run the
/// exact same version, so ANY change to the message set — a variant added,
/// removed, or renamed; a field added, removed, or renamed; a field's type
/// changed; a serde attribute changed — requires a bump, even when the change
/// is wire-compatible in one direction. The message set is pinned by the
/// fingerprint test beside `PROTOCOL_FINGERPRINT` below, so a message-set
/// change without a bump cannot pass CI.
pub const PROTOCOL_VERSION: u32 = 7;

/// Pinned fingerprint of the protocol message set, checked by the
/// `fingerprint_tests` module: an FNV-1a hash over the serde-visible surface
/// of [`ClientMessage`] and [`DaemonMessage`] — container serde attributes,
/// variant names, field names, and field types, with comments and whitespace
/// ignored. When the message set changes deliberately, bump
/// [`PROTOCOL_VERSION`] above and re-pin this value (the failing test prints
/// the new fingerprint).
#[cfg(test)]
const PROTOCOL_FINGERPRINT: u64 = 0x6313_30a3_4d24_c08d;

/// Messages the client sends to the daemon.
///
/// `Attach` opens this client's own tmux control-mode attach for a named
/// session; `Input`, `ResizePane`, and `TmuxCommand` then drive that attach, and
/// the daemon streams the reverse path back as [`DaemonMessage`] layout and
/// pane-output events. Pane input is opaque bytes — the protocol forwards it to
/// tmux and never interprets it (agent-agnostic).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Open a terminal attach for `session`, carrying the `RIFT_SESSION` knob
    /// end-to-end: the daemon runs attach-or-create (`new-session -A -s
    /// <session>`) per attach, so the dogfooding isolation session
    /// (`RIFT_SESSION=rift-dev`) survives the protocol seam. The daemon answers
    /// with a [`DaemonMessage::LayoutSnapshot`] baseline, then the live stream.
    Attach {
        session: String,
    },
    Input {
        pane_id: u32,
        data: String,
    },
    ResizePane {
        pane_id: u32,
        cols: u16,
        rows: u16,
    },
    TmuxCommand {
        cmd: String,
    },
    /// Request a bounded `capture-pane` of pre-attach scrollback for `pane_id`.
    /// `start`/`end` are tmux line addresses for `-S`/`-E` (`"-"` for the
    /// extreme, a negative number for a history offset); `join` is `-J` (rejoin
    /// soft-wrapped rows). The daemon answers with exactly one
    /// [`DaemonMessage::PaneCapture`] for this pane — the captured bytes, or
    /// empty on a capture error. This is a request/response exchange, separate
    /// from the live `%output` stream and the snapshot↔live-stream contract
    /// (pre-attach scrollback is explicitly outside that contract).
    CapturePane {
        pane_id: u32,
        start: String,
        end: String,
        join: bool,
    },
    /// Ask the daemon to (re-)query `list-keys` and `show-options -A`
    /// (session-resolved — includes values inherited from the global scope,
    /// e.g. a `.tmux.conf` `set -g prefix C-a`) for the mirrored tmux
    /// key-table lookup (`docs/spec-tmux-keytable-mirroring.md`).
    /// The daemon answers with exactly one [`DaemonMessage::KeyTableReply`].
    /// Sent automatically by the daemon's own attach (no client request
    /// needed there — mirroring how the layout query is issued unprompted);
    /// the client sends this explicitly to refresh on an explicit user
    /// trigger, or after dispatching a binding-mutating bound command
    /// (`bind-key`/`unbind-key`/`source-file`, or `set-option` touching
    /// `prefix`/`prefix2`/`repeat-time`).
    QueryKeyTable,
    /// Ask the daemon to (re-)query the host's tmux session list via
    /// `list-sessions` (`docs/spec-session-switch.md`). The daemon answers
    /// with exactly one [`DaemonMessage::SessionListReply`] carrying one
    /// [`SessionEntry`] per session on the server. The daemon also re-issues
    /// this query on its own — coalesced, like the layout re-query — whenever
    /// tmux signals session churn (`%sessions-changed`, `%session-renamed`,
    /// `%client-session-changed`) and pushes the fresh reply unprompted, so
    /// the client's session list stays live without polling; the client sends
    /// this explicitly only for an on-demand refresh (e.g. opening the
    /// session switcher).
    QuerySessionList,
    /// Read request on the buffer channel: pull the current content of the file
    /// at `path` (relative to the worktree root, the same key space as
    /// [`WorktreeEntry::path`]). The daemon answers with exactly one
    /// [`DaemonMessage::FileContent`] for this path — the file's whole UTF-8 text
    /// plus its `mtime`. This is the **first request/response pair in the
    /// protocol** (the worktree, git, and diagnostics paths are all push-only):
    /// file content is **pulled on open**, never broadcast, so it stays off the
    /// content-free worktree structure path.
    OpenFile {
        path: String,
    },
    /// Write request on the buffer channel: replace the file at `path` (relative
    /// to the worktree root) with `content` — the whole new UTF-8 text, no
    /// deltas. `base_mtime` is the `mtime` the editor last read for this path (the
    /// open buffer's base); the daemon compares it against the on-disk `mtime` to
    /// detect a change made under the editor. It answers with one
    /// [`DaemonMessage::SaveResult`] carrying the new `mtime` on success, or one
    /// [`DaemonMessage::SaveConflict`] when the on-disk `mtime` no longer matches
    /// `base_mtime` — a stale base is **rejected, never clobbered**. The write is
    /// atomic on the daemon side (temp + rename). `base_mtime` is the same
    /// `std::time::SystemTime` as [`WorktreeEntry::mtime`], so the base can be
    /// compared across the structure and buffer paths.
    SaveFile {
        path: String,
        content: String,
        base_mtime: SystemTime,
    },
    /// Live-buffer feed for the open file (the disk→buffer source-of-truth shift,
    /// `spec-editor.md` scope cut C). The editor sends the open buffer's whole
    /// current UTF-8 `content` (debounced on edit) so the daemon forwards it to
    /// the language server(s) as a `didChange` — the buffer, not the on-disk
    /// content, becomes the LSP's source of truth for `path`, so an **unsaved**
    /// edit's errors surface without a save first. `path` is relative to the
    /// worktree root (the same key space as [`WorktreeEntry::path`]).
    ///
    /// This is **not** a write: nothing touches disk. It is additive and push-only
    /// (no reply); diagnostics flow back over the existing push-only
    /// [`DaemonMessage::Diagnostics`] path, recomputed against the buffer. While a
    /// path has a live buffer the daemon suppresses disk-driven `didChange` for it
    /// (the buffer owns it); [`ClientMessage::BufferClosed`] reverts it to the
    /// disk-backed baseline.
    BufferChanged {
        path: String,
        content: String,
    },
    /// End the live-buffer feed for `path` (the editor closed the file, opened a
    /// different one, or auto-reloaded). The daemon drops the buffer override and
    /// reverts `path` to the disk-backed baseline — re-reading on-disk content and
    /// pushing it as a `didChange` so diagnostics converge to the on-disk state,
    /// coherently with the pre-buffer behavior. Additive and push-only.
    BufferClosed {
        path: String,
    },
    /// Hover request: ask the daemon what the language server knows about the
    /// symbol at `position` in `path`. `path` is relative to the worktree root.
    /// `id` is a client-assigned [`NavRequestId`] that correlates the response;
    /// a stale or superseded response carrying a different id must be dropped.
    /// The daemon answers with exactly one [`DaemonMessage::HoverResponse`].
    HoverRequest {
        id: NavRequestId,
        path: String,
        position: Position,
    },
    /// Go-to-definition request: ask the daemon where `position` in `path` is
    /// defined. `path` is relative to the worktree root. `id` correlates the
    /// [`DaemonMessage::DefinitionResponse`] reply.
    DefinitionRequest {
        id: NavRequestId,
        path: String,
        position: Position,
    },
    /// Find-references request: ask the daemon for all references to the symbol
    /// at `position` in `path`. `path` is relative to the worktree root. `id`
    /// correlates the [`DaemonMessage::ReferencesResponse`] reply.
    ReferencesRequest {
        id: NavRequestId,
        path: String,
        position: Position,
    },
    /// Document-symbol request: ask the daemon for the outline of the whole
    /// file at `path` (relative to the worktree root). Unlike the other
    /// navigation requests this carries no [`Position`] — the symbol tree
    /// covers the entire document, not a cursor location. `id` correlates the
    /// [`DaemonMessage::DocumentSymbolResponse`] reply, same drop-stale
    /// discipline as `HoverRequest`/`DefinitionRequest`/`ReferencesRequest`.
    DocumentSymbolRequest {
        id: NavRequestId,
        path: String,
    },
    /// Source-control diff request (`docs/spec-source-control.md`): pull a
    /// structured diff of `path`'s current on-disk content against its blob at
    /// HEAD — always worktree-vs-HEAD, regardless of staging state. `path` is
    /// relative to the worktree root (the same key space as
    /// [`WorktreeEntry::path`]). Computed on request, like
    /// [`ClientMessage::OpenFile`], not pushed: a diff is only needed for the
    /// file currently under review. The daemon answers with exactly one
    /// [`DaemonMessage::FileDiff`] for this path — path-keyed request/response,
    /// like the buffer channel (no [`NavRequestId`]: at most one diff is ever
    /// inflight per path).
    RequestDiff {
        path: String,
    },
    /// Stage the whole file at `path` (relative to the worktree root): write its
    /// current worktree content into the index — `git add` semantics (add for
    /// an untracked path; autocrlf filters per gix's pipeline for a tracked
    /// one). The daemon answers with exactly one [`DaemonMessage::GitOpResult`]
    /// carrying [`GitWriteOp::StageFile`]. The resulting status/line-total
    /// change is never echoed in the reply — it arrives through the existing
    /// push-only git recompute ([`DaemonMessage::UpdateGitStatus`] /
    /// [`DaemonMessage::RepoState`]), the protocol's one source of truth for
    /// git state (`docs/spec-source-control-write.md`).
    StageFile {
        path: String,
    },
    /// Unstage the whole file at `path`: restore its index entry from HEAD
    /// (remove from the index for a path newly added with no HEAD entry). The
    /// daemon answers with one [`DaemonMessage::GitOpResult`] carrying
    /// [`GitWriteOp::UnstageFile`]; state converges via the push recompute,
    /// same as [`ClientMessage::StageFile`].
    UnstageFile {
        path: String,
    },
    /// Stage exactly one hunk of `path`'s worktree-vs-HEAD diff, identified by
    /// `hunk_id` — the [`hunk_fingerprint`] of the [`DiffHunk`] the client
    /// last received from [`ClientMessage::RequestDiff`]. The daemon
    /// recomputes the file's current hunks fresh and verifies `hunk_id`
    /// matches one of them before applying: a stale id (the worktree changed
    /// since the diff was pushed) or a content-changed id (same shape,
    /// different text) is rejected with a clean error, never fuzzily applied.
    /// Application targets the index via decompose-and-reapply against the
    /// already-staged subset (`docs/spec-source-control-write.md`); a
    /// divergent index (external `git add`, staged-then-edited) is also
    /// rejected, naming file-level staging as the fallback. Answered with one
    /// [`DaemonMessage::GitOpResult`] carrying [`GitWriteOp::StageHunk`].
    StageHunk {
        path: String,
        hunk_id: u64,
    },
    /// Discard the worktree edits to `path`: restore its worktree content from
    /// the index — checkout-file semantics (unstaged edits reverted, staged
    /// content kept; an untracked path, absent from the index, is removed).
    /// **Destructive.** The client gates this behind an explicit confirm
    /// dialog (the #420 pattern) and never batches it — one request per file.
    /// Answered with one [`DaemonMessage::GitOpResult`] carrying
    /// [`GitWriteOp::DiscardFile`].
    DiscardFile {
        path: String,
    },
    /// Commit the currently staged index: build a tree from it (gix
    /// `tree-editor`), commit with `parents = [HEAD]`, author/committer from
    /// the repo's git config. An empty `message` or a NOTHING-STAGED index
    /// (index tree equals HEAD tree — an index is never literally empty in a
    /// non-empty repo) is rejected with a clean error, never a partial
    /// commit. A transient `index.lock` (a live agent writing concurrently)
    /// gets one bounded retry before erroring. Answered with one
    /// [`DaemonMessage::GitOpResult`] carrying [`GitWriteOp::Commit`] — not
    /// path-keyed, unlike the other write ops.
    Commit {
        message: String,
    },
    Hello {
        version: u32,
    },
}

/// Messages the daemon sends to the client.
///
/// ## Terminal snapshot ↔ live-stream consistency contract
///
/// On [`ClientMessage::Attach`] the daemon opens this client's own tmux
/// control-mode attach and sends exactly one [`LayoutSnapshot`] — the complete
/// window/pane layout as of the attach instant — and from that instant streams
/// the live notifications: [`LayoutUpdate`] for every structural change and
/// [`PaneOutput`] for pane bytes. The seam between the snapshot and the live
/// stream is **gap-free and duplicate-free**:
///
/// - **No gap**: the daemon subscribes to tmux's notification stream before it
///   reads the snapshot, so every change at or after the snapshot instant
///   appears in the live stream; none is lost in the handover.
/// - **No duplicate**: the snapshot is the baseline state, not a replay — no
///   layout change already reflected in it is re-sent as a live event.
///   [`LayoutUpdate`] carries the full latest layout (replace semantics), so even
///   a coalesced or reordered change converges without double-applying.
///
/// On reconnect the daemon reattaches and sends a fresh [`LayoutSnapshot`]; the
/// client resets its layout to it and resumes from the new baseline — tmux
/// remains the session persistence, so no terminal state is lost. Pane scrollback
/// that predates the attach is fetched separately via `capture-pane` (command
/// emission) and is outside this contract — it governs only the seam between the
/// attach snapshot and the live `%output` stream.
///
/// [`LayoutSnapshot`]: DaemonMessage::LayoutSnapshot
/// [`LayoutUpdate`]: DaemonMessage::LayoutUpdate
/// [`PaneOutput`]: DaemonMessage::PaneOutput
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Raw terminal bytes for one pane, in stream order. Per the VTE-location
    /// spike verdict the daemon forwards bytes, not cells: the client feeds them
    /// straight into its `alacritty_terminal::Term`, so the payload is an opaque
    /// ANSI byte run the protocol never interprets (agent-agnostic). `pane_id` is
    /// tmux's `%<n>` pane id as an integer.
    PaneOutput {
        pane_id: u32,
        bytes: Vec<u8>,
    },
    /// The reply to a [`ClientMessage::CapturePane`]: the captured pre-attach
    /// scrollback for `pane_id` as opaque bytes (tmux-decoded, ANSI included via
    /// `capture-pane -e`), or empty on a capture error so the client can clear
    /// its in-flight flag and retry. One per request, correlated by `pane_id`.
    /// Distinct from [`PaneOutput`] — the client routes this to its scrollback
    /// history, not the live `Term`.
    ///
    /// [`PaneOutput`]: DaemonMessage::PaneOutput
    PaneCapture {
        pane_id: u32,
        bytes: Vec<u8>,
    },
    /// The reply to a [`ClientMessage::QueryKeyTable`]: the raw `list-keys` and
    /// `show-options -A` output (newline-joined, tmux-decoded — the control-mode
    /// decode already run by the daemon's command-reply path), for the client
    /// to parse with `rift_terminal::keytable::{parse_list_keys, parse_options}`
    /// into the mirrored key-table lookup. The daemon never interprets this
    /// text itself — it is tmux's own config, not pane content. Sent once per
    /// attach (issued unprompted by the daemon's `Attach`, mirroring the
    /// layout query) and again for every later [`ClientMessage::QueryKeyTable`].
    KeyTableReply {
        list_keys: String,
        options: String,
    },
    /// The reply to a [`ClientMessage::QuerySessionList`]: every tmux session
    /// on the server, parsed daemon-side from `list-sessions` (a tab-separated
    /// format with the session name last, the same convention as the daemon's
    /// layout query, so names with spaces or tabs survive). Also pushed
    /// unprompted whenever tmux signals session churn (`%sessions-changed`,
    /// `%session-renamed`, `%client-session-changed`), so the client replaces
    /// its whole session list on every arrival — replace semantics, like
    /// [`LayoutUpdate`]. Which session THIS client is attached to is not
    /// carried here: the `session` string on [`LayoutSnapshot`] /
    /// [`LayoutUpdate`] already owns that (the truthful-indicator contract).
    ///
    /// [`LayoutUpdate`]: DaemonMessage::LayoutUpdate
    /// [`LayoutSnapshot`]: DaemonMessage::LayoutSnapshot
    SessionListReply {
        sessions: Vec<SessionEntry>,
    },
    /// The complete window/pane layout for `session`, sent once per attach as the
    /// baseline of the consistency contract (see the type-level docs). The client
    /// replaces its entire layout model with this — on first attach and again on
    /// every reconnect.
    LayoutSnapshot {
        session: String,
        windows: Vec<WindowLayout>,
    },
    /// The full latest window/pane layout for `session` after a structural change
    /// (window add/close, pane split/resize, active-window switch). Carries the
    /// whole layout, not a delta, so applying it is an idempotent replace.
    LayoutUpdate {
        session: String,
        windows: Vec<WindowLayout>,
    },
    /// The terminal path for `session` went down: the daemon's tmux control
    /// attach ended — the tmux server exited (`%exit`) or the control-mode
    /// child died. The client's pane streams for this attach stop; it may
    /// re-`attach` to resume (tmux is the session persistence, so a still-live
    /// server reattaches with a fresh snapshot). `reason` is tmux's `%exit`
    /// message when it supplied one. This is a terminal-path-down signal, never
    /// a daemon failure — the daemon keeps serving its other clients.
    TerminalExit {
        session: String,
        reason: Option<String>,
    },
    /// Initial worktree contents, sent on connect. A large tree is split across
    /// several `WorktreeSnapshot` messages: the client appends `entries` from
    /// each in order and holds the complete tree once it receives the message
    /// with `final_chunk` set. `root` is the absolute daemon-side project root;
    /// entry paths are relative to it.
    WorktreeSnapshot {
        root: String,
        entries: Vec<WorktreeEntry>,
        final_chunk: bool,
    },
    /// Incremental worktree change since the last snapshot or update. The client
    /// upserts `added` and `changed` by path and drops `removed` paths. A move is
    /// modeled as the old path in `removed` plus the new path in `added` (rename
    /// events are not trusted; moves are reconciled through the snapshot diff).
    UpdateWorktree {
        added: Vec<WorktreeEntry>,
        changed: Vec<WorktreeEntry>,
        removed: Vec<String>,
    },
    /// Incremental git-status change decorating the worktree entries. The
    /// client upserts the status of every `changed` path and drops the
    /// decoration for every `cleared` path (the file returned to clean / was
    /// removed from git's view). Keyed by path relative to the worktree root —
    /// the same key space as [`WorktreeEntry::path`]; ignored paths never
    /// appear. The daemon diffs its previous git state against the new one to
    /// produce these deltas, mirroring the `UpdateWorktree` pattern. A status
    /// arriving for a path the client has not yet added is reconciled
    /// client-side (the worktree snapshot is the source of truth; see #135).
    UpdateGitStatus {
        changed: Vec<GitStatusEntry>,
        cleared: Vec<String>,
    },
    /// Repo-level git state for the watched worktree, recomputed on `.git/`
    /// changes (commit, branch switch, staging) and on a worktree change — the
    /// line totals ride the same debounced git-status recompute as the rest
    /// of this message, never a dedicated timer. `branch` is `None` when HEAD
    /// is detached; `ahead_behind` is `None` when the current branch has no
    /// upstream. `lines_added`/`lines_removed` are the working-tree line
    /// totals — `git diff HEAD --numstat` semantics (worktree content vs
    /// HEAD, regardless of staging) plus untracked text file additions; a
    /// rename with no content change contributes `0`/`0` (it diffs against
    /// its rewrite source blob, not a nonexistent HEAD entry at the new
    /// path). Both are `0` on a clean worktree. Produced and streamed by
    /// Phase 3.3, but not wired into the statusbar by it (the #18 statusbar
    /// swap is a later step).
    RepoState {
        branch: Option<String>,
        ahead_behind: Option<AheadBehind>,
        lines_added: u32,
        lines_removed: u32,
    },
    /// The complete current diagnostic set one language server reports for one
    /// file, replacing whatever that server last reported for it. Keyed by
    /// `path` relative to the worktree root (the same key space as
    /// [`WorktreeEntry::path`]) and by `server` — the daemon-assigned id of the
    /// publishing language server. The daemon translates each server
    /// `publishDiagnostics` notification into one of these messages, mirroring
    /// LSP's full-set-per-`(file, server)` replace semantics: `items` is the
    /// authoritative set, so an empty `items` clears that server's diagnostics
    /// for the file while leaving every other server's set for it intact. This
    /// per-server keying lets a linter and a type-checker aggregate on the same
    /// file without one clobbering the other. Push-only — the client is a pure
    /// consumer and never requests diagnostics. A message arriving for a path
    /// the client has not yet added is reconciled client-side (the worktree
    /// snapshot is the source of truth; see #135).
    Diagnostics {
        path: String,
        server: String,
        items: Vec<Diagnostic>,
    },
    /// A language server's lifecycle transition, keyed by `server` — its
    /// stable name (e.g. `"rust-analyzer"`), NOT the per-spawn server id
    /// [`Diagnostics`](DaemonMessage::Diagnostics) keys by. A restart mints a
    /// fresh internal id, but the status-line health dot asks "is my
    /// rust-analyzer OK", which is name-scoped, not spawn-scoped. Emitted by
    /// the daemon's LSP registry around its observe cycle: `starting` when a
    /// (re)start is triggered, `running` once the server has completed its
    /// `initialize` handshake, `crashed` once a dead instance is pruned
    /// (detected on the next observe after the server exits) or a (re)start
    /// attempt fails. There is no `stopped` state — a server the daemon has
    /// observed is never deliberately stopped while a client is attached.
    /// Push-only, and replayed once per known server behind
    /// [`Welcome`](DaemonMessage::Welcome) so a (re)attaching client sees
    /// current health without waiting for the next transition.
    LspStatus {
        server: String,
        state: LspServerState,
    },
    /// The reply to a [`ClientMessage::OpenFile`]: the whole current UTF-8
    /// `content` of the file at `path` (relative to the worktree root) plus its
    /// `mtime`. The `mtime` is the same `std::time::SystemTime` as
    /// [`WorktreeEntry::mtime`] — the editor keeps it as the open buffer's base
    /// and hands it back as [`ClientMessage::SaveFile`]'s `base_mtime`, so a save
    /// can be checked against the on-disk version across the structure and buffer
    /// paths. This is the only daemon message that carries file content; the
    /// worktree, git, and diagnostics paths stay content-free.
    FileContent {
        path: String,
        content: String,
        mtime: SystemTime,
    },
    /// The success reply to a [`ClientMessage::SaveFile`]: the write landed and
    /// `path` (relative to the worktree root) now has the new on-disk `mtime`. The
    /// editor adopts this as the buffer's new base `mtime` for the next save. Same
    /// `std::time::SystemTime` as [`WorktreeEntry::mtime`].
    SaveResult {
        path: String,
        mtime: SystemTime,
    },
    /// The conflict reply to a [`ClientMessage::SaveFile`]: the file at `path`
    /// (relative to the worktree root) changed on disk since the editor read it —
    /// its `disk_mtime` no longer matches the save's `base_mtime` — so the daemon
    /// **rejected** the write rather than clobber the newer on-disk version. The
    /// editor surfaces the conflict; `disk_mtime` is the current on-disk value
    /// (the same `std::time::SystemTime` as [`WorktreeEntry::mtime`]), letting the
    /// editor re-open from disk to rebase. No write happened.
    SaveConflict {
        path: String,
        disk_mtime: SystemTime,
    },
    /// Reply to [`ClientMessage::HoverRequest`]. `id` echoes the request's
    /// [`NavRequestId`] so the client can match and drop superseded responses.
    /// `content` is the server's markdown-rendered hover text; `None` when the
    /// server has nothing to say about that position (silent no-op for the UI).
    /// `content` serializes as `null` on the wire — **not omitted** — so the
    /// client can distinguish "server responded with nothing" from "response not
    /// yet received". This is deliberate and differs from the other optional
    /// fields on navigation types (`line_preview`, `range`) which are omitted
    /// when absent.
    HoverResponse {
        id: NavRequestId,
        content: Option<HoverContent>,
    },
    /// Reply to [`ClientMessage::DefinitionRequest`]. `id` echoes the request's
    /// [`NavRequestId`]. `targets` is empty when the server found no definition
    /// (silent no-op); more than one target (e.g. trait method impls in Rust)
    /// is surfaced in the same jump-list the references path uses.
    DefinitionResponse {
        id: NavRequestId,
        targets: Vec<NavLocation>,
    },
    /// Reply to [`ClientMessage::ReferencesRequest`]. `id` echoes the request's
    /// [`NavRequestId`]. `locations` is empty when the server found no
    /// references (silent no-op). Each entry carries path, range, and a
    /// one-line preview for the jump-list.
    ReferencesResponse {
        id: NavRequestId,
        locations: Vec<NavLocation>,
    },
    /// Reply to [`ClientMessage::DocumentSymbolRequest`]. `id` echoes the
    /// request's [`NavRequestId`]. `symbols` is the file's outline flattened
    /// to a depth-tagged list (see [`DocumentSymbolEntry`]); empty when the
    /// server has no symbols for the file (silent no-op, not an error).
    DocumentSymbolResponse {
        id: NavRequestId,
        symbols: Vec<DocumentSymbolEntry>,
    },
    /// The reply to a [`ClientMessage::RequestDiff`]: `path`'s structured diff
    /// against HEAD, or a [`FileDiffPayload`] sentinel when the daemon cannot
    /// produce one (binary content on either side, or a diff exceeding the
    /// size ceiling — see [`FileDiffPayload::TooLarge`]). `path` is relative to
    /// the worktree root (the same key space as [`WorktreeEntry::path`]).
    FileDiff {
        path: String,
        diff: FileDiffPayload,
    },
    /// The reply to every source-control write request
    /// ([`ClientMessage::StageFile`], [`ClientMessage::UnstageFile`],
    /// [`ClientMessage::StageHunk`], [`ClientMessage::DiscardFile`],
    /// [`ClientMessage::Commit`]): whether `op` succeeded, or `error` — a
    /// human-readable reason — when it did not. This is the **only** signal
    /// the write path returns over the wire; the resulting state change
    /// (staged/unstaged status, working-tree line totals, branch/
    /// ahead-behind) is never echoed here — it arrives through the existing
    /// push-only git recompute ([`UpdateGitStatus`](DaemonMessage::UpdateGitStatus)
    /// / [`RepoState`](DaemonMessage::RepoState)), keeping one source of
    /// truth for git state instead of two (`docs/spec-source-control-write.md`).
    /// `error` is omitted on success.
    GitOpResult {
        op: GitWriteOp,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Welcome {
        version: u32,
    },
}

/// One tmux window inside a [`DaemonMessage::LayoutSnapshot`] /
/// [`DaemonMessage::LayoutUpdate`]: its identity, title, active flag, and the
/// panes it holds. `window_id` is tmux's `@<n>` window id as an integer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowLayout {
    pub window_id: u32,
    /// tmux's real `#{window_index}` (the tab number `list-windows`/`select-window
    /// -t :N` uses), not a position derived from array order — closing a window
    /// leaves a gap in this numbering unless `renumber-windows` is set, and the
    /// client must show that same gap (#495). Defaults on deserialize so a layout
    /// emitted before this field existed still parses (additive change, same
    /// tolerance as `PaneLayout::current_path`, #442).
    #[serde(default)]
    pub window_index: u32,
    pub name: String,
    /// Whether this is the session's active (currently selected) window.
    pub active: bool,
    pub panes: Vec<PaneLayout>,
}

/// One tmux pane's identity, active flag, and geometry within its window.
///
/// Geometry is in terminal cells, matching tmux's layout coordinates: `left` and
/// `top` are the pane's offset from the window's top-left corner, `width` and
/// `height` its size. `pane_id` is tmux's `%<n>` pane id as an integer — the same
/// id space as the `pane_id` in [`DaemonMessage::PaneOutput`] and
/// [`ClientMessage::Input`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub pane_id: u32,
    /// Whether this is the window's active pane.
    pub active: bool,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    /// The pane's current working directory (tmux `#{pane_current_path}`);
    /// empty when unknown. Defaults on deserialize so a layout emitted before
    /// this field existed still parses (additive change, #442).
    #[serde(default)]
    pub current_path: String,
    /// The pane's current foreground command name (tmux
    /// `#{pane_current_command}`); empty when unknown. Same deserialize
    /// tolerance as `current_path`.
    #[serde(default)]
    pub current_command: String,
    /// Whether the pane's foreground command is its shell — tmux's own
    /// `#{==:#{pane_current_command},#{b:default-shell}}` comparison against
    /// the session's `default-shell` option, evaluated server-side so the
    /// client never carries a shell name list or process taxonomy
    /// (agent-agnostic, #510). Defaults to `false` on deserialize so a layout
    /// emitted before this field existed still parses (additive change, same
    /// tolerance as `current_path`).
    #[serde(default)]
    pub is_shell: bool,
}

/// One tmux session inside a [`DaemonMessage::SessionListReply`]: its server
/// identity, name, window count, and whether any client is attached to it.
///
/// `id` is tmux's `$<n>` session id as an integer — the stable key across
/// renames (`#{session_id}`); `name` is the current session name
/// (`#{session_name}`). `windows` is the session's window count
/// (`#{session_windows}`); `attached` is whether at least one client is
/// attached (`#{session_attached}` reports a count; the daemon folds it to a
/// bool — the picker only marks attached sessions, never counts clients).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: u32,
    pub name: String,
    pub windows: u32,
    /// Whether at least one client is attached to this session.
    pub attached: bool,
}

/// A single worktree entry, keyed by its path relative to the worktree root.
///
/// `mtime` is the file's last-modification time. It is what lets the daemon's
/// snapshot diff observe a content modification — which leaves `path`, `kind`,
/// and `ignored` unchanged — and surface it as a `changed` entry the client can
/// upsert. A `changed` entry always carries the full record, not just the path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub kind: EntryKind,
    pub ignored: bool,
    pub mtime: SystemTime,
}

/// Whether a [`WorktreeEntry`] is a regular file or a directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
}

/// One side's porcelain status code for a path.
///
/// Git models each path as an **index** (staged) component and a **worktree**
/// (unstaged) component — the `XY` pair of `git status --porcelain`.
/// [`GitEntryStatus`] carries both. Most codes can appear on either side;
/// [`GitStatusCode::Untracked`] is only ever a worktree-side code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStatusCode {
    /// No change on this side.
    Unmodified,
    Modified,
    /// The file's type changed (e.g. regular file <-> symlink).
    TypeChange,
    Added,
    Deleted,
    Renamed,
    Copied,
    /// Updated but unmerged — a merge conflict.
    Unmerged,
    /// Present in the worktree but not tracked by git.
    Untracked,
}

/// The git status of one path: its index (staged) and worktree (unstaged)
/// components, mirroring git's porcelain `XY`.
///
/// Examples: an untracked file is `{ index: Unmodified, worktree: Untracked }`;
/// a file staged and then left alone is `{ index: Modified, worktree:
/// Unmodified }`; a tracked file edited but not staged is `{ index:
/// Unmodified, worktree: Modified }`. A clean (unmodified on both sides) path
/// carries no status at all — it is never sent, and a path returning to clean
/// is reported via `cleared` in [`DaemonMessage::UpdateGitStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitEntryStatus {
    pub index: GitStatusCode,
    pub worktree: GitStatusCode,
}

/// A path paired with its git status, keyed by path relative to the worktree
/// root — the same key space as [`WorktreeEntry::path`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub status: GitEntryStatus,
}

/// Ahead/behind commit counts of the current branch versus its upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AheadBehind {
    pub ahead: u32,
    pub behind: u32,
}

/// One diagnostic a language server reports for a file: a source span plus the
/// human-readable problem.
///
/// rift's own type, deliberately independent of `lsp-types` — the daemon
/// translates each LSP diagnostic into this so the shared protocol stays
/// dependency-light and serialization-agnostic (it may migrate to MessagePack),
/// mirroring how worktree and git messages are rift types, not library types.
/// `source` (the producing tool, e.g. `"rustc"`) and `code` (the rule / error
/// identifier) are `None` when the server omits them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: DiagnosticSeverity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// How serious a [`Diagnostic`] is, mirroring LSP's four severities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// A language server's lifecycle state, carried by
/// [`DaemonMessage::LspStatus`]. No `Stopped` variant: a server the daemon
/// has observed is never deliberately stopped while a client is attached —
/// only started, running, or crashed (and possibly restarted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspServerState {
    /// A (re)start was just triggered; the `initialize` handshake has not
    /// completed yet.
    Starting,
    /// The server is alive and has completed its `initialize` handshake.
    Running,
    /// The server's main loop ended (exit, crash, or a transport failure)
    /// and it has not been restarted yet.
    Crashed,
}

/// A half-open span within a file, from `start` (inclusive) to `end`
/// (exclusive), mirroring LSP ranges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// A zero-based line / character offset within a file.
///
/// `character` is a **UTF-8 character offset** (number of Unicode scalar values
/// from the start of the line, not bytes). This is rift's canonical wire
/// encoding; `crates/lsp` translates to and from the UTF-16 code-unit offsets
/// that LSP servers negotiate before sending or receiving positions
/// (`docs/spec-lsp-navigation.md` §Constraints — "the client and protocol
/// speak only rift's own position type").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// An opaque monotonically-increasing request identifier for the navigation
/// request/response family (hover, go-to-definition, find-references,
/// document symbols).
///
/// Every navigation [`ClientMessage`] variant carries one `NavRequestId`; the
/// matching [`DaemonMessage`] reply echoes it unchanged so the client can:
///
/// 1. **Correlate** the response to the inflight request that issued it.
/// 2. **Drop stale responses** — when the user has moved on (new file, new
///    cursor position, or the request was superseded by a later one), the client
///    compares the echoed `id` against its current inflight id and silently
///    discards the response if they differ, so a slow server can never land its
///    result on the wrong file or position.
///
/// The buffer channel correlates by `path`, which is insufficient here because
/// concurrent requests can target the **same file at different positions**;
/// this explicit id is the minimal correct mechanism. The client is responsible
/// for generating ids; a `u64` counter starting at `0` is the canonical choice.
///
/// This is the protocol's **request-id correlation convention**, established by
/// the navigation family (Phase 5 `spec-lsp-navigation.md`). Whether the buffer
/// channel retroactively adopts it is evaluated separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NavRequestId(pub u64);

/// One navigation target (a definition or reference location) returned by the
/// daemon in response to a navigation request.
///
/// `path` is relative to the worktree root — the same key space as
/// [`WorktreeEntry::path`] — unless the target lives **outside the worktree
/// root** (e.g. a stdlib or registry dependency), in which case `path` is the
/// absolute daemon-side path and `out_of_root` is `true`. The client opens
/// out-of-root targets read-only (no save path). `line_preview` is the
/// zero-indexed source line at `range.start.line`, trimmed, used for jump-list
/// display; it is `None` when the daemon cannot read the file (e.g. permissions
/// or the file does not exist on disk — a degenerate server response, never a
/// crash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavLocation {
    pub path: String,
    pub range: Range,
    /// `true` when `path` is absolute and lives outside the worktree root; the
    /// client must open this target read-only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub out_of_root: bool,
    /// One trimmed source line at `range.start.line`, for jump-list previews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_preview: Option<String>,
}

/// Hover content returned by the daemon in response to a
/// [`ClientMessage::HoverRequest`].
///
/// `markdown` is the server's hover text in markdown format (LSP
/// `MarkupContent` with kind `markdown` or `plaintext`, both forwarded as-is
/// for the client's markdown renderer). `range` is the symbol span the hover
/// covers; when present the client may highlight it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoverContent {
    pub markdown: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// One symbol in a file's outline, returned by the daemon in response to a
/// [`ClientMessage::DocumentSymbolRequest`]. Serves both the editor's
/// breadcrumb (enclosing symbol at the cursor) and the outline panel
/// (`docs/spec-editor-chrome.md`).
///
/// LSP servers report symbols either as a hierarchical tree (`DocumentSymbol`,
/// nested via `children`) or a flat list (`SymbolInformation`, no nesting);
/// `crates/lsp` normalizes both shapes into this single flat, depth-tagged
/// list so `crates/protocol` never sees the two LSP variants. `depth` is the
/// symbol's nesting depth (`0` = top-level), reconstructed from the LSP
/// `children` tree by a pre-order flatten; a flat-shape (`SymbolInformation`)
/// response carries no hierarchy, so every entry there is `depth` `0`.
/// `selection_range` is the sub-span that should be selected/revealed when the
/// symbol is picked (e.g. just the identifier) and is contained by `range`;
/// a flat-shape response has no independent selection range, so
/// `selection_range` falls back to `range`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSymbolEntry {
    pub name: String,
    pub kind: SymbolKind,
    pub range: Range,
    pub selection_range: Range,
    pub depth: u32,
}

/// A symbol's kind, mirroring LSP's `SymbolKind` enum. rift's own type,
/// deliberately independent of `lsp-types` — the same precedent as
/// [`Diagnostic`]/[`DiagnosticSeverity`] — so `crates/protocol` stays
/// dependency-light and serialization-agnostic; `crates/lsp` translates the
/// LSP integer enum into this on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Method,
    Property,
    Field,
    Constructor,
    Enum,
    Interface,
    Function,
    Variable,
    Constant,
    String,
    Number,
    Boolean,
    Array,
    Object,
    Key,
    Null,
    EnumMember,
    Struct,
    Event,
    Operator,
    TypeParameter,
}

/// The payload of a [`DaemonMessage::FileDiff`]: either a structured line diff
/// against HEAD, or a sentinel for content the daemon cannot diff structurally
/// (`docs/spec-source-control.md`). Tagged by `kind` on the wire so the client
/// can match on the payload shape without probing for field presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileDiffPayload {
    /// A unified-diff-style line diff against HEAD. Empty `hunks` means the
    /// worktree content is identical to HEAD.
    Hunks { hunks: Vec<DiffHunk> },
    /// Either side (the HEAD blob or the worktree content) is binary — no
    /// line diff is produced.
    Binary,
    /// The diff exceeds the daemon's size ceiling (~20k changed lines or
    /// ~2MB per side, pinned in the diff-compute implementation) — too large
    /// to stream as a structured diff.
    TooLarge,
}

/// A contiguous run of unified-diff lines within a [`FileDiffPayload::Hunks`],
/// addressed against both the old (HEAD) and new (worktree) line numbering.
/// `old_start`/`new_start` are 1-based, matching unified-diff / `git diff`
/// hunk headers (`@@ -old_start,old_len +new_start,new_len @@`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

/// One line of a [`DiffHunk`]: its role plus content, with the line
/// terminator stripped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

/// A [`DiffLine`]'s role within its hunk, mirroring unified-diff's
/// context/add/remove line prefixes (` `/`+`/`-`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    /// Present on both sides, shown for surrounding context.
    Context,
    /// Present only on the new (worktree) side.
    Add,
    /// Present only on the old (HEAD) side.
    Remove,
}

/// Which write operation a [`DaemonMessage::GitOpResult`] answers, echoing
/// enough of the originating [`ClientMessage`] to correlate the reply to it —
/// `path` for the file-level ops, `path` plus `hunk_id` for hunk staging, no
/// fields for [`GitWriteOp::Commit`] (a repo-level op, not path-keyed).
/// Tagged by `kind` on the wire, mirroring [`FileDiffPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitWriteOp {
    StageFile { path: String },
    UnstageFile { path: String },
    StageHunk { path: String, hunk_id: u64 },
    DiscardFile { path: String },
    Commit,
}

/// The FNV-1a fingerprint of a [`DiffHunk`], serving as `hunk_id` on
/// [`ClientMessage::StageHunk`] — the deploy.rs fingerprint pattern
/// (`crates/ssh/src/deploy.rs::binary_fingerprint`; hand-rolled rather than a
/// hashing crate dependency for one function), hashed over the hunk's header
/// numbers (`old_start`/`old_len`/`new_start`/`new_len`, little-endian) and
/// every line's kind plus content, each line terminated by a delimiter byte
/// so no ambiguity arises between adjacent lines. A same-shape edit (an
/// identical header, different line text) therefore yields a different id.
/// Not cryptographic — only stability and low collision odds across a
/// single file's hunks matter, matching the deploy fingerprint's own
/// tradeoff.
///
/// The client computes this from the [`DiffHunk`] it already holds (the last
/// [`ClientMessage::RequestDiff`] reply) when the user stages a hunk; the
/// daemon recomputes the file's current worktree-vs-HEAD hunks and verifies
/// the id matches one of them before applying — a stale or content-changed
/// id is rejected, never fuzzily applied (`docs/spec-source-control-write.md`).
pub fn hunk_fingerprint(hunk: &DiffHunk) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x100_0000_01b3;
    const LINE_DELIMITER: u8 = 0x0a;

    fn step(hash: &mut u64, byte: u8) {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }

    let mut hash = FNV_OFFSET;
    for n in [hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len] {
        for byte in n.to_le_bytes() {
            step(&mut hash, byte);
        }
    }
    for line in &hunk.lines {
        let kind_tag: u8 = match line.kind {
            DiffLineKind::Context => 0,
            DiffLineKind::Add => 1,
            DiffLineKind::Remove => 2,
        };
        step(&mut hash, kind_tag);
        for byte in line.content.as_bytes() {
            step(&mut hash, *byte);
        }
        step(&mut hash, LINE_DELIMITER);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_attach_roundtrip_carries_session_name() {
        // The attach request is the seam that carries `RIFT_SESSION` end-to-end,
        // so the session name must survive serialization untouched.
        let msg = ClientMessage::Attach {
            session: "rift-dev".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize Attach");
        assert_eq!(json, r#"{"type":"attach","session":"rift-dev"}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize Attach");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::Attach { session } => assert_eq!(session, "rift-dev"),
            other => panic!("expected Attach, got {other:?}"),
        }
    }

    #[test]
    fn test_input_roundtrip_preserves_pane_and_data() {
        let msg = ClientMessage::Input {
            pane_id: 3,
            data: "ls\n".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize Input");
        assert!(json.contains(r#""type":"input""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize Input"),
            msg
        );
    }

    #[test]
    fn test_resize_pane_roundtrip_preserves_dimensions() {
        let msg = ClientMessage::ResizePane {
            pane_id: 7,
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&msg).expect("serialize ResizePane");
        assert!(json.contains(r#""type":"resize_pane""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize ResizePane"),
            msg
        );
    }

    #[test]
    fn test_tmux_command_roundtrip_preserves_cmd() {
        let msg = ClientMessage::TmuxCommand {
            cmd: "split-window -h".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize TmuxCommand");
        assert!(json.contains(r#""type":"tmux_command""#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize TmuxCommand"),
            msg
        );
    }

    #[test]
    fn test_capture_pane_roundtrip_preserves_range_and_join() {
        let msg = ClientMessage::CapturePane {
            pane_id: 4,
            start: "-".to_owned(),
            end: "-128".to_owned(),
            join: true,
        };
        let json = serde_json::to_string(&msg).expect("serialize CapturePane");
        assert!(json.contains(r#""type":"capture_pane""#));
        assert!(json.contains(r#""start":"-""#));
        assert!(json.contains(r#""end":"-128""#));
        assert!(json.contains(r#""join":true"#));
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&json).expect("deserialize CapturePane"),
            msg
        );
    }

    #[test]
    fn test_client_message_unknown_type_is_rejected() {
        // An unknown tag fails loudly rather than being silently misread, so a
        // future client message a daemon does not know is not mistaken for a
        // known one.
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"frobnicate"}"#);
        assert!(
            err.is_err(),
            "unknown client message type must not deserialize"
        );
    }

    #[test]
    fn test_attach_missing_session_field_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"attach"}"#);
        assert!(
            err.is_err(),
            "attach without a session must not deserialize"
        );
    }

    #[test]
    fn test_pane_output_roundtrip_carries_bytes_field() {
        // The spike verdict pins pane output as raw bytes, not cells: the wire
        // field is `bytes` and round-trips the exact byte run (control bytes
        // included).
        let msg = DaemonMessage::PaneOutput {
            pane_id: 2,
            bytes: vec![0x1b, b'[', b'1', b'm', b'h', b'i'],
        };
        let json = serde_json::to_string(&msg).expect("serialize PaneOutput");
        assert!(json.contains(r#""type":"pane_output""#));
        assert!(json.contains(r#""bytes":[27,91,49,109,104,105]"#));
        assert!(
            !json.contains("cells"),
            "pane output must not carry a cells field"
        );

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize PaneOutput");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::PaneOutput { pane_id, bytes } => {
                assert_eq!(pane_id, 2);
                assert_eq!(bytes, vec![0x1b, b'[', b'1', b'm', b'h', b'i']);
            }
            other => panic!("expected PaneOutput, got {other:?}"),
        }
    }

    #[test]
    fn test_pane_capture_roundtrip_carries_bytes() {
        // The capture reply carries opaque bytes (ANSI included), and an empty
        // capture (error / no history) round-trips as an empty list, not absent.
        let msg = DaemonMessage::PaneCapture {
            pane_id: 6,
            bytes: vec![0x1b, b'[', b'3', b'1', b'm', b'h', b'i'],
        };
        let json = serde_json::to_string(&msg).expect("serialize PaneCapture");
        assert!(json.contains(r#""type":"pane_capture""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize PaneCapture"),
            msg
        );

        let empty = DaemonMessage::PaneCapture {
            pane_id: 6,
            bytes: vec![],
        };
        let json = serde_json::to_string(&empty).expect("serialize empty PaneCapture");
        assert!(json.contains(r#""bytes":[]"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize empty PaneCapture"),
            empty
        );
    }

    fn sample_layout() -> Vec<WindowLayout> {
        vec![
            WindowLayout {
                window_id: 1,
                window_index: 0,
                name: "editor".to_owned(),
                active: true,
                panes: vec![
                    PaneLayout {
                        pane_id: 0,
                        active: true,
                        left: 0,
                        top: 0,
                        width: 80,
                        height: 24,
                        current_path: "/home/dev/my project".to_owned(),
                        current_command: "bash".to_owned(),
                        is_shell: true,
                    },
                    PaneLayout {
                        pane_id: 1,
                        active: false,
                        left: 81,
                        top: 0,
                        width: 79,
                        height: 24,
                        current_path: String::new(),
                        current_command: String::new(),
                        is_shell: false,
                    },
                ],
            },
            WindowLayout {
                window_id: 2,
                // Deliberately non-contiguous with the first window's index,
                // matching a real gap left by a closed window (#495) — not 1.
                window_index: 2,
                name: "logs".to_owned(),
                active: false,
                panes: vec![PaneLayout {
                    pane_id: 2,
                    active: true,
                    left: 0,
                    top: 0,
                    width: 160,
                    height: 24,
                    current_path: "/tmp".to_owned(),
                    current_command: "cargo".to_owned(),
                    is_shell: false,
                }],
            },
        ]
    }

    #[test]
    fn test_window_and_pane_layout_roundtrip_preserves_all_fields() {
        for window in sample_layout() {
            let json = serde_json::to_string(&window).expect("serialize WindowLayout");
            let parsed: WindowLayout =
                serde_json::from_str(&json).expect("deserialize WindowLayout");
            assert_eq!(parsed, window);
        }
    }

    #[test]
    fn test_pane_layout_missing_meta_fields_defaults_to_empty() {
        // A layout serialized before `current_path`/`current_command` existed
        // (#442) must still deserialize, with both fields empty ("unknown").
        let json = r#"{"pane_id":3,"active":false,"left":0,"top":0,"width":80,"height":24}"#;
        let parsed: PaneLayout = serde_json::from_str(json).expect("deserialize legacy PaneLayout");
        assert_eq!(parsed.pane_id, 3);
        assert_eq!(parsed.current_path, "");
        assert_eq!(parsed.current_command, "");
        assert!(!parsed.is_shell);
    }

    #[test]
    fn test_pane_layout_malformed_meta_field_type_rejected() {
        // A present-but-wrongly-typed field is a protocol error, not an
        // empty-default case.
        let json = r#"{"pane_id":3,"active":false,"left":0,"top":0,"width":80,"height":24,"current_path":7}"#;
        assert!(serde_json::from_str::<PaneLayout>(json).is_err());
    }

    #[test]
    fn test_pane_layout_missing_is_shell_defaults_to_false() {
        // A layout serialized before `is_shell` existed (#510) must still
        // deserialize, with the field defaulting to false — same additive
        // tolerance as `current_path`/`current_command` (#442).
        let json = r#"{"pane_id":3,"active":false,"left":0,"top":0,"width":80,"height":24,"current_path":"/tmp","current_command":"cargo"}"#;
        let parsed: PaneLayout = serde_json::from_str(json).expect("deserialize legacy PaneLayout");
        assert_eq!(parsed.pane_id, 3);
        assert!(!parsed.is_shell);
    }

    #[test]
    fn test_pane_layout_malformed_is_shell_field_type_rejected() {
        // A present-but-wrongly-typed field is a protocol error, not a
        // default-to-false case.
        let json = r#"{"pane_id":3,"active":false,"left":0,"top":0,"width":80,"height":24,"is_shell":"yes"}"#;
        assert!(serde_json::from_str::<PaneLayout>(json).is_err());
    }

    #[test]
    fn test_window_layout_missing_index_defaults_to_zero() {
        // A layout serialized before `window_index` existed (#495) must still
        // deserialize, with the field defaulting to 0.
        let json = r#"{"window_id":1,"name":"editor","active":true,"panes":[]}"#;
        let parsed: WindowLayout =
            serde_json::from_str(json).expect("deserialize legacy WindowLayout");
        assert_eq!(parsed.window_id, 1);
        assert_eq!(parsed.window_index, 0);
    }

    #[test]
    fn test_window_layout_malformed_index_field_type_rejected() {
        // A present-but-wrongly-typed field is a protocol error, not an
        // empty-default case.
        let json =
            r#"{"window_id":1,"window_index":"one","name":"editor","active":true,"panes":[]}"#;
        assert!(serde_json::from_str::<WindowLayout>(json).is_err());
    }

    #[test]
    fn test_layout_snapshot_roundtrip_preserves_windows_and_panes() {
        let msg = DaemonMessage::LayoutSnapshot {
            session: "rift".to_owned(),
            windows: sample_layout(),
        };
        let json = serde_json::to_string(&msg).expect("serialize LayoutSnapshot");
        assert!(json.contains(r#""type":"layout_snapshot""#));
        assert!(json.contains(r#""session":"rift""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize LayoutSnapshot");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::LayoutSnapshot { session, windows } => {
                assert_eq!(session, "rift");
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].panes.len(), 2);
                assert!(windows[0].active);
                assert!(windows[0].panes[0].active);
            }
            other => panic!("expected LayoutSnapshot, got {other:?}"),
        }
    }

    #[test]
    fn test_layout_update_roundtrip_preserves_layout() {
        let msg = DaemonMessage::LayoutUpdate {
            session: "rift-dev".to_owned(),
            windows: sample_layout(),
        };
        let json = serde_json::to_string(&msg).expect("serialize LayoutUpdate");
        assert!(json.contains(r#""type":"layout_update""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize LayoutUpdate"),
            msg
        );
    }

    #[test]
    fn test_layout_snapshot_empty_windows_roundtrips() {
        // A fresh session may attach before any window exists; an empty layout is
        // a valid baseline, not an error.
        let msg = DaemonMessage::LayoutSnapshot {
            session: "rift".to_owned(),
            windows: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize empty LayoutSnapshot");
        assert!(json.contains(r#""windows":[]"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize empty LayoutSnapshot"),
            msg
        );
    }

    #[test]
    fn test_terminal_exit_roundtrip_with_and_without_reason() {
        // The terminal-path-down signal: carries the session and tmux's optional
        // %exit reason, and round-trips both shapes.
        let with_reason = DaemonMessage::TerminalExit {
            session: "rift".to_owned(),
            reason: Some("server exited".to_owned()),
        };
        let json = serde_json::to_string(&with_reason).expect("serialize TerminalExit");
        assert!(json.contains(r#""type":"terminal_exit""#));
        assert!(json.contains(r#""session":"rift""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize TerminalExit"),
            with_reason
        );

        let no_reason = DaemonMessage::TerminalExit {
            session: "rift-dev".to_owned(),
            reason: None,
        };
        let json = serde_json::to_string(&no_reason).expect("serialize TerminalExit");
        assert!(json.contains(r#""reason":null"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize TerminalExit"),
            no_reason
        );
    }

    #[test]
    fn test_daemon_message_unknown_type_is_rejected() {
        let err = serde_json::from_str::<DaemonMessage>(r#"{"type":"sparkle"}"#);
        assert!(
            err.is_err(),
            "unknown daemon message type must not deserialize"
        );
    }

    #[test]
    fn test_hello_roundtrip_current_version_preserves_version() {
        let msg = ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).expect("serialize Hello");
        assert_eq!(
            json,
            format!(r#"{{"type":"hello","version":{PROTOCOL_VERSION}}}"#)
        );

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize Hello");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::Hello { version } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn test_welcome_roundtrip_current_version_preserves_version() {
        let msg = DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).expect("serialize Welcome");
        assert_eq!(
            json,
            format!(r#"{{"type":"welcome","version":{PROTOCOL_VERSION}}}"#)
        );

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize Welcome");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::Welcome { version } => assert_eq!(version, PROTOCOL_VERSION),
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    #[test]
    fn test_hello_mismatched_version_parses_differing_version() {
        let json = r#"{"type":"hello","version":999}"#;
        let parsed: ClientMessage = serde_json::from_str(json).expect("deserialize Hello");
        match parsed {
            ClientMessage::Hello { version } => {
                assert_ne!(version, PROTOCOL_VERSION);
                assert_eq!(version, 999);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[test]
    fn test_worktree_snapshot_roundtrip_preserves_entries_and_chunk_flag() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/home/dev/project".to_owned(),
            entries: vec![
                WorktreeEntry {
                    path: "src".to_owned(),
                    kind: EntryKind::Dir,
                    ignored: false,
                    mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                },
                WorktreeEntry {
                    path: "target/debug/build".to_owned(),
                    kind: EntryKind::File,
                    ignored: true,
                    mtime: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_001, 500),
                },
            ],
            final_chunk: false,
        };

        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        assert!(json.contains(r#""type":"worktree_snapshot""#));
        assert!(json.contains(r#""kind":"dir""#));
        assert!(json.contains(r#""kind":"file""#));
        assert!(json.contains(r#""ignored":true"#));
        assert!(json.contains(r#""final_chunk":false"#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize WorktreeSnapshot");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_update_worktree_roundtrip_preserves_added_changed_removed() {
        let msg = DaemonMessage::UpdateWorktree {
            added: vec![WorktreeEntry {
                path: "src/new.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(10),
            }],
            changed: vec![WorktreeEntry {
                path: "src/main.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(20),
            }],
            removed: vec!["src/old.rs".to_owned()],
        };

        let json = serde_json::to_string(&msg).expect("serialize UpdateWorktree");
        assert!(json.contains(r#""type":"update_worktree""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize UpdateWorktree");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::UpdateWorktree {
                added,
                changed,
                removed,
            } => {
                assert_eq!(added.len(), 1);
                assert_eq!(changed.len(), 1);
                assert_eq!(removed, vec!["src/old.rs".to_owned()]);
            }
            other => panic!("expected UpdateWorktree, got {other:?}"),
        }
    }

    #[test]
    fn test_worktree_snapshot_final_chunk_true_with_empty_entries_roundtrips() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/home/dev/project".to_owned(),
            entries: vec![],
            final_chunk: true,
        };
        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        assert!(json.contains(r#""final_chunk":true"#));
        assert!(json.contains(r#""entries":[]"#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize WorktreeSnapshot");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_worktree_entry_mtime_serializes_as_epoch_secs_and_nanos() {
        let msg = DaemonMessage::WorktreeSnapshot {
            root: "/p".to_owned(),
            entries: vec![WorktreeEntry {
                path: "a".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH + Duration::new(5, 7),
            }],
            final_chunk: true,
        };
        let json = serde_json::to_string(&msg).expect("serialize WorktreeSnapshot");
        // Pin the wire shape of `mtime`: the protocol may migrate to MessagePack,
        // so an accidental change to the timestamp representation must fail a test.
        assert!(json.contains(r#""mtime":{"secs_since_epoch":5,"nanos_since_epoch":7}"#));
    }

    #[test]
    fn test_update_git_status_roundtrip_preserves_changed_and_cleared() {
        let msg = DaemonMessage::UpdateGitStatus {
            changed: vec![
                GitStatusEntry {
                    path: "src/main.rs".to_owned(),
                    status: GitEntryStatus {
                        index: GitStatusCode::Unmodified,
                        worktree: GitStatusCode::Modified,
                    },
                },
                GitStatusEntry {
                    path: "new.rs".to_owned(),
                    status: GitEntryStatus {
                        index: GitStatusCode::Added,
                        worktree: GitStatusCode::Unmodified,
                    },
                },
            ],
            cleared: vec!["was_dirty.rs".to_owned()],
        };

        let json = serde_json::to_string(&msg).expect("serialize UpdateGitStatus");
        assert!(json.contains(r#""type":"update_git_status""#));
        assert!(json.contains(r#""index":"added""#));
        assert!(json.contains(r#""worktree":"modified""#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize UpdateGitStatus");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::UpdateGitStatus { changed, cleared } => {
                assert_eq!(changed.len(), 2);
                assert_eq!(cleared, vec!["was_dirty.rs".to_owned()]);
            }
            other => panic!("expected UpdateGitStatus, got {other:?}"),
        }
    }

    #[test]
    fn test_git_entry_status_untracked_and_conflict_pairs_roundtrip() {
        // The two edge pairs: an untracked file (worktree-only `Untracked`) and
        // a merge conflict (`Unmerged` on both sides).
        let untracked = GitEntryStatus {
            index: GitStatusCode::Unmodified,
            worktree: GitStatusCode::Untracked,
        };
        let conflict = GitEntryStatus {
            index: GitStatusCode::Unmerged,
            worktree: GitStatusCode::Unmerged,
        };
        for status in [untracked, conflict] {
            let json = serde_json::to_string(&status).expect("serialize GitEntryStatus");
            let parsed: GitEntryStatus =
                serde_json::from_str(&json).expect("deserialize GitEntryStatus");
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_repo_state_roundtrip_branch_and_detached_head() {
        let on_branch = DaemonMessage::RepoState {
            branch: Some("main".to_owned()),
            ahead_behind: Some(AheadBehind {
                ahead: 2,
                behind: 1,
            }),
            lines_added: 12,
            lines_removed: 3,
        };
        let json = serde_json::to_string(&on_branch).expect("serialize RepoState");
        assert!(json.contains(r#""type":"repo_state""#));
        assert!(json.contains(r#""branch":"main""#));
        assert!(json.contains(r#""ahead":2"#));
        assert!(json.contains(r#""lines_added":12"#));
        assert!(json.contains(r#""lines_removed":3"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize RepoState"),
            on_branch
        );

        // Detached HEAD with no upstream and a clean worktree: both optional
        // fields are absent (`None`) and the totals are both zero.
        let detached = DaemonMessage::RepoState {
            branch: None,
            ahead_behind: None,
            lines_added: 0,
            lines_removed: 0,
        };
        let json = serde_json::to_string(&detached).expect("serialize detached RepoState");
        assert!(json.contains(r#""branch":null"#));
        assert!(json.contains(r#""ahead_behind":null"#));
        assert!(json.contains(r#""lines_added":0"#));
        assert!(json.contains(r#""lines_removed":0"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize detached RepoState"),
            detached
        );
    }

    #[test]
    fn test_repo_state_missing_line_totals_are_rejected() {
        // The totals are non-optional plain `u32`s (always `0` on a clean
        // worktree, never absent) — a `RepoState` frame missing either must
        // not deserialize by silently defaulting to zero.
        for json in [
            r#"{"type":"repo_state","branch":null,"ahead_behind":null,"lines_removed":0}"#,
            r#"{"type":"repo_state","branch":null,"ahead_behind":null,"lines_added":0}"#,
            r#"{"type":"repo_state","branch":null,"ahead_behind":null,"lines_added":"1","lines_removed":0}"#,
        ] {
            assert!(
                serde_json::from_str::<DaemonMessage>(json).is_err(),
                "malformed RepoState must not deserialize: {json}"
            );
        }
    }

    #[test]
    fn test_lsp_status_roundtrips_each_state() {
        for state in [
            LspServerState::Starting,
            LspServerState::Running,
            LspServerState::Crashed,
        ] {
            let msg = DaemonMessage::LspStatus {
                server: "rust-analyzer".to_owned(),
                state,
            };
            let json = serde_json::to_string(&msg).expect("serialize LspStatus");
            assert!(json.contains(r#""type":"lsp_status""#));
            assert!(json.contains(r#""server":"rust-analyzer""#));
            assert_eq!(
                serde_json::from_str::<DaemonMessage>(&json).expect("deserialize LspStatus"),
                msg
            );
        }
    }

    #[test]
    fn test_lsp_status_unknown_state_is_rejected() {
        // An unrecognized state must fail loudly, not coerce to a known one —
        // the same strict-enum discipline as `GitStatusCode`.
        let json = r#"{"type":"lsp_status","server":"rust-analyzer","state":"stopped"}"#;
        assert!(
            serde_json::from_str::<DaemonMessage>(json).is_err(),
            "an unknown LspServerState must not deserialize"
        );
    }

    #[test]
    fn test_lsp_status_missing_field_is_rejected() {
        for json in [
            r#"{"type":"lsp_status","state":"running"}"#,
            r#"{"type":"lsp_status","server":"rust-analyzer"}"#,
        ] {
            assert!(
                serde_json::from_str::<DaemonMessage>(json).is_err(),
                "malformed LspStatus must not deserialize: {json}"
            );
        }
    }

    #[test]
    fn test_git_status_code_unknown_variant_is_rejected() {
        // serde rejects an unknown enum variant rather than silently defaulting,
        // so a future daemon emitting a code this client does not know fails
        // loudly instead of being misread as a valid status.
        let err = serde_json::from_str::<GitStatusCode>(r#""partially_staged""#);
        assert!(err.is_err(), "unknown status code must not deserialize");
    }

    #[test]
    fn test_diagnostics_roundtrip_preserves_path_server_and_items() {
        let msg = DaemonMessage::Diagnostics {
            path: "src/main.rs".to_owned(),
            server: "rust-analyzer".to_owned(),
            items: vec![Diagnostic {
                range: Range {
                    start: Position {
                        line: 10,
                        character: 4,
                    },
                    end: Position {
                        line: 10,
                        character: 9,
                    },
                },
                severity: DiagnosticSeverity::Error,
                message: "cannot find value `foo` in this scope".to_owned(),
                source: Some("rustc".to_owned()),
                code: Some("E0425".to_owned()),
            }],
        };

        let json = serde_json::to_string(&msg).expect("serialize Diagnostics");
        assert!(json.contains(r#""type":"diagnostics""#));
        assert!(json.contains(r#""path":"src/main.rs""#));
        assert!(json.contains(r#""server":"rust-analyzer""#));
        assert!(json.contains(r#""severity":"error""#));
        assert!(json.contains(r#""code":"E0425""#));

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize Diagnostics");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::Diagnostics {
                path,
                server,
                items,
            } => {
                assert_eq!(path, "src/main.rs");
                assert_eq!(server, "rust-analyzer");
                assert_eq!(items.len(), 1);
            }
            other => panic!("expected Diagnostics, got {other:?}"),
        }
    }

    #[test]
    fn test_diagnostics_empty_items_clears_one_servers_set() {
        // An empty `items` is the full-set-replace clear for that `(file,
        // server)` pair — it must round-trip as an empty list, not vanish.
        let msg = DaemonMessage::Diagnostics {
            path: "src/lib.rs".to_owned(),
            server: "clippy".to_owned(),
            items: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize Diagnostics");
        assert!(json.contains(r#""items":[]"#));

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize Diagnostics");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_diagnostic_omits_absent_source_and_code() {
        // A server that supplies neither `source` nor `code` must produce a
        // diagnostic with those fields absent on the wire, and reading a payload
        // without them back must yield `None` (forward-compatible defaults).
        let diag = Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            severity: DiagnosticSeverity::Warning,
            message: "unused import".to_owned(),
            source: None,
            code: None,
        };
        let json = serde_json::to_string(&diag).expect("serialize Diagnostic");
        assert!(!json.contains("source"));
        assert!(!json.contains(r#""code""#));

        let parsed: Diagnostic = serde_json::from_str(&json).expect("deserialize Diagnostic");
        assert_eq!(parsed, diag);
        assert!(parsed.source.is_none());
        assert!(parsed.code.is_none());
    }

    #[test]
    fn test_diagnostic_severity_unknown_variant_is_rejected() {
        // An unknown severity must fail loudly rather than silently defaulting,
        // matching the git-status-code discipline.
        let err = serde_json::from_str::<DiagnosticSeverity>(r#""fatal""#);
        assert!(err.is_err(), "unknown severity must not deserialize");
    }

    #[test]
    fn test_open_file_roundtrip_preserves_path() {
        // The buffer channel's read request: carries only the path, no content —
        // content is pulled back on the reply, never sent on the request.
        let msg = ClientMessage::OpenFile {
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize OpenFile");
        assert_eq!(json, r#"{"type":"open_file","path":"src/main.rs"}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize OpenFile");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::OpenFile { path } => assert_eq!(path, "src/main.rs"),
            other => panic!("expected OpenFile, got {other:?}"),
        }
    }

    #[test]
    fn test_buffer_changed_roundtrip_preserves_path_and_content() {
        // The live-buffer feed carries the whole current buffer text but, unlike
        // SaveFile, no `mtime` — it is not a write, only the LSP's source of truth.
        let msg = ClientMessage::BufferChanged {
            path: "src/main.rs".to_owned(),
            content: "fn main() { let x: u32 = \"oops\"; }\n".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize BufferChanged");
        assert!(json.contains(r#""type":"buffer_changed""#));
        assert!(json.contains(r#""path":"src/main.rs""#));
        // No `mtime` / `base_mtime` — the feed is not a write.
        assert!(!json.contains("mtime"));

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize BufferChanged");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::BufferChanged { path, content } => {
                assert_eq!(path, "src/main.rs");
                assert!(content.contains("oops"));
            }
            other => panic!("expected BufferChanged, got {other:?}"),
        }
    }

    #[test]
    fn test_buffer_closed_roundtrip_preserves_path() {
        // Ending the live-buffer feed carries only the path; the daemon reverts it
        // to the disk-backed baseline.
        let msg = ClientMessage::BufferClosed {
            path: "src/lib.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize BufferClosed");
        assert_eq!(json, r#"{"type":"buffer_closed","path":"src/lib.rs"}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize BufferClosed");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_save_file_roundtrip_preserves_path_content_and_base_mtime() {
        // The write request carries the whole file plus the base `mtime` the
        // editor read on open — the conflict detector's input.
        let msg = ClientMessage::SaveFile {
            path: "src/lib.rs".to_owned(),
            content: "fn main() {}\n".to_owned(),
            base_mtime: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 42),
        };
        let json = serde_json::to_string(&msg).expect("serialize SaveFile");
        assert!(json.contains(r#""type":"save_file""#));
        assert!(json.contains(r#""content":"fn main() {}\n""#));

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize SaveFile");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::SaveFile {
                path,
                content,
                base_mtime,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(content, "fn main() {}\n");
                assert_eq!(
                    base_mtime,
                    SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 42)
                );
            }
            other => panic!("expected SaveFile, got {other:?}"),
        }
    }

    #[test]
    fn test_file_content_roundtrip_preserves_path_content_and_mtime() {
        // The read reply carries the whole file content and its `mtime` — the
        // only daemon message that carries file content.
        let msg = DaemonMessage::FileContent {
            path: "src/main.rs".to_owned(),
            content: "use std::io;\n".to_owned(),
            mtime: SystemTime::UNIX_EPOCH + Duration::new(1_700_000_001, 500),
        };
        let json = serde_json::to_string(&msg).expect("serialize FileContent");
        assert!(json.contains(r#""type":"file_content""#));
        assert!(json.contains(r#""content":"use std::io;\n""#));

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize FileContent");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::FileContent {
                path,
                content,
                mtime,
            } => {
                assert_eq!(path, "src/main.rs");
                assert_eq!(content, "use std::io;\n");
                assert_eq!(
                    mtime,
                    SystemTime::UNIX_EPOCH + Duration::new(1_700_000_001, 500)
                );
            }
            other => panic!("expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn test_save_result_roundtrip_preserves_path_and_mtime() {
        let msg = DaemonMessage::SaveResult {
            path: "src/lib.rs".to_owned(),
            mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_100),
        };
        let json = serde_json::to_string(&msg).expect("serialize SaveResult");
        assert!(json.contains(r#""type":"save_result""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize SaveResult"),
            msg
        );
    }

    #[test]
    fn test_save_conflict_roundtrip_preserves_path_and_disk_mtime() {
        // The stale-base rejection: carries the current on-disk `mtime` so the
        // editor can rebase. No write happened.
        let msg = DaemonMessage::SaveConflict {
            path: "src/lib.rs".to_owned(),
            disk_mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_200),
        };
        let json = serde_json::to_string(&msg).expect("serialize SaveConflict");
        assert!(json.contains(r#""type":"save_conflict""#));
        assert!(json.contains(r#""disk_mtime""#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize SaveConflict"),
            msg
        );
    }

    #[test]
    fn test_buffer_channel_mtime_matches_worktree_entry_wire_shape() {
        // The buffer channel's `mtime` / `base_mtime` / `disk_mtime` must be the
        // identical `SystemTime` representation as the worktree entry's `mtime`
        // (#107), so a base read on one path can be compared on the other. Pin
        // all four to the same wire shape with the same instant.
        let mtime = SystemTime::UNIX_EPOCH + Duration::new(5, 7);
        let expected = r#"{"secs_since_epoch":5,"nanos_since_epoch":7}"#;

        let entry = WorktreeEntry {
            path: "a".to_owned(),
            kind: EntryKind::File,
            ignored: false,
            mtime,
        };
        let content = DaemonMessage::FileContent {
            path: "a".to_owned(),
            content: String::new(),
            mtime,
        };
        let save = ClientMessage::SaveFile {
            path: "a".to_owned(),
            content: String::new(),
            base_mtime: mtime,
        };
        let conflict = DaemonMessage::SaveConflict {
            path: "a".to_owned(),
            disk_mtime: mtime,
        };

        let entry_json = serde_json::to_string(&entry).expect("serialize WorktreeEntry");
        assert!(entry_json.contains(&format!(r#""mtime":{expected}"#)));
        assert!(serde_json::to_string(&content)
            .expect("serialize FileContent")
            .contains(&format!(r#""mtime":{expected}"#)));
        assert!(serde_json::to_string(&save)
            .expect("serialize SaveFile")
            .contains(&format!(r#""base_mtime":{expected}"#)));
        assert!(serde_json::to_string(&conflict)
            .expect("serialize SaveConflict")
            .contains(&format!(r#""disk_mtime":{expected}"#)));
    }

    #[test]
    fn test_structure_path_messages_carry_no_file_content() {
        // The buffer channel is the only path that moves file content. The
        // worktree / git / diagnostics messages must stay content-free: a sample
        // of each must not serialize a `content` field.
        let worktree = DaemonMessage::WorktreeSnapshot {
            root: "/p".to_owned(),
            entries: vec![WorktreeEntry {
                path: "a.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH,
            }],
            final_chunk: true,
        };
        let update = DaemonMessage::UpdateWorktree {
            added: vec![WorktreeEntry {
                path: "a.rs".to_owned(),
                kind: EntryKind::File,
                ignored: false,
                mtime: SystemTime::UNIX_EPOCH,
            }],
            changed: vec![],
            removed: vec![],
        };
        let git = DaemonMessage::UpdateGitStatus {
            changed: vec![GitStatusEntry {
                path: "a.rs".to_owned(),
                status: GitEntryStatus {
                    index: GitStatusCode::Unmodified,
                    worktree: GitStatusCode::Modified,
                },
            }],
            cleared: vec![],
        };
        let diagnostics = DaemonMessage::Diagnostics {
            path: "a.rs".to_owned(),
            server: "rust-analyzer".to_owned(),
            items: vec![],
        };

        for msg in [worktree, update, git, diagnostics] {
            let json = serde_json::to_string(&msg).expect("serialize structure message");
            assert!(
                !json.contains("content"),
                "structure-path message must carry no file content: {json}"
            );
        }
    }

    // ---- Navigation request/response round-trip tests ----------------------

    #[test]
    fn test_nav_request_id_roundtrip_preserves_value() {
        // NavRequestId is the correlation key for navigation requests; the wire
        // value must survive serialization unchanged so stale-response detection
        // works.
        let id = NavRequestId(42);
        let json = serde_json::to_string(&id).expect("serialize NavRequestId");
        assert_eq!(json, "42");
        let parsed: NavRequestId = serde_json::from_str(&json).expect("deserialize NavRequestId");
        assert_eq!(parsed, id);
    }

    #[test]
    fn test_hover_request_roundtrip_preserves_id_path_position() {
        let msg = ClientMessage::HoverRequest {
            id: NavRequestId(1),
            path: "src/main.rs".to_owned(),
            position: Position {
                line: 5,
                character: 10,
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize HoverRequest");
        assert!(json.contains(r#""type":"hover_request""#));
        assert!(json.contains(r#""path":"src/main.rs""#));
        assert!(json.contains(r#""line":5"#));
        assert!(json.contains(r#""character":10"#));
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize HoverRequest");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_definition_request_roundtrip_preserves_id_path_position() {
        let msg = ClientMessage::DefinitionRequest {
            id: NavRequestId(2),
            path: "src/lib.rs".to_owned(),
            position: Position {
                line: 0,
                character: 0,
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize DefinitionRequest");
        assert!(json.contains(r#""type":"definition_request""#));
        assert!(json.contains(r#""path":"src/lib.rs""#));
        let parsed: ClientMessage =
            serde_json::from_str(&json).expect("deserialize DefinitionRequest");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_references_request_roundtrip_preserves_id_path_position() {
        let msg = ClientMessage::ReferencesRequest {
            id: NavRequestId(3),
            path: "src/lib.rs".to_owned(),
            position: Position {
                line: 20,
                character: 4,
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize ReferencesRequest");
        assert!(json.contains(r#""type":"references_request""#));
        let parsed: ClientMessage =
            serde_json::from_str(&json).expect("deserialize ReferencesRequest");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_hover_response_with_content_roundtrip_preserves_id_and_markdown() {
        let msg = DaemonMessage::HoverResponse {
            id: NavRequestId(1),
            content: Some(HoverContent {
                markdown: "**fn main()** — entry point".to_owned(),
                range: Some(Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 7,
                    },
                }),
            }),
        };
        let json = serde_json::to_string(&msg).expect("serialize HoverResponse");
        assert!(json.contains(r#""type":"hover_response""#));
        assert!(json.contains(r#""markdown":"**fn main()** — entry point""#));
        assert!(json.contains(r#""range""#));
        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize HoverResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_hover_response_no_content_is_none_not_absent() {
        // A position with no hover result: `content` is `None`; the client shows
        // nothing (silent no-op, no error surface). The field must survive
        // round-trip as `None`, never disappear or default to something.
        let msg = DaemonMessage::HoverResponse {
            id: NavRequestId(7),
            content: None,
        };
        let json = serde_json::to_string(&msg).expect("serialize HoverResponse none");
        assert!(json.contains(r#""content":null"#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize HoverResponse none");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_hover_content_omits_range_when_absent() {
        // When the server does not supply a symbol range with the hover, the
        // `range` field must be absent on the wire (skip_serializing_if None).
        let content = HoverContent {
            markdown: "i32".to_owned(),
            range: None,
        };
        let json = serde_json::to_string(&content).expect("serialize HoverContent");
        assert!(
            !json.contains("range"),
            "range must be absent when None: {json}"
        );
        let parsed: HoverContent = serde_json::from_str(&json).expect("deserialize HoverContent");
        assert_eq!(parsed, content);
    }

    #[test]
    fn test_definition_response_single_target_roundtrip() {
        let msg = DaemonMessage::DefinitionResponse {
            id: NavRequestId(2),
            targets: vec![NavLocation {
                path: "src/lib.rs".to_owned(),
                range: Range {
                    start: Position {
                        line: 10,
                        character: 4,
                    },
                    end: Position {
                        line: 10,
                        character: 12,
                    },
                },
                out_of_root: false,
                line_preview: Some("pub fn foo() {}".to_owned()),
            }],
        };
        let json = serde_json::to_string(&msg).expect("serialize DefinitionResponse");
        assert!(json.contains(r#""type":"definition_response""#));
        assert!(json.contains(r#""path":"src/lib.rs""#));
        assert!(json.contains(r#""line_preview":"pub fn foo() {}""#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize DefinitionResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_definition_response_empty_targets_is_silent_no_op() {
        // An empty `targets` means the server found no definition — the client
        // shows nothing. Must round-trip as an empty list, not as an error.
        let msg = DaemonMessage::DefinitionResponse {
            id: NavRequestId(5),
            targets: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize empty DefinitionResponse");
        assert!(json.contains(r#""targets":[]"#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize empty DefinitionResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_definition_response_multiple_targets_roundtrip() {
        // Rust trait method impls: multiple definition targets land in the
        // jump-list picker. The protocol must carry all of them.
        let msg = DaemonMessage::DefinitionResponse {
            id: NavRequestId(3),
            targets: vec![
                NavLocation {
                    path: "src/a.rs".to_owned(),
                    range: Range {
                        start: Position {
                            line: 1,
                            character: 0,
                        },
                        end: Position {
                            line: 1,
                            character: 5,
                        },
                    },
                    out_of_root: false,
                    line_preview: Some("impl Foo for A {}".to_owned()),
                },
                NavLocation {
                    path: "src/b.rs".to_owned(),
                    range: Range {
                        start: Position {
                            line: 3,
                            character: 0,
                        },
                        end: Position {
                            line: 3,
                            character: 5,
                        },
                    },
                    out_of_root: false,
                    line_preview: Some("impl Foo for B {}".to_owned()),
                },
            ],
        };
        let json = serde_json::to_string(&msg).expect("serialize multi-target DefinitionResponse");
        assert!(json.contains(r#""path":"src/a.rs""#));
        assert!(json.contains(r#""path":"src/b.rs""#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize multi-target DefinitionResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_references_response_roundtrip_preserves_locations() {
        let msg = DaemonMessage::ReferencesResponse {
            id: NavRequestId(4),
            locations: vec![
                NavLocation {
                    path: "src/main.rs".to_owned(),
                    range: Range {
                        start: Position {
                            line: 5,
                            character: 4,
                        },
                        end: Position {
                            line: 5,
                            character: 7,
                        },
                    },
                    out_of_root: false,
                    line_preview: Some("    foo(x)".to_owned()),
                },
                NavLocation {
                    path: "tests/integration.rs".to_owned(),
                    range: Range {
                        start: Position {
                            line: 20,
                            character: 12,
                        },
                        end: Position {
                            line: 20,
                            character: 15,
                        },
                    },
                    out_of_root: false,
                    line_preview: Some("    assert!(foo(y))".to_owned()),
                },
            ],
        };
        let json = serde_json::to_string(&msg).expect("serialize ReferencesResponse");
        assert!(json.contains(r#""type":"references_response""#));
        assert!(json.contains(r#""path":"tests/integration.rs""#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize ReferencesResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_references_response_empty_locations_is_silent_no_op() {
        let msg = DaemonMessage::ReferencesResponse {
            id: NavRequestId(9),
            locations: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize empty ReferencesResponse");
        assert!(json.contains(r#""locations":[]"#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize empty ReferencesResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_document_symbol_request_roundtrip_preserves_id_and_path() {
        // No `Position` field, unlike the other navigation requests — a
        // document-symbol request covers the whole file.
        let msg = ClientMessage::DocumentSymbolRequest {
            id: NavRequestId(10),
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize DocumentSymbolRequest");
        assert!(json.contains(r#""type":"document_symbol_request""#));
        assert!(!json.contains("position"));
        let parsed: ClientMessage =
            serde_json::from_str(&json).expect("deserialize DocumentSymbolRequest");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_document_symbol_response_roundtrip_preserves_symbols() {
        let msg = DaemonMessage::DocumentSymbolResponse {
            id: NavRequestId(11),
            symbols: vec![
                DocumentSymbolEntry {
                    name: "Foo".to_owned(),
                    kind: SymbolKind::Struct,
                    range: Range {
                        start: Position {
                            line: 0,
                            character: 0,
                        },
                        end: Position {
                            line: 3,
                            character: 1,
                        },
                    },
                    selection_range: Range {
                        start: Position {
                            line: 0,
                            character: 7,
                        },
                        end: Position {
                            line: 0,
                            character: 10,
                        },
                    },
                    depth: 0,
                },
                DocumentSymbolEntry {
                    name: "bar".to_owned(),
                    kind: SymbolKind::Field,
                    range: Range {
                        start: Position {
                            line: 1,
                            character: 4,
                        },
                        end: Position {
                            line: 1,
                            character: 12,
                        },
                    },
                    selection_range: Range {
                        start: Position {
                            line: 1,
                            character: 4,
                        },
                        end: Position {
                            line: 1,
                            character: 7,
                        },
                    },
                    depth: 1,
                },
            ],
        };
        let json = serde_json::to_string(&msg).expect("serialize DocumentSymbolResponse");
        assert!(json.contains(r#""type":"document_symbol_response""#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize DocumentSymbolResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_document_symbol_response_empty_symbols_is_silent_no_op() {
        let msg = DaemonMessage::DocumentSymbolResponse {
            id: NavRequestId(12),
            symbols: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize empty DocumentSymbolResponse");
        assert!(json.contains(r#""symbols":[]"#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize empty DocumentSymbolResponse");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_symbol_kind_roundtrips_every_variant() {
        let kinds = [
            SymbolKind::File,
            SymbolKind::Module,
            SymbolKind::Namespace,
            SymbolKind::Package,
            SymbolKind::Class,
            SymbolKind::Method,
            SymbolKind::Property,
            SymbolKind::Field,
            SymbolKind::Constructor,
            SymbolKind::Enum,
            SymbolKind::Interface,
            SymbolKind::Function,
            SymbolKind::Variable,
            SymbolKind::Constant,
            SymbolKind::String,
            SymbolKind::Number,
            SymbolKind::Boolean,
            SymbolKind::Array,
            SymbolKind::Object,
            SymbolKind::Key,
            SymbolKind::Null,
            SymbolKind::EnumMember,
            SymbolKind::Struct,
            SymbolKind::Event,
            SymbolKind::Operator,
            SymbolKind::TypeParameter,
        ];
        for kind in kinds {
            let json = serde_json::to_string(&kind).expect("serialize SymbolKind");
            let parsed: SymbolKind = serde_json::from_str(&json).expect("deserialize SymbolKind");
            assert_eq!(parsed, kind);
        }
    }

    #[test]
    fn test_symbol_kind_unknown_variant_is_rejected() {
        let result: Result<SymbolKind, _> = serde_json::from_str(r#""bogus_kind""#);
        assert!(
            result.is_err(),
            "unknown SymbolKind variant must be rejected"
        );
    }

    #[test]
    fn test_document_symbol_entry_missing_field_is_rejected() {
        // A malformed entry missing `depth` must not silently default — every
        // field is required, mirroring the other nav wire types.
        let json = r#"{
            "name": "Foo",
            "kind": "struct",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } },
            "selection_range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }
        }"#;
        let result: Result<DocumentSymbolEntry, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "malformed DocumentSymbolEntry (missing depth) must not deserialize"
        );
    }

    #[test]
    fn test_nav_location_out_of_root_carries_flag_and_absolute_path() {
        // A stdlib / registry dependency target: `out_of_root` is true, `path`
        // is absolute. The client opens it read-only. The flag must be present
        // on the wire when true (it is absent/false-defaulting when in-root).
        let loc = NavLocation {
            path: "/home/user/.cargo/registry/src/foo/src/lib.rs".to_owned(),
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 1,
                },
            },
            out_of_root: true,
            line_preview: None,
        };
        let json = serde_json::to_string(&loc).expect("serialize out-of-root NavLocation");
        assert!(json.contains(r#""out_of_root":true"#));
        // No line_preview: the field must be absent, not null.
        assert!(
            !json.contains("line_preview"),
            "line_preview must be absent when None: {json}"
        );
        let parsed: NavLocation =
            serde_json::from_str(&json).expect("deserialize out-of-root NavLocation");
        assert_eq!(parsed, loc);
    }

    #[test]
    fn test_nav_location_in_root_omits_out_of_root_flag() {
        // An in-root location: `out_of_root` defaults to `false` and must be
        // absent from the wire (skip_serializing_if false), so existing consumers
        // that do not know the flag keep working.
        let loc = NavLocation {
            path: "src/main.rs".to_owned(),
            range: Range {
                start: Position {
                    line: 1,
                    character: 0,
                },
                end: Position {
                    line: 1,
                    character: 4,
                },
            },
            out_of_root: false,
            line_preview: Some("fn main() {}".to_owned()),
        };
        let json = serde_json::to_string(&loc).expect("serialize in-root NavLocation");
        assert!(
            !json.contains("out_of_root"),
            "out_of_root must be absent when false: {json}"
        );
        let parsed: NavLocation =
            serde_json::from_str(&json).expect("deserialize in-root NavLocation");
        assert_eq!(parsed, loc);
    }

    #[test]
    fn test_nav_request_id_correlation_echoed_in_responses() {
        // The explicit id must survive the full request → response round-trip so
        // the client can match and drop stale responses. Verify that the echoed
        // id in each response type equals what was sent in the request.
        let id = NavRequestId(99);
        let hover_req = ClientMessage::HoverRequest {
            id,
            path: "src/main.rs".to_owned(),
            position: Position {
                line: 0,
                character: 0,
            },
        };
        let def_req = ClientMessage::DefinitionRequest {
            id,
            path: "src/main.rs".to_owned(),
            position: Position {
                line: 0,
                character: 0,
            },
        };
        let ref_req = ClientMessage::ReferencesRequest {
            id,
            path: "src/main.rs".to_owned(),
            position: Position {
                line: 0,
                character: 0,
            },
        };

        // Daemon echoes the same id in every response variant.
        let hover_resp = DaemonMessage::HoverResponse { id, content: None };
        let def_resp = DaemonMessage::DefinitionResponse {
            id,
            targets: vec![],
        };
        let ref_resp = DaemonMessage::ReferencesResponse {
            id,
            locations: vec![],
        };

        for req in [
            serde_json::to_string(&hover_req).expect("serialize hover req"),
            serde_json::to_string(&def_req).expect("serialize def req"),
            serde_json::to_string(&ref_req).expect("serialize ref req"),
        ] {
            assert!(
                req.contains(r#""id":99"#),
                "request must carry id 99: {req}"
            );
        }
        for resp in [
            serde_json::to_string(&hover_resp).expect("serialize hover resp"),
            serde_json::to_string(&def_resp).expect("serialize def resp"),
            serde_json::to_string(&ref_resp).expect("serialize ref resp"),
        ] {
            assert!(
                resp.contains(r#""id":99"#),
                "response must echo id 99: {resp}"
            );
        }
    }

    #[test]
    fn test_nav_request_types_share_position_type_with_diagnostics() {
        // Navigation requests use the same `Position` and `Range` types as the
        // Diagnostics message (#176) — one position convention in the protocol,
        // never two. Verify the wire shape is identical.
        let pos = Position {
            line: 10,
            character: 4,
        };
        let nav_req = ClientMessage::HoverRequest {
            id: NavRequestId(0),
            path: "src/main.rs".to_owned(),
            position: pos,
        };
        let diag = DaemonMessage::Diagnostics {
            path: "src/main.rs".to_owned(),
            server: "rust-analyzer".to_owned(),
            items: vec![Diagnostic {
                range: Range {
                    start: pos,
                    end: pos,
                },
                severity: DiagnosticSeverity::Error,
                message: "test".to_owned(),
                source: None,
                code: None,
            }],
        };
        let req_json = serde_json::to_string(&nav_req).expect("serialize HoverRequest");
        let diag_json = serde_json::to_string(&diag).expect("serialize Diagnostics");
        // Both must use the same wire shape for the position fields.
        assert!(req_json.contains(r#""line":10,"character":4"#));
        assert!(diag_json.contains(r#""line":10,"character":4"#));
    }

    // ---- Source-control diff round-trip tests -------------------------------

    #[test]
    fn test_request_diff_roundtrip_preserves_path() {
        // The diff pull request carries only the path, no id — at most one
        // diff is ever inflight per path, so path-keying (like the buffer
        // channel) is sufficient correlation.
        let msg = ClientMessage::RequestDiff {
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize RequestDiff");
        assert_eq!(json, r#"{"type":"request_diff","path":"src/main.rs"}"#);

        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize RequestDiff");
        assert_eq!(parsed, msg);
        match parsed {
            ClientMessage::RequestDiff { path } => assert_eq!(path, "src/main.rs"),
            other => panic!("expected RequestDiff, got {other:?}"),
        }
    }

    fn sample_hunk() -> DiffHunk {
        DiffHunk {
            old_start: 1,
            old_len: 3,
            new_start: 1,
            new_len: 3,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    content: "one".to_owned(),
                },
                DiffLine {
                    kind: DiffLineKind::Remove,
                    content: "two".to_owned(),
                },
                DiffLine {
                    kind: DiffLineKind::Add,
                    content: "TWO".to_owned(),
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    content: "three".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn test_file_diff_hunks_roundtrip_preserves_lines_and_ranges() {
        let msg = DaemonMessage::FileDiff {
            path: "src/main.rs".to_owned(),
            diff: FileDiffPayload::Hunks {
                hunks: vec![sample_hunk()],
            },
        };
        let json = serde_json::to_string(&msg).expect("serialize FileDiff");
        assert!(json.contains(r#""type":"file_diff""#));
        assert!(json.contains(r#""kind":"hunks""#));
        assert!(json.contains(r#""kind":"remove""#));
        assert!(json.contains(r#""kind":"add""#));
        assert!(json.contains(r#""kind":"context""#));

        let parsed: DaemonMessage = serde_json::from_str(&json).expect("deserialize FileDiff");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::FileDiff { path, diff } => {
                assert_eq!(path, "src/main.rs");
                match diff {
                    FileDiffPayload::Hunks { hunks } => {
                        assert_eq!(hunks.len(), 1);
                        assert_eq!(hunks[0].lines.len(), 4);
                    }
                    other => panic!("expected Hunks, got {other:?}"),
                }
            }
            other => panic!("expected FileDiff, got {other:?}"),
        }
    }

    #[test]
    fn test_file_diff_empty_hunks_roundtrips_as_identical_content() {
        // No hunks means the worktree content matches HEAD exactly — must
        // round-trip as an empty list, not vanish or collapse to a sentinel.
        let msg = DaemonMessage::FileDiff {
            path: "src/lib.rs".to_owned(),
            diff: FileDiffPayload::Hunks { hunks: vec![] },
        };
        let json = serde_json::to_string(&msg).expect("serialize empty FileDiff");
        assert!(json.contains(r#""hunks":[]"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize empty FileDiff"),
            msg
        );
    }

    #[test]
    fn test_file_diff_binary_and_too_large_sentinels_roundtrip() {
        for payload in [FileDiffPayload::Binary, FileDiffPayload::TooLarge] {
            let msg = DaemonMessage::FileDiff {
                path: "assets/logo.png".to_owned(),
                diff: payload.clone(),
            };
            let json = serde_json::to_string(&msg).expect("serialize FileDiff sentinel");
            assert!(!json.contains("hunks"), "a sentinel must carry no hunks");
            assert_eq!(
                serde_json::from_str::<DaemonMessage>(&json)
                    .expect("deserialize FileDiff sentinel"),
                msg
            );
        }
    }

    #[test]
    fn test_file_diff_payload_kind_tag_is_rejected_when_unknown() {
        let err = serde_json::from_str::<FileDiffPayload>(r#"{"kind":"frobnicate"}"#);
        assert!(
            err.is_err(),
            "unknown diff payload kind must not deserialize"
        );
    }

    // ---- Source-control write round-trip tests (docs/spec-source-control-write.md) ----

    #[test]
    fn test_stage_file_roundtrip_preserves_path() {
        let msg = ClientMessage::StageFile {
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize StageFile");
        assert_eq!(json, r#"{"type":"stage_file","path":"src/main.rs"}"#);
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize StageFile");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_stage_file_missing_path_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"stage_file"}"#);
        assert!(
            err.is_err(),
            "stage_file without a path must not deserialize"
        );
    }

    #[test]
    fn test_unstage_file_roundtrip_preserves_path() {
        let msg = ClientMessage::UnstageFile {
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize UnstageFile");
        assert_eq!(json, r#"{"type":"unstage_file","path":"src/main.rs"}"#);
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize UnstageFile");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_unstage_file_missing_path_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"unstage_file"}"#);
        assert!(
            err.is_err(),
            "unstage_file without a path must not deserialize"
        );
    }

    #[test]
    fn test_stage_hunk_roundtrip_preserves_path_and_hunk_id() {
        let msg = ClientMessage::StageHunk {
            path: "src/main.rs".to_owned(),
            hunk_id: 0x1234_5678_9abc_def0,
        };
        let json = serde_json::to_string(&msg).expect("serialize StageHunk");
        assert!(json.contains(r#""type":"stage_hunk""#));
        assert!(json.contains(r#""path":"src/main.rs""#));
        assert!(json.contains(&format!(r#""hunk_id":{}"#, 0x1234_5678_9abc_def0u64)));
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize StageHunk");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_stage_hunk_missing_hunk_id_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"stage_hunk","path":"a.rs"}"#);
        assert!(
            err.is_err(),
            "stage_hunk without a hunk_id must not deserialize"
        );
    }

    #[test]
    fn test_discard_file_roundtrip_preserves_path() {
        let msg = ClientMessage::DiscardFile {
            path: "src/main.rs".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize DiscardFile");
        assert_eq!(json, r#"{"type":"discard_file","path":"src/main.rs"}"#);
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize DiscardFile");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_discard_file_missing_path_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"discard_file"}"#);
        assert!(
            err.is_err(),
            "discard_file without a path must not deserialize"
        );
    }

    #[test]
    fn test_commit_roundtrip_preserves_message() {
        let msg = ClientMessage::Commit {
            message: "fix: handle malformed frame".to_owned(),
        };
        let json = serde_json::to_string(&msg).expect("serialize Commit");
        assert_eq!(
            json,
            r#"{"type":"commit","message":"fix: handle malformed frame"}"#
        );
        let parsed: ClientMessage = serde_json::from_str(&json).expect("deserialize Commit");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_commit_missing_message_is_rejected() {
        let err = serde_json::from_str::<ClientMessage>(r#"{"type":"commit"}"#);
        assert!(
            err.is_err(),
            "commit without a message must not deserialize"
        );
    }

    #[test]
    fn test_git_op_result_ok_roundtrip_for_each_write_op() {
        // One representative op per GitWriteOp variant, success case: `error`
        // must be omitted on the wire, never serialized as `null`.
        for op in [
            GitWriteOp::StageFile {
                path: "a.rs".to_owned(),
            },
            GitWriteOp::UnstageFile {
                path: "a.rs".to_owned(),
            },
            GitWriteOp::StageHunk {
                path: "a.rs".to_owned(),
                hunk_id: 42,
            },
            GitWriteOp::DiscardFile {
                path: "a.rs".to_owned(),
            },
            GitWriteOp::Commit,
        ] {
            let msg = DaemonMessage::GitOpResult {
                op: op.clone(),
                ok: true,
                error: None,
            };
            let json = serde_json::to_string(&msg).expect("serialize GitOpResult");
            assert!(json.contains(r#""type":"git_op_result""#));
            assert!(
                !json.contains("error"),
                "error must be omitted on success: {json}"
            );
            let parsed: DaemonMessage =
                serde_json::from_str(&json).expect("deserialize GitOpResult");
            assert_eq!(parsed, msg);
        }
    }

    #[test]
    fn test_git_op_result_error_roundtrip_carries_message() {
        let msg = DaemonMessage::GitOpResult {
            op: GitWriteOp::Commit,
            ok: false,
            error: Some("nothing staged".to_owned()),
        };
        let json = serde_json::to_string(&msg).expect("serialize GitOpResult error");
        assert!(json.contains(r#""kind":"commit""#));
        assert!(json.contains(r#""ok":false"#));
        assert!(json.contains(r#""error":"nothing staged""#));
        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize GitOpResult error");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::GitOpResult { op, ok, error } => {
                assert_eq!(op, GitWriteOp::Commit);
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("nothing staged"));
            }
            other => panic!("expected GitOpResult, got {other:?}"),
        }
    }

    #[test]
    fn test_git_write_op_stage_hunk_roundtrip_preserves_path_and_hunk_id() {
        let op = GitWriteOp::StageHunk {
            path: "src/lib.rs".to_owned(),
            hunk_id: 7,
        };
        let json = serde_json::to_string(&op).expect("serialize GitWriteOp::StageHunk");
        assert_eq!(
            json,
            r#"{"kind":"stage_hunk","path":"src/lib.rs","hunk_id":7}"#
        );
        let parsed: GitWriteOp =
            serde_json::from_str(&json).expect("deserialize GitWriteOp::StageHunk");
        assert_eq!(parsed, op);
    }

    #[test]
    fn test_git_write_op_kind_tag_is_rejected_when_unknown() {
        let err = serde_json::from_str::<GitWriteOp>(r#"{"kind":"frobnicate"}"#);
        assert!(
            err.is_err(),
            "unknown git write op kind must not deserialize"
        );
    }

    #[test]
    fn test_git_op_result_missing_ok_field_is_rejected() {
        let err = serde_json::from_str::<DaemonMessage>(
            r#"{"type":"git_op_result","op":{"kind":"commit"}}"#,
        );
        assert!(
            err.is_err(),
            "git_op_result without an ok field must not deserialize"
        );
    }

    #[test]
    fn test_hunk_fingerprint_is_deterministic() {
        let hunk = sample_hunk();
        assert_eq!(hunk_fingerprint(&hunk), hunk_fingerprint(&hunk));
    }

    #[test]
    fn test_hunk_fingerprint_differs_for_different_headers() {
        let mut moved = sample_hunk();
        moved.old_start += 1;
        moved.new_start += 1;
        assert_ne!(hunk_fingerprint(&sample_hunk()), hunk_fingerprint(&moved));
    }

    #[test]
    fn test_hunk_fingerprint_differs_for_same_shape_content_change() {
        // Same header, same line count and kinds, different text: a same-shape
        // content edit must still change the fingerprint (spec-review finding
        // 2 — a stale id must never be fuzzily matched by shape alone).
        let mut edited = sample_hunk();
        edited.lines[2].content = "TWO_EDITED".to_owned();
        assert_ne!(hunk_fingerprint(&sample_hunk()), hunk_fingerprint(&edited));
    }

    #[test]
    fn test_hunk_fingerprint_differs_for_line_kind_change() {
        // Same header and same text content, different role (e.g. context vs
        // add): the kind must be part of the hash, not just the text.
        let mut retagged = sample_hunk();
        retagged.lines[0].kind = DiffLineKind::Add;
        assert_ne!(
            hunk_fingerprint(&sample_hunk()),
            hunk_fingerprint(&retagged)
        );
    }

    #[test]
    fn test_hunk_fingerprint_no_line_boundary_ambiguity() {
        // Two hunks whose concatenated line content is identical but split
        // differently across lines must not collide — the per-line delimiter
        // must prevent boundary-shifting collisions.
        let joined = DiffHunk {
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 1,
            lines: vec![DiffLine {
                kind: DiffLineKind::Context,
                content: "ab".to_owned(),
            }],
        };
        let split = DiffHunk {
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 1,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    content: "a".to_owned(),
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    content: "b".to_owned(),
                },
            ],
        };
        assert_ne!(hunk_fingerprint(&joined), hunk_fingerprint(&split));
    }

    // ---- Session-list round-trip tests (docs/spec-session-switch.md) --------

    #[test]
    fn test_query_session_list_roundtrips() {
        let msg = ClientMessage::QuerySessionList;
        let json = serde_json::to_string(&msg).expect("serialize QuerySessionList");
        assert_eq!(json, r#"{"type":"query_session_list"}"#);
        let parsed: ClientMessage =
            serde_json::from_str(&json).expect("deserialize QuerySessionList");
        assert_eq!(parsed, msg);
    }

    #[test]
    fn test_session_list_reply_roundtrip_preserves_entries() {
        let msg = DaemonMessage::SessionListReply {
            sessions: vec![
                SessionEntry {
                    id: 0,
                    name: "rift".to_owned(),
                    windows: 3,
                    attached: true,
                },
                SessionEntry {
                    id: 4,
                    name: "my project".to_owned(),
                    windows: 1,
                    attached: false,
                },
            ],
        };
        let json = serde_json::to_string(&msg).expect("serialize SessionListReply");
        assert!(json.contains(r#""type":"session_list_reply""#));
        assert!(json.contains(r#""name":"rift""#));
        assert!(json.contains(r#""attached":true"#));

        let parsed: DaemonMessage =
            serde_json::from_str(&json).expect("deserialize SessionListReply");
        assert_eq!(parsed, msg);
        match parsed {
            DaemonMessage::SessionListReply { sessions } => {
                assert_eq!(sessions.len(), 2);
                assert_eq!(sessions[1].name, "my project");
                assert!(!sessions[1].attached);
            }
            other => panic!("expected SessionListReply, got {other:?}"),
        }
    }

    #[test]
    fn test_session_list_reply_empty_sessions_roundtrips() {
        // A server with no session cannot be attached to, but the reply shape
        // must still round-trip as an empty list, not vanish.
        let msg = DaemonMessage::SessionListReply { sessions: vec![] };
        let json = serde_json::to_string(&msg).expect("serialize empty SessionListReply");
        assert!(json.contains(r#""sessions":[]"#));
        assert_eq!(
            serde_json::from_str::<DaemonMessage>(&json).expect("deserialize SessionListReply"),
            msg
        );
    }

    #[test]
    fn test_session_list_reply_missing_sessions_field_is_rejected() {
        let err = serde_json::from_str::<DaemonMessage>(r#"{"type":"session_list_reply"}"#);
        assert!(
            err.is_err(),
            "a session-list reply without a sessions field must not deserialize"
        );
    }

    #[test]
    fn test_session_entry_malformed_field_types_are_rejected() {
        // A present-but-wrongly-typed field is a protocol error, never coerced.
        for json in [
            r#"{"id":"0","name":"rift","windows":1,"attached":true}"#,
            r#"{"id":0,"name":"rift","windows":"one","attached":true}"#,
            r#"{"id":0,"name":"rift","windows":1,"attached":1}"#,
            r#"{"id":0,"name":"rift","windows":1}"#,
        ] {
            assert!(
                serde_json::from_str::<SessionEntry>(json).is_err(),
                "malformed session entry must not deserialize: {json}"
            );
        }
    }
}

/// Pins the protocol message set: a stable FNV-1a hash over the serde-visible
/// surface of `ClientMessage` and `DaemonMessage` (container serde attributes,
/// variant names, field names, field TYPES — so a wire-breaking type change
/// also trips it), extracted from this crate's own source with comments and
/// whitespace stripped. Changing either enum without re-pinning fails
/// `cargo test -p rift-protocol`; the failure message instructs to bump
/// `PROTOCOL_VERSION` and re-pin (`docs/protocol.md` — Versioning policy).
#[cfg(test)]
mod fingerprint_tests {
    use super::PROTOCOL_FINGERPRINT;

    /// This crate's own source — the authoritative message-set definition.
    const PROTOCOL_SOURCE: &str = include_str!("lib.rs");

    /// Strips `//` line comments (doc comments included) from `source`,
    /// preserving line structure. The enum definitions contain no string
    /// literal with `//` or braces, so line-based stripping is exact for the
    /// extracted regions.
    fn strip_line_comments(source: &str) -> String {
        source
            .lines()
            .map(|line| line.find("//").map_or(line, |idx| &line[..idx]))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Extracts one enum's serde-visible surface from `source`: the
    /// `#[serde(...)]` container attributes directly above the declaration
    /// (non-serde attributes such as `#[derive(...)]` are not wire-visible
    /// and are skipped) plus the `pub enum <name>` declaration and its
    /// brace-delimited body, with comments stripped and all whitespace
    /// removed. Returns `None` when the declaration is missing or its body
    /// braces never balance (malformed input).
    fn enum_surface(source: &str, name: &str) -> Option<String> {
        let stripped = strip_line_comments(source);
        let decl = format!("pub enum {name}");
        let start = stripped.find(&decl)?;

        let body_open = start + stripped[start..].find('{')?;
        let mut depth = 0usize;
        let mut end = None;
        for (idx, ch) in stripped[body_open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    // The scan starts on the opening brace, so depth is
                    // always >= 1 here.
                    depth -= 1;
                    if depth == 0 {
                        end = Some(body_open + idx + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;

        let mut attrs: Vec<&str> = Vec::new();
        for line in stripped[..start].lines().rev() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("#[") {
                if trimmed.starts_with("#[serde") {
                    attrs.push(trimmed);
                }
                continue;
            }
            break;
        }
        attrs.reverse();

        let mut surface: String = attrs.concat();
        surface.push_str(&stripped[start..end]);
        surface.retain(|ch| !ch.is_whitespace());
        Some(surface)
    }

    /// FNV-1a 64-bit — the same tiny, dependency-free hash the daemon deploy
    /// fingerprint uses (`crates/ssh/src/deploy.rs`; not shared because `ssh`
    /// depends on `protocol`, never the reverse). Only stability matters here,
    /// not collision resistance.
    fn fnv1a_64(bytes: &[u8]) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x100_0000_01b3;
        let mut hash = FNV_OFFSET;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    /// The message-set fingerprint: FNV-1a over both message enums' surfaces,
    /// separated so content cannot shift between them without a hash change.
    fn message_set_fingerprint(source: &str) -> Option<u64> {
        let client = enum_surface(source, "ClientMessage")?;
        let daemon = enum_surface(source, "DaemonMessage")?;
        Some(fnv1a_64(format!("{client}|{daemon}").as_bytes()))
    }

    /// A sample enum shaped like the real message enums, for the trip-wire
    /// tests below (its own name, so it never collides with the real
    /// extraction).
    const SAMPLE_SOURCE: &str = r#"
/// A sample message enum.
#[derive(Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sample {
    /// Doc comment noise.
    Alpha { id: u32 },
    Beta { name: String, flag: bool },
}
"#;

    fn sample_fingerprint(source: &str) -> u64 {
        let surface = enum_surface(source, "Sample").expect("sample enum must extract");
        fnv1a_64(surface.as_bytes())
    }

    #[test]
    fn test_message_set_fingerprint_unchanged_source_matches_pin() {
        let actual = message_set_fingerprint(PROTOCOL_SOURCE)
            .expect("ClientMessage and DaemonMessage must be extractable from the crate source");
        assert_eq!(
            actual, PROTOCOL_FINGERPRINT,
            "the protocol message set changed (fingerprint 0x{actual:016x}, pinned \
             0x{PROTOCOL_FINGERPRINT:016x}): bump PROTOCOL_VERSION and re-pin \
             PROTOCOL_FINGERPRINT in crates/protocol/src/lib.rs (strict-equality \
             policy, docs/protocol.md)"
        );
    }

    #[test]
    fn test_enum_surface_real_enums_carry_field_types() {
        // The surface must include field TYPES (a wire-breaking type change
        // trips the fingerprint) and the container serde attributes.
        let client = enum_surface(PROTOCOL_SOURCE, "ClientMessage").expect("extract ClientMessage");
        assert!(client.starts_with(r##"#[serde(tag="type",rename_all="snake_case")]"##));
        assert!(client.contains("Attach{session:String,}"));
        assert!(client.contains("Hello{version:u32,}"));

        let daemon = enum_surface(PROTOCOL_SOURCE, "DaemonMessage").expect("extract DaemonMessage");
        assert!(daemon.contains("PaneOutput{pane_id:u32,bytes:Vec<u8>,}"));
        assert!(daemon.contains("Welcome{version:u32,}"));
    }

    #[test]
    fn test_enum_surface_variant_addition_changes_fingerprint() {
        let grown = SAMPLE_SOURCE.replace(
            "Beta { name: String, flag: bool },",
            "Beta { name: String, flag: bool },\n    Gamma { count: u64 },",
        );
        assert_ne!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(&grown)
        );
    }

    #[test]
    fn test_enum_surface_field_rename_changes_fingerprint() {
        let renamed = SAMPLE_SOURCE.replace("name: String", "title: String");
        assert_ne!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(&renamed)
        );
    }

    #[test]
    fn test_enum_surface_field_type_change_changes_fingerprint() {
        let retyped = SAMPLE_SOURCE.replace("id: u32", "id: u64");
        assert_ne!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(&retyped)
        );
    }

    #[test]
    fn test_enum_surface_serde_attribute_change_changes_fingerprint() {
        // Container serde attributes are wire-visible (the `type` tag), so
        // changing one must trip the fingerprint.
        let retagged = SAMPLE_SOURCE.replace(r#"tag = "type""#, r#"tag = "kind""#);
        assert_ne!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(&retagged)
        );
    }

    #[test]
    fn test_enum_surface_comment_and_whitespace_changes_keep_fingerprint() {
        // Doc comments, ordinary comments, and reformatting are not
        // wire-visible: the fingerprint must not produce false bumps for them.
        let reformatted = r#"
// Entirely different commentary.
#[derive(Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Sample {
    Alpha {
        id: u32
    },
    Beta {
        name: String, flag: bool
    },
}
"#;
        assert_eq!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(reformatted)
        );
    }

    #[test]
    fn test_enum_surface_derive_attribute_change_keeps_fingerprint() {
        // Derives are not wire-visible; adding one must not force a version
        // bump.
        let rederived = SAMPLE_SOURCE.replace(
            "#[derive(Debug, Clone, PartialEq)]",
            "#[derive(Debug, Clone, PartialEq, Eq)]",
        );
        assert_eq!(
            sample_fingerprint(SAMPLE_SOURCE),
            sample_fingerprint(&rederived)
        );
    }

    #[test]
    fn test_enum_surface_missing_enum_returns_none() {
        assert_eq!(enum_surface("pub struct NotAnEnum;", "Sample"), None);
        assert_eq!(enum_surface(SAMPLE_SOURCE, "Missing"), None);
    }

    #[test]
    fn test_enum_surface_unbalanced_braces_returns_none() {
        // A body whose braces never balance is malformed input, not a panic.
        assert_eq!(
            enum_surface("pub enum Broken { Alpha { id: u32 }", "Broken"),
            None
        );
        // A declaration with no body brace at all.
        assert_eq!(enum_surface("pub enum Broken", "Broken"), None);
    }
}
