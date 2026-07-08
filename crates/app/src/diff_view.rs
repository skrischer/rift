//! Virtualized unified diff view (`docs/spec-source-control.md`, issue #338).
//!
//! Renders the [`rift_protocol::DaemonMessage::FileDiff`] streamed for the path
//! the source-control panel's [`crate::source_control::SourceControlEvent::OpenDiff`]
//! selects: a single-column unified diff with add/remove/context styling from
//! theme tokens, or a placeholder for the binary/too-large sentinels
//! ([`rift_protocol::FileDiffPayload`]). Mirrors the editor's `OpenFile ->
//! FileContent` request/reply pattern (`crate::workspace`) — `open_diff` sends
//! a path on the `request_diff_tx` channel the workspace wires to
//! `ClientMessage::RequestDiff`, and `apply_file_diff` renders the daemon's
//! reply. Path-keyed, like the buffer channel: at most one diff is ever
//! inflight, so a reply is only applied while it still matches the currently
//! open path (a stale reply for an already-abandoned selection is dropped).
//!
//! [`flatten_hunks`] and [`DiffViewState::from_payload`] are pure, GPUI-free
//! functions — the hunk-to-rows flattening (line-number bookkeping per
//! [`DiffLineKind`]) and the sentinel mapping are unit-tested headless,
//! mirroring [`crate::problems_panel::ProblemsSummary::from_diagnostics`].
//! Rows are flattened once per reply (not re-derived per virtual-list frame,
//! unlike [`crate::problems_panel::ProblemsPanel`]'s smaller diagnostics set) —
//! the spec's size ceiling (~20k changed lines) makes a per-frame re-flatten
//! wasteful for the view this panel renders. [`flatten_hunks`] also derives one
//! [`HunkSummary`] per hunk (its content-fingerprint `hunk_id` plus added/
//! removed line counts) in the same pass — the header's `+n -m` total and mini
//! hunk squares (issue #547) read these instead of re-walking the rows.
//!
//! The header (issue #547, `docs/spec-source-control-write.md` §4) shows the
//! open file's name + directory, the aggregated `+n -m` line counts, one mini
//! square per hunk, and a Split|Unified segmented toggle whose preference is
//! persisted in the window-state store (`crate::window_state`) — mirroring
//! `crate::set_theme_mode_persisted`'s "apply, then best-effort persist"
//! shape. The `Split` mode (issue #548) renders the same hunks as two aligned
//! columns: per-side line-number gutters, add/delete tints, hatched
//! [`gpui::pattern_slash`] filler rows on the side a change block does not
//! reach, and word-level intra-line emphasis computed client-side by a
//! token-level longest-common-subsequence over each changed line pair. The
//! split row *structure* is flattened once per reply alongside the unified
//! rows ([`flatten_split_rows`]); the LCS runs only for the VISIBLE changed
//! pairs inside the virtual-list render callback, so no full re-diff happens
//! per scroll frame (`docs/spec-source-control-write.md`). Each hunk header row
//! also carries a `+ Stage hunk`
//! ghost button wired to [`rift_protocol::ClientMessage::StageHunk`] with the
//! hunk's own fingerprint — the daemon (`crates/daemon/src/git_write.rs`)
//! recomputes and verifies it before applying, so a stale id is rejected
//! rather than mis-staged.
//!
//! Agent-agnostic: this view only requests/displays a computed diff and sends
//! explicit user stage actions; it performs no other git write operations and
//! inspects no agent output.

use std::path::PathBuf;
use std::rc::Rc;

use flume::Sender;
use gpui::{
    div, pattern_slash, px, AnyElement, App, Context, EventEmitter, FocusHandle, Focusable, Hsla,
    IntoElement, ParentElement as _, Pixels, Render, SharedString, Size, Styled as _, Window,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_component::dock::{Panel, PanelEvent};
use gpui_component::{
    h_flex, v_virtual_list, ActiveTheme as _, IconName, Selectable as _, Sizable as _,
    VirtualListScrollHandle,
};
use rift_protocol::{hunk_fingerprint, ClientMessage, DiffHunk, DiffLineKind, FileDiffPayload};
use tracing::debug;

use crate::source_control::split_name_dir;
use crate::window_state::{self, DiffViewMode};

/// Stable, distinct dock-panel identity for the diff view (`Panel::panel_name`).
/// Once shipped this must not change — it is the persisted panel identifier.
pub const DIFF_VIEW_PANEL_NAME: &str = "diff-view";

/// Fixed row height for every diff row (hunk header and line alike) — a
/// uniform height keeps the virtual list's size vector trivial, mirroring
/// `ProblemsPanel::ROW_HEIGHT`.
const ROW_HEIGHT: Pixels = px(20.0);

/// Height of the fixed path header above the scrollable diff — roomier than
/// `ROW_HEIGHT` so the Split|Unified toggle's buttons fit comfortably (#547).
const HEADER_HEIGHT: Pixels = px(32.0);

/// Width of each line-number column.
const LINE_NUMBER_WIDTH: Pixels = px(44.0);

/// Side length of one hunk mini-square in the header (#547).
const HUNK_SQUARE_SIZE: Pixels = px(6.0);

/// Cap on the hunk-squares strip's width, so a file with dozens of hunks
/// clips instead of pushing the Split|Unified toggle off the header.
const HUNK_SQUARES_MAX_WIDTH: Pixels = px(160.0);

/// One flattened row of the virtualized diff list: either a hunk's `@@ ... @@`
/// header or one of its lines, addressed against both the old (HEAD) and new
/// (worktree) line numbering. Derived once from the streamed hunks by
/// [`flatten_hunks`] and held in [`DiffViewState::Hunks`] — never re-derived
/// per render frame.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffRow {
    HunkHeader {
        /// The [`hunk_fingerprint`] of the hunk this header opens — carried
        /// on the row so the row's `+ Stage hunk` button (#547) can send
        /// [`ClientMessage::StageHunk`] without re-deriving it from the
        /// header numbers alone (which omit line content).
        hunk_id: u64,
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
    },
    Line {
        kind: DiffLineKind,
        /// The line's 1-based position on the old (HEAD) side; `None` for an
        /// added line, which has no old-side position.
        old_line: Option<u32>,
        /// The line's 1-based position on the new (worktree) side; `None` for
        /// a removed line, which has no new-side position.
        new_line: Option<u32>,
        content: String,
    },
}

/// One hunk's header-strip summary (#547): its content-fingerprint `hunk_id`
/// (the same value carried on its [`DiffRow::HunkHeader`]) plus its added/
/// removed line counts, feeding the diff header's `+n -m` total and mini
/// hunk squares without a second walk over the flattened rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HunkSummary {
    hunk_id: u64,
    added: u32,
    removed: u32,
}

/// Flatten a [`FileDiffPayload::Hunks`]' hunks into the virtual list's row
/// sequence, computing each line's old/new line number by walking the hunk's
/// `old_start`/`new_start` forward: context and remove lines advance the old
/// counter, context and add lines advance the new counter — mirroring
/// unified-diff's own line-counting rule. Also derives one [`HunkSummary`]
/// per hunk in the same pass (#547). Pure and GPUI-free so it is
/// unit-testable headless.
fn flatten_hunks(hunks: Vec<DiffHunk>) -> (Vec<DiffRow>, Vec<HunkSummary>) {
    let mut rows = Vec::new();
    let mut summaries = Vec::with_capacity(hunks.len());
    for hunk in hunks {
        let hunk_id = hunk_fingerprint(&hunk);
        let added = hunk
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Add)
            .count() as u32;
        let removed = hunk
            .lines
            .iter()
            .filter(|line| line.kind == DiffLineKind::Remove)
            .count() as u32;
        summaries.push(HunkSummary {
            hunk_id,
            added,
            removed,
        });

        rows.push(DiffRow::HunkHeader {
            hunk_id,
            old_start: hunk.old_start,
            old_len: hunk.old_len,
            new_start: hunk.new_start,
            new_len: hunk.new_len,
        });

        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;
        for line in hunk.lines {
            let (old_num, new_num) = match line.kind {
                DiffLineKind::Context => {
                    let nums = (Some(old_line), Some(new_line));
                    old_line += 1;
                    new_line += 1;
                    nums
                }
                DiffLineKind::Remove => {
                    let nums = (Some(old_line), None);
                    old_line += 1;
                    nums
                }
                DiffLineKind::Add => {
                    let nums = (None, Some(new_line));
                    new_line += 1;
                    nums
                }
            };
            rows.push(DiffRow::Line {
                kind: line.kind,
                old_line: old_num,
                new_line: new_num,
                content: line.content,
            });
        }
    }
    (rows, summaries)
}

/// One side (old/HEAD or new/worktree) of a [`SplitRow::Pair`]: a real diff
/// line with its per-side line number, kind, and content. A `None` side of a
/// pair is a filler (hatched), not a [`SplitCell`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct SplitCell {
    kind: DiffLineKind,
    /// 1-based line number on this side (old-side for the left cell, new-side
    /// for the right cell).
    line_number: u32,
    content: String,
}

/// One flattened row of the side-by-side split view: a hunk header (rendered
/// full-width, identically to the unified view) or a pair of optional sides. A
/// context line pairs both sides with identical content; a change block pairs
/// the i-th removed line with the i-th added line, and the longer side's
/// surplus lines pair with a `None` (hatched filler) on the other side. Derived
/// once per reply by [`flatten_split_rows`], never re-derived per frame — the
/// word-level emphasis over a `(Remove, Add)` pair is the only per-frame work,
/// and only for visible rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SplitRow {
    HunkHeader {
        hunk_id: u64,
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
    },
    Pair {
        left: Option<SplitCell>,
        right: Option<SplitCell>,
    },
}

/// Emit one [`SplitRow::Pair`] per line of a change block, pairing the i-th
/// removed line with the i-th added line and padding the shorter side with
/// `None` (a hatched filler). Drains both buffers by value (no clone) so the
/// caller's accumulators are empty for the next block.
fn flush_change_block(
    rows: &mut Vec<SplitRow>,
    removes: &mut Vec<SplitCell>,
    adds: &mut Vec<SplitCell>,
) {
    let removes = std::mem::take(removes);
    let adds = std::mem::take(adds);
    let paired = removes.len().max(adds.len());
    let mut removes = removes.into_iter();
    let mut adds = adds.into_iter();
    for _ in 0..paired {
        rows.push(SplitRow::Pair {
            left: removes.next(),
            right: adds.next(),
        });
    }
}

/// Flatten hunks into the split view's row sequence: one full-width header per
/// hunk, then context lines paired on both sides and change blocks paired
/// removed-to-added with filler padding ([`flush_change_block`]). Line numbers
/// are walked exactly as [`flatten_hunks`] does. Pure and GPUI-free so it is
/// unit-testable headless; borrows the hunks so [`DiffViewState::from_payload`]
/// can still hand them to [`flatten_hunks`] afterwards without a clone.
fn flatten_split_rows(hunks: &[DiffHunk]) -> Vec<SplitRow> {
    let mut rows = Vec::new();
    for hunk in hunks {
        rows.push(SplitRow::HunkHeader {
            hunk_id: hunk_fingerprint(hunk),
            old_start: hunk.old_start,
            old_len: hunk.old_len,
            new_start: hunk.new_start,
            new_len: hunk.new_len,
        });

        let mut old_line = hunk.old_start;
        let mut new_line = hunk.new_start;
        let mut removes: Vec<SplitCell> = Vec::new();
        let mut adds: Vec<SplitCell> = Vec::new();
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Context => {
                    flush_change_block(&mut rows, &mut removes, &mut adds);
                    rows.push(SplitRow::Pair {
                        left: Some(SplitCell {
                            kind: DiffLineKind::Context,
                            line_number: old_line,
                            content: line.content.clone(),
                        }),
                        right: Some(SplitCell {
                            kind: DiffLineKind::Context,
                            line_number: new_line,
                            content: line.content.clone(),
                        }),
                    });
                    old_line += 1;
                    new_line += 1;
                }
                DiffLineKind::Remove => {
                    removes.push(SplitCell {
                        kind: DiffLineKind::Remove,
                        line_number: old_line,
                        content: line.content.clone(),
                    });
                    old_line += 1;
                }
                DiffLineKind::Add => {
                    adds.push(SplitCell {
                        kind: DiffLineKind::Add,
                        line_number: new_line,
                        content: line.content.clone(),
                    });
                    new_line += 1;
                }
            }
        }
        flush_change_block(&mut rows, &mut removes, &mut adds);
    }
    rows
}

/// One coalesced run of a changed line's content, tagged with whether it
/// differs from the paired line ([`word_emphasis`]). Adjacent tokens of the
/// same `emphasized` state are merged into a single span so a line renders as a
/// handful of elements, not one per token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmphasisSpan {
    text: String,
    emphasized: bool,
}

/// Product-of-token-counts ceiling above which [`word_emphasis`] skips the LCS
/// and falls back to a single un-emphasized span per side — a guard against a
/// pathological minified line making the per-frame DP expensive. The row tint
/// still conveys add/remove; only the intra-line highlight is dropped.
const MAX_EMPHASIS_TOKENS: usize = 512;

/// The character class a token coalesces on: word runs and whitespace runs each
/// collapse into one token; every other character is its own token, giving
/// word-level granularity for code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn char_class(c: char) -> CharClass {
    if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else if c.is_whitespace() {
        CharClass::Space
    } else {
        CharClass::Other
    }
}

/// Split a line into word/whitespace/other tokens for the word-level LCS.
/// Consecutive word characters and consecutive whitespace each form one token;
/// every other character stands alone.
fn tokenize(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut class: Option<CharClass> = None;
    for (i, c) in s.char_indices() {
        let this = char_class(c);
        let extends = matches!(
            (class, this),
            (Some(CharClass::Word), CharClass::Word) | (Some(CharClass::Space), CharClass::Space)
        );
        if extends {
            continue;
        }
        if let Some(start) = start {
            tokens.push(&s[start..i]);
        }
        start = Some(i);
        class = Some(this);
    }
    if let Some(start) = start {
        tokens.push(&s[start..]);
    }
    tokens
}

/// Append `text` to `spans`, merging into the previous span when it carries the
/// same `emphasized` state so runs stay coalesced.
fn push_span(spans: &mut Vec<EmphasisSpan>, text: &str, emphasized: bool) {
    if let Some(last) = spans.last_mut() {
        if last.emphasized == emphasized {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(EmphasisSpan {
        text: text.to_owned(),
        emphasized,
    });
}

/// Word-level intra-line emphasis for a `(removed, added)` line pair: a
/// token-level longest-common-subsequence marks the tokens unique to the old
/// side (emphasized in the removed line) and unique to the new side (emphasized
/// in the added line); shared tokens render plain. Returns `(old_spans,
/// new_spans)`. Pure and GPUI-free so it is unit-testable headless. Falls back
/// to a single un-emphasized span per side when either side is empty or the
/// token product exceeds [`MAX_EMPHASIS_TOKENS`].
fn word_emphasis(old: &str, new: &str) -> (Vec<EmphasisSpan>, Vec<EmphasisSpan>) {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let n = old_tokens.len();
    let m = new_tokens.len();

    let plain = || {
        let old_spans = if old.is_empty() {
            Vec::new()
        } else {
            vec![EmphasisSpan {
                text: old.to_owned(),
                emphasized: false,
            }]
        };
        let new_spans = if new.is_empty() {
            Vec::new()
        } else {
            vec![EmphasisSpan {
                text: new.to_owned(),
                emphasized: false,
            }]
        };
        (old_spans, new_spans)
    };

    if n == 0 || m == 0 || n.saturating_mul(m) > MAX_EMPHASIS_TOKENS {
        return plain();
    }

    // LCS length DP, filled bottom-up on a flat (n+1)x(m+1) grid.
    let stride = m + 1;
    let mut dp = vec![0u16; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let cell = if old_tokens[i] == new_tokens[j] {
                dp[(i + 1) * stride + (j + 1)] + 1
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + (j + 1)])
            };
            dp[i * stride + j] = cell;
        }
    }

    let mut old_spans = Vec::new();
    let mut new_spans = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_tokens[i] == new_tokens[j] {
            push_span(&mut old_spans, old_tokens[i], false);
            push_span(&mut new_spans, new_tokens[j], false);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * stride + j] >= dp[i * stride + (j + 1)] {
            push_span(&mut old_spans, old_tokens[i], true);
            i += 1;
        } else {
            push_span(&mut new_spans, new_tokens[j], true);
            j += 1;
        }
    }
    while i < n {
        push_span(&mut old_spans, old_tokens[i], true);
        i += 1;
    }
    while j < m {
        push_span(&mut new_spans, new_tokens[j], true);
        j += 1;
    }
    (old_spans, new_spans)
}

/// The diff view's current display state for the open path. `Empty` (no file
/// selected yet) and `Loading` (request sent, reply not yet in) render a
/// placeholder identically to the sentinels — never a partial/garbled render.
#[derive(Debug, Clone, PartialEq)]
enum DiffViewState {
    Empty,
    Loading,
    Hunks {
        rows: Vec<DiffRow>,
        /// The same hunks flattened for the side-by-side split renderer (#548),
        /// derived once alongside `rows` — the active `view_mode` selects which
        /// of the two the virtual list walks.
        split_rows: Vec<SplitRow>,
        /// One summary per hunk, in the same order as their
        /// [`DiffRow::HunkHeader`]s — feeds the header's `+n -m` total and
        /// mini hunk squares (#547).
        hunks: Vec<HunkSummary>,
    },
    Binary,
    TooLarge,
}

impl DiffViewState {
    /// Map a daemon [`FileDiffPayload`] reply onto the view's display state.
    /// Pure and GPUI-free so the binary/too-large sentinel handling is
    /// unit-testable headless, alongside [`flatten_hunks`].
    fn from_payload(payload: FileDiffPayload) -> Self {
        match payload {
            FileDiffPayload::Hunks { hunks } => {
                let split_rows = flatten_split_rows(&hunks);
                let (rows, hunks) = flatten_hunks(hunks);
                Self::Hunks {
                    rows,
                    split_rows,
                    hunks,
                }
            }
            FileDiffPayload::Binary => Self::Binary,
            FileDiffPayload::TooLarge => Self::TooLarge,
        }
    }
}

/// The action to take when a git-status tick's `changed`/`cleared` paths
/// (`docs/spec-source-control.md`'s refresh semantics, issue #339) are checked
/// against the diff view's currently open path: `Refresh` re-requests the
/// diff (the open file is still in the changed set, so its on-disk content
/// may have moved since the last reply); `Close` empties the view (the open
/// file left the changed set entirely — e.g. a commit landed); `None` when
/// neither list mentions the open path. Pure and GPUI-free so the decision is
/// unit-testable headless, mirroring [`flatten_hunks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitUpdateAction {
    None,
    Refresh,
    Close,
}

/// Decide [`GitUpdateAction`] for `open_path` given one `UpdateGitStatus`
/// tick's `changed`/`cleared` path lists. `Close` takes priority over
/// `Refresh` if a path were ever listed in both (the daemon's
/// `git_delta_messages` never does this — `changed` and `cleared` are
/// disjoint by construction — but `Close` is the safer default if it did).
fn git_update_action(open_path: &str, changed: &[String], cleared: &[String]) -> GitUpdateAction {
    if cleared.iter().any(|path| path == open_path) {
        GitUpdateAction::Close
    } else if changed.iter().any(|path| path == open_path) {
        GitUpdateAction::Refresh
    } else {
        GitUpdateAction::None
    }
}

/// The diff view: a virtualized, read-only unified diff of the currently
/// selected changed file, streamed from the daemon on request.
pub struct DiffView {
    /// The path currently open (selected in the source-control panel), if any.
    path: Option<String>,
    state: DiffViewState,
    /// Diff pull requests: a root-relative path the workspace forwards onto
    /// the protocol as `ClientMessage::RequestDiff` (mirrors
    /// `WorkspaceChannels::open_file_tx`).
    request_diff_tx: Sender<String>,
    /// Git write op sender: the per-hunk `+ Stage hunk` button (#547) sends
    /// `ClientMessage::StageHunk { path, hunk_id }` on it, mirroring
    /// `SourceControlPanel::git_op_tx` — the daemon's `ok`/`error` reply is
    /// not echoed into this view's state, the resulting change arrives via
    /// the existing push recompute.
    git_op_tx: Sender<ClientMessage>,
    /// The header's Split|Unified display preference (#547). `Split` only
    /// drives the toggle's selected state in this issue — the split renderer
    /// itself is issue #548's scope.
    view_mode: DiffViewMode,
    /// Where to persist `view_mode`; `None` degrades to in-memory-only for
    /// the session, mirroring `WorkspaceView::window_state_path`.
    window_state_path: Option<PathBuf>,
    focus_handle: FocusHandle,
    scroll_handle: VirtualListScrollHandle,
}

impl DiffView {
    pub fn new(
        request_diff_tx: Sender<String>,
        git_op_tx: Sender<ClientMessage>,
        window_state_path: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_mode = window_state_path
            .as_deref()
            .map(|path| window_state::load(path).diff_view_mode)
            .unwrap_or_default();
        Self {
            path: None,
            state: DiffViewState::Empty,
            request_diff_tx,
            git_op_tx,
            view_mode,
            window_state_path,
            focus_handle: cx.focus_handle(),
            scroll_handle: VirtualListScrollHandle::new(),
        }
    }

    /// Switch the Split|Unified preference and best-effort persist it — a
    /// no-op if it already matches, mirroring `open_diff`'s own guard against
    /// redundant work. Persistence failure only logs, matching
    /// `crate::persist_theme_mode`'s "the live change already applied
    /// regardless" contract.
    fn set_view_mode(&mut self, mode: DiffViewMode, cx: &mut Context<Self>) {
        if self.view_mode == mode {
            return;
        }
        self.view_mode = mode;
        if let Some(path) = &self.window_state_path {
            if let Err(e) = window_state::save_diff_view_mode(path, mode) {
                tracing::warn!(error = %e, "failed to persist diff view mode");
            }
        }
        cx.notify();
    }

    /// Send `StageHunk` for `hunk_id` against the currently open path — the
    /// `+ Stage hunk` button's action (#547), sent verbatim like
    /// `SourceControlPanel::send_op`; the daemon recomputes and verifies the
    /// fingerprint before applying, so a stale id is rejected, never
    /// mis-staged (`docs/spec-source-control-write.md`). A no-op when no diff
    /// is open — the button only ever renders while one is, but this keeps
    /// the method safe to call regardless.
    fn stage_hunk(&self, hunk_id: u64) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if let Err(e) = self
            .git_op_tx
            .try_send(ClientMessage::StageHunk { path, hunk_id })
        {
            debug!(error = %e, hunk_id, "failed to enqueue stage hunk");
        }
    }

    /// Open `path`'s diff: send the request onto the protocol and arm the
    /// loading state — but only when `path` differs from the currently open
    /// one. Re-requesting the already-open path (a git-status refresh tick,
    /// or the user reselecting the same row) keeps the rendered diff visible
    /// until the reply swaps it in (#487) — no "Loading diff..." flash for
    /// content that is most likely unchanged. Called by the workspace on
    /// [`crate::source_control::SourceControlEvent::OpenDiff`] and by
    /// [`Self::apply_git_update`]'s refresh path.
    pub fn open_diff(&mut self, path: String, cx: &mut Context<Self>) {
        if let Err(e) = self.request_diff_tx.try_send(path.clone()) {
            debug!(error = %e, %path, "failed to enqueue diff request");
        }
        if self.path.as_deref() != Some(path.as_str()) {
            self.path = Some(path);
            self.state = DiffViewState::Loading;
        }
        cx.notify();
    }

    /// Apply a `FileDiff` reply. A reply for a path that is no longer open
    /// (the user selected a different file before this one arrived) is
    /// dropped — mirrors `EditorView::load`'s stale-reply guard.
    pub fn apply_file_diff(&mut self, path: String, diff: FileDiffPayload, cx: &mut Context<Self>) {
        if self.path.as_deref() != Some(path.as_str()) {
            return;
        }
        self.state = DiffViewState::from_payload(diff);
        cx.notify();
    }

    /// React to an `UpdateGitStatus` tick for the currently open diff's path
    /// (`docs/spec-source-control.md`'s refresh semantics, #339): re-requests
    /// the diff when the open path is still in `changed` (its content may
    /// have moved since the last reply), closes the view when the open path
    /// left the changed set entirely (`cleared` — e.g. a commit landed), and
    /// is a no-op otherwise — including when no diff is open. Called by the
    /// workspace after it folds an `UpdateGitStatus` onto the file tree's
    /// model, alongside the existing re-request-on-reselection path
    /// (`open_diff`, driven by `SourceControlEvent::OpenDiff`).
    pub fn apply_git_update(
        &mut self,
        changed: &[String],
        cleared: &[String],
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.path.clone() else {
            return;
        };
        match git_update_action(&path, changed, cleared) {
            GitUpdateAction::Refresh => self.open_diff(path, cx),
            GitUpdateAction::Close => self.close(cx),
            GitUpdateAction::None => {}
        }
    }

    /// React to an `UpdateWorktree` tick for the currently open diff's path
    /// (#488): a content-only edit to an already-tracked file leaves its git
    /// status unchanged (`M` stays `M`), so `git_delta_messages` (daemon
    /// `crates/daemon/src/lib.rs`) never emits an `UpdateGitStatus` for it and
    /// `apply_git_update` alone never re-requests — an agent iterating on an
    /// open file's content leaves the diff stale. `UpdateWorktree` fires on
    /// every disk write regardless of git status (file-watch, not git-status,
    /// driven), so re-requesting here catches the content-only case that
    /// `apply_git_update` misses. `open_diff`'s same-path branch keeps the
    /// rendered diff visible until the reply swaps it in — no loading flash.
    /// A no-op when no diff is open or `changed` does not mention it.
    pub fn apply_content_update(&mut self, changed: &[String], cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if changed.iter().any(|changed_path| changed_path == &path) {
            self.open_diff(path, cx);
        }
    }

    /// Empty the view and forget the open path: the file it was showing left
    /// the changed set (e.g. a commit landed), so there is nothing left to
    /// review. No request is sent — mirrors `open_diff`'s state assignment in
    /// reverse, with no diff left to pull.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.path = None;
        self.state = DiffViewState::Empty;
        cx.notify();
    }

    fn placeholder(text: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .p(px(8.0))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    /// Render a hunk's `@@ ... @@` header row with its `+ Stage hunk` ghost
    /// button (#547). Shared by the unified ([`Self::render_row`]) and split
    /// ([`Self::render_split_row`]) views — the header spans the full width in
    /// both, so a hunk is stageable regardless of the active mode.
    fn render_hunk_header(
        hunk_id: u64,
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .h(ROW_HEIGHT)
            .items_center()
            .justify_between()
            .px(px(8.0))
            .bg(cx.theme().muted)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(format!(
                        "@@ -{old_start},{old_len} +{new_start},{new_len} @@"
                    )),
            )
            .child(
                Button::new(SharedString::from(format!("diff-stage-hunk-{hunk_id}")))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("Stage hunk")
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        cx.stop_propagation();
                        this.stage_hunk(hunk_id);
                    })),
            )
            .into_any_element()
    }

    /// Render one unified row: a hunk header ([`Self::render_hunk_header`]) or
    /// one line with its old/new line numbers, a +/-/space marker, and add/
    /// remove/context styling from theme tokens (`success`/`danger`, mirroring
    /// the file tree's git-status decoration — no diff-specific tokens
    /// invented).
    fn render_row(row: &DiffRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            DiffRow::HunkHeader {
                hunk_id,
                old_start,
                old_len,
                new_start,
                new_len,
            } => Self::render_hunk_header(*hunk_id, *old_start, *old_len, *new_start, *new_len, cx),
            DiffRow::Line {
                kind,
                old_line,
                new_line,
                content,
            } => {
                let (bg, marker): (Hsla, &str) = match kind {
                    DiffLineKind::Add => (cx.theme().success.opacity(0.14), "+"),
                    DiffLineKind::Remove => (cx.theme().danger.opacity(0.14), "-"),
                    DiffLineKind::Context => (cx.theme().background, " "),
                };
                let old_col = old_line.map(|n| n.to_string()).unwrap_or_default();
                let new_col = new_line.map(|n| n.to_string()).unwrap_or_default();

                div()
                    .h(ROW_HEIGHT)
                    .flex()
                    .items_center()
                    .bg(bg)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_xs()
                    .child(
                        div()
                            .w(LINE_NUMBER_WIDTH)
                            .flex_shrink_0()
                            .px(px(4.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(old_col),
                    )
                    .child(
                        div()
                            .w(LINE_NUMBER_WIDTH)
                            .flex_shrink_0()
                            .px(px(4.0))
                            .text_color(cx.theme().muted_foreground)
                            .child(new_col),
                    )
                    .child(
                        div()
                            .w(px(14.0))
                            .flex_shrink_0()
                            .text_color(cx.theme().foreground)
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_color(cx.theme().foreground)
                            .child(content.clone()),
                    )
                    .into_any_element()
            }
        }
    }

    /// Render one split row: a full-width hunk header, or a pair of side-by-
    /// side cells. A `(Remove, Add)` pair gets word-level emphasis
    /// ([`word_emphasis`]) computed here — visible rows only, so no full re-diff
    /// per scroll frame; every other pairing (context, or a change block's
    /// filler side) renders plain.
    fn render_split_row(row: &SplitRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            SplitRow::HunkHeader {
                hunk_id,
                old_start,
                old_len,
                new_start,
                new_len,
            } => Self::render_hunk_header(*hunk_id, *old_start, *old_len, *new_start, *new_len, cx),
            SplitRow::Pair { left, right } => {
                let (left_spans, right_spans) = match (left, right) {
                    (Some(l), Some(r))
                        if l.kind == DiffLineKind::Remove && r.kind == DiffLineKind::Add =>
                    {
                        let (ls, rs) = word_emphasis(&l.content, &r.content);
                        (Some(ls), Some(rs))
                    }
                    _ => (None, None),
                };
                div()
                    .h(ROW_HEIGHT)
                    .flex()
                    .child(Self::render_split_side(
                        left.as_ref(),
                        left_spans,
                        false,
                        cx,
                    ))
                    .child(Self::render_split_side(
                        right.as_ref(),
                        right_spans,
                        true,
                        cx,
                    ))
                    .into_any_element()
            }
        }
    }

    /// Render one side of a split row: a hatched filler when `cell` is `None`,
    /// otherwise a tinted line with its own line-number gutter, a +/-/space
    /// marker, and either coalesced emphasis spans (when `spans` is `Some`) or
    /// plain truncated content. `is_new` (the right/worktree column) draws the
    /// vertical divider between the two columns.
    fn render_split_side(
        cell: Option<&SplitCell>,
        spans: Option<Vec<EmphasisSpan>>,
        is_new: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut side = div().flex_1().min_w_0().h_full();
        if is_new {
            side = side.border_l_1().border_color(cx.theme().border);
        }

        let Some(cell) = cell else {
            // Filler: a faint hatch marks "no line on this side" (#548).
            return side
                .bg(pattern_slash(
                    cx.theme().muted_foreground.opacity(0.12),
                    4.0,
                    4.0,
                ))
                .into_any_element();
        };

        let (bg, marker, emph_bg): (Hsla, &str, Hsla) = match cell.kind {
            DiffLineKind::Add => (
                cx.theme().success.opacity(0.14),
                "+",
                cx.theme().success.opacity(0.30),
            ),
            DiffLineKind::Remove => (
                cx.theme().danger.opacity(0.14),
                "-",
                cx.theme().danger.opacity(0.30),
            ),
            DiffLineKind::Context => (cx.theme().background, " ", cx.theme().background),
        };

        let content = match spans {
            Some(spans) => h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .children(spans.into_iter().map(|span| {
                    let mut el = div().flex_none().child(span.text);
                    if span.emphasized {
                        el = el.bg(emph_bg).rounded(px(2.0));
                    }
                    el
                }))
                .into_any_element(),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(cell.content.clone())
                .into_any_element(),
        };

        side.flex()
            .items_center()
            .bg(bg)
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .w(LINE_NUMBER_WIDTH)
                    .flex_shrink_0()
                    .px(px(4.0))
                    .text_color(cx.theme().muted_foreground)
                    .child(cell.line_number.to_string()),
            )
            .child(
                div()
                    .w(px(14.0))
                    .flex_shrink_0()
                    .text_color(cx.theme().muted_foreground)
                    .child(marker),
            )
            .child(content)
            .into_any_element()
    }

    /// The fixed header above the scrollable diff (#547, §4): the open
    /// file's two-tone name + directory (reusing
    /// `crate::source_control::split_name_dir`), the aggregated `+n -m` line
    /// counts, one mini square per hunk, and the Split|Unified segmented
    /// toggle. `hunks` is the same [`HunkSummary`] slice `render` already
    /// holds — no re-derivation from the rows.
    fn render_header(
        &self,
        path: &str,
        hunks: &[HunkSummary],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (name, dir) = split_name_dir(path);
        let total_added: u32 = hunks.iter().map(|hunk| hunk.added).sum();
        let total_removed: u32 = hunks.iter().map(|hunk| hunk.removed).sum();

        let mut name_column = h_flex()
            .items_center()
            .gap(px(6.0))
            .min_w_0()
            .flex_1()
            .child(
                div()
                    .flex_none()
                    .text_color(cx.theme().foreground)
                    .child(name.to_owned()),
            );
        if !dir.is_empty() {
            name_column = name_column.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(dir.to_owned()),
            );
        }

        let stats = h_flex()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .child(
                div()
                    .text_color(cx.theme().success)
                    .child(format!("+{total_added}")),
            )
            .child(
                div()
                    .text_color(cx.theme().danger)
                    .child(format!("-{total_removed}")),
            );

        let squares = h_flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .max_w(HUNK_SQUARES_MAX_WIDTH)
            .overflow_hidden()
            .children(hunks.iter().map(|hunk| {
                let color = if hunk.added > 0 && hunk.removed > 0 {
                    cx.theme().warning
                } else if hunk.removed > 0 {
                    cx.theme().danger
                } else {
                    cx.theme().success
                };
                div()
                    .flex_none()
                    .size(HUNK_SQUARE_SIZE)
                    .rounded(px(1.0))
                    .bg(color)
            }));

        let toggle = ButtonGroup::new("diff-view-mode")
            .compact()
            .outline()
            .xsmall()
            .child(
                Button::new("diff-view-mode-unified")
                    .label("Unified")
                    .selected(self.view_mode == DiffViewMode::Unified),
            )
            .child(
                Button::new("diff-view-mode-split")
                    .label("Split")
                    .selected(self.view_mode == DiffViewMode::Split),
            )
            .on_click(cx.listener(|this, clicks: &Vec<usize>, _window, cx| {
                let mode = if clicks.contains(&1) {
                    DiffViewMode::Split
                } else {
                    DiffViewMode::Unified
                };
                this.set_view_mode(mode, cx);
            }));

        div()
            .h(HEADER_HEIGHT)
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .px(px(8.0))
            .text_sm()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(name_column)
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.0))
                    .child(stats)
                    .child(squares)
                    .child(toggle),
            )
            .into_any_element()
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for DiffView {}

impl Panel for DiffView {
    fn panel_name(&self) -> &'static str {
        DIFF_VIEW_PANEL_NAME
    }

    fn title(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        SharedString::from("Diff")
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(path) = self.path.clone() else {
            return Self::placeholder("Select a changed file to view its diff", cx);
        };

        let (rows, split_rows, hunks) = match &self.state {
            DiffViewState::Empty => {
                return Self::placeholder("Select a changed file to view its diff", cx)
            }
            DiffViewState::Loading => return Self::placeholder("Loading diff...", cx),
            DiffViewState::Binary => return Self::placeholder("Binary file - diff not shown", cx),
            DiffViewState::TooLarge => return Self::placeholder("Diff too large to display", cx),
            DiffViewState::Hunks { rows, .. } if rows.is_empty() => {
                return Self::placeholder("No changes", cx)
            }
            DiffViewState::Hunks {
                rows,
                split_rows,
                hunks,
            } => (rows, split_rows, hunks),
        };

        let use_split = self.view_mode == DiffViewMode::Split;
        let row_count = if use_split {
            split_rows.len()
        } else {
            rows.len()
        };
        let item_sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            (0..row_count)
                .map(|_| Size::new(px(0.0), ROW_HEIGHT))
                .collect(),
        );
        let header = self.render_header(&path, hunks, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div().flex_1().min_h_0().child(
                    v_virtual_list(
                        cx.entity().clone(),
                        "diff-view-list",
                        item_sizes,
                        move |this, visible_range, _window, cx| {
                            let DiffViewState::Hunks {
                                rows, split_rows, ..
                            } = &this.state
                            else {
                                return Vec::new();
                            };
                            if use_split {
                                visible_range
                                    .filter_map(|ix| {
                                        split_rows
                                            .get(ix)
                                            .map(|row| Self::render_split_row(row, cx))
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                visible_range
                                    .filter_map(|ix| {
                                        rows.get(ix).map(|row| Self::render_row(row, cx))
                                    })
                                    .collect::<Vec<_>>()
                            }
                        },
                    )
                    .track_scroll(&self.scroll_handle),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use rift_protocol::DiffLine;

    fn line(kind: DiffLineKind, content: &str) -> DiffLine {
        DiffLine {
            kind,
            content: content.to_owned(),
        }
    }

    /// Construct a `DiffView` wired to fresh unbounded channels and no
    /// window-state path (in-memory-only view-mode persistence) — the shared
    /// rig every `DiffView` test below builds on (#547).
    fn new_test_diff_view(
        cx: &mut TestAppContext,
    ) -> (
        Entity<DiffView>,
        flume::Receiver<String>,
        flume::Receiver<ClientMessage>,
    ) {
        let (request_diff_tx, request_diff_rx) = flume::unbounded();
        let (git_op_tx, git_op_rx) = flume::unbounded();
        let diff_view =
            cx.update(|cx| cx.new(|cx| DiffView::new(request_diff_tx, git_op_tx, None, cx)));
        (diff_view, request_diff_rx, git_op_rx)
    }

    // --- flatten_hunks ---

    #[test]
    fn test_flatten_single_hunk_numbers_context_add_remove_lines_correctly() {
        // 3 old lines (1..3), 3 new lines (1..3): line 2 removed and replaced
        // by a new "b2" line, mirroring a typical one-line edit.
        let hunk = DiffHunk {
            old_start: 1,
            old_len: 3,
            new_start: 1,
            new_len: 3,
            lines: vec![
                line(DiffLineKind::Context, "a1"),
                line(DiffLineKind::Remove, "a2"),
                line(DiffLineKind::Add, "b2"),
                line(DiffLineKind::Context, "a3"),
            ],
        };
        let hunk_id = hunk_fingerprint(&hunk);

        let (rows, hunks) = flatten_hunks(vec![hunk]);
        assert_eq!(
            rows,
            vec![
                DiffRow::HunkHeader {
                    hunk_id,
                    old_start: 1,
                    old_len: 3,
                    new_start: 1,
                    new_len: 3,
                },
                DiffRow::Line {
                    kind: DiffLineKind::Context,
                    old_line: Some(1),
                    new_line: Some(1),
                    content: "a1".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Remove,
                    old_line: Some(2),
                    new_line: None,
                    content: "a2".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Add,
                    old_line: None,
                    new_line: Some(2),
                    content: "b2".into(),
                },
                DiffRow::Line {
                    kind: DiffLineKind::Context,
                    old_line: Some(3),
                    new_line: Some(3),
                    content: "a3".into(),
                },
            ]
        );
        assert_eq!(
            hunks,
            vec![HunkSummary {
                hunk_id,
                added: 1,
                removed: 1,
            }],
            "one added and one removed line summarize into the hunk's totals"
        );
    }

    #[test]
    fn test_flatten_multiple_hunks_each_restart_from_their_own_start() {
        let hunks = vec![
            DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            },
            DiffHunk {
                old_start: 50,
                old_len: 1,
                new_start: 50,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a50")],
            },
        ];

        let (rows, summaries) = flatten_hunks(hunks);
        assert_eq!(rows.len(), 4, "2 headers + 2 lines");
        assert_eq!(summaries.len(), 2, "one summary per hunk");
        assert_eq!(
            rows[1],
            DiffRow::Line {
                kind: DiffLineKind::Context,
                old_line: Some(1),
                new_line: Some(1),
                content: "a1".into(),
            }
        );
        assert_eq!(
            rows[3],
            DiffRow::Line {
                kind: DiffLineKind::Context,
                old_line: Some(50),
                new_line: Some(50),
                content: "a50".into(),
            }
        );
    }

    #[test]
    fn test_flatten_added_file_lines_carry_no_old_line_number() {
        let hunks = vec![DiffHunk {
            old_start: 0,
            old_len: 0,
            new_start: 1,
            new_len: 2,
            lines: vec![
                line(DiffLineKind::Add, "new1"),
                line(DiffLineKind::Add, "new2"),
            ],
        }];

        let (rows, summaries) = flatten_hunks(hunks);
        for row in &rows[1..] {
            match row {
                DiffRow::Line {
                    old_line, new_line, ..
                } => {
                    assert_eq!(*old_line, None);
                    assert!(new_line.is_some());
                }
                other => panic!("expected a line row, got {other:?}"),
            }
        }
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].added, 2);
        assert_eq!(summaries[0].removed, 0);
    }

    #[test]
    fn test_flatten_empty_hunks_yields_no_rows_or_summaries() {
        let (rows, hunks) = flatten_hunks(vec![]);
        assert!(rows.is_empty());
        assert!(hunks.is_empty());
    }

    #[test]
    fn test_flatten_same_shape_different_content_hunks_yield_different_ids() {
        // Spec-review finding 2: a same-shape edit (identical header, different
        // line text) must fingerprint differently, so a stale `hunk_id`
        // (`docs/spec-source-control-write.md`) is never fuzzily matched.
        let base = DiffHunk {
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 1,
            lines: vec![line(DiffLineKind::Remove, "old")],
        };
        let mut changed = base.clone();
        changed.lines = vec![line(DiffLineKind::Remove, "different")];

        let (_, summaries_a) = flatten_hunks(vec![base]);
        let (_, summaries_b) = flatten_hunks(vec![changed]);
        assert_ne!(summaries_a[0].hunk_id, summaries_b[0].hunk_id);
    }

    // --- flatten_split_rows (#548) ---

    #[test]
    fn test_flatten_split_context_line_pairs_both_sides() {
        let hunk = DiffHunk {
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 1,
            lines: vec![line(DiffLineKind::Context, "a1")],
        };
        let rows = flatten_split_rows(&[hunk]);
        assert_eq!(rows.len(), 2, "one header + one context pair");
        assert_eq!(
            rows[1],
            SplitRow::Pair {
                left: Some(SplitCell {
                    kind: DiffLineKind::Context,
                    line_number: 1,
                    content: "a1".into(),
                }),
                right: Some(SplitCell {
                    kind: DiffLineKind::Context,
                    line_number: 1,
                    content: "a1".into(),
                }),
            }
        );
    }

    #[test]
    fn test_flatten_split_replacement_pairs_remove_with_add() {
        // a1 / (a2 -> b2) / a3 — the removed and added line land on the same row,
        // left and right, so the split view shows them opposite each other.
        let hunk = DiffHunk {
            old_start: 1,
            old_len: 3,
            new_start: 1,
            new_len: 3,
            lines: vec![
                line(DiffLineKind::Context, "a1"),
                line(DiffLineKind::Remove, "a2"),
                line(DiffLineKind::Add, "b2"),
                line(DiffLineKind::Context, "a3"),
            ],
        };
        let rows = flatten_split_rows(&[hunk]);
        assert_eq!(rows.len(), 4, "header + ctx + change pair + ctx");
        assert_eq!(
            rows[2],
            SplitRow::Pair {
                left: Some(SplitCell {
                    kind: DiffLineKind::Remove,
                    line_number: 2,
                    content: "a2".into(),
                }),
                right: Some(SplitCell {
                    kind: DiffLineKind::Add,
                    line_number: 2,
                    content: "b2".into(),
                }),
            }
        );
    }

    #[test]
    fn test_flatten_split_unequal_change_block_pads_shorter_side_with_filler() {
        // Two removed lines, one added: the second removed line pairs with a
        // filler (None) on the added side.
        let hunk = DiffHunk {
            old_start: 1,
            old_len: 2,
            new_start: 1,
            new_len: 1,
            lines: vec![
                line(DiffLineKind::Remove, "r1"),
                line(DiffLineKind::Remove, "r2"),
                line(DiffLineKind::Add, "a1"),
            ],
        };
        let rows = flatten_split_rows(&[hunk]);
        assert_eq!(rows.len(), 3, "header + two paired rows");
        assert_eq!(
            rows[1],
            SplitRow::Pair {
                left: Some(SplitCell {
                    kind: DiffLineKind::Remove,
                    line_number: 1,
                    content: "r1".into(),
                }),
                right: Some(SplitCell {
                    kind: DiffLineKind::Add,
                    line_number: 1,
                    content: "a1".into(),
                }),
            }
        );
        assert_eq!(
            rows[2],
            SplitRow::Pair {
                left: Some(SplitCell {
                    kind: DiffLineKind::Remove,
                    line_number: 2,
                    content: "r2".into(),
                }),
                right: None,
            },
            "the surplus removed line pairs with a filler"
        );
    }

    #[test]
    fn test_flatten_split_pure_addition_fillers_the_old_side() {
        let hunk = DiffHunk {
            old_start: 0,
            old_len: 0,
            new_start: 1,
            new_len: 2,
            lines: vec![
                line(DiffLineKind::Add, "new1"),
                line(DiffLineKind::Add, "new2"),
            ],
        };
        let rows = flatten_split_rows(&[hunk]);
        assert_eq!(rows.len(), 3);
        for (row, expected) in rows[1..].iter().zip([("new1", 1u32), ("new2", 2u32)]) {
            assert_eq!(
                row,
                &SplitRow::Pair {
                    left: None,
                    right: Some(SplitCell {
                        kind: DiffLineKind::Add,
                        line_number: expected.1,
                        content: expected.0.into(),
                    }),
                }
            );
        }
    }

    #[test]
    fn test_flatten_split_pure_deletion_fillers_the_new_side() {
        let hunk = DiffHunk {
            old_start: 1,
            old_len: 2,
            new_start: 0,
            new_len: 0,
            lines: vec![
                line(DiffLineKind::Remove, "d1"),
                line(DiffLineKind::Remove, "d2"),
            ],
        };
        let rows = flatten_split_rows(&[hunk]);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1],
            SplitRow::Pair {
                left: Some(SplitCell {
                    kind: DiffLineKind::Remove,
                    line_number: 1,
                    content: "d1".into(),
                }),
                right: None,
            }
        );
    }

    // --- tokenize / word_emphasis (#548) ---

    fn reconstruct(spans: &[EmphasisSpan]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn emphasized_text(spans: &[EmphasisSpan]) -> String {
        spans
            .iter()
            .filter(|s| s.emphasized)
            .map(|s| s.text.as_str())
            .collect()
    }

    #[test]
    fn test_tokenize_splits_words_whitespace_and_punctuation() {
        assert_eq!(
            tokenize("foo bar_baz(x)"),
            vec!["foo", " ", "bar_baz", "(", "x", ")"]
        );
    }

    #[test]
    fn test_tokenize_empty_string_yields_no_tokens() {
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn test_word_emphasis_identical_lines_have_no_emphasis() {
        let (old, new) = word_emphasis("let x = 1;", "let x = 1;");
        assert_eq!(emphasized_text(&old), "");
        assert_eq!(emphasized_text(&new), "");
        assert_eq!(reconstruct(&old), "let x = 1;");
        assert_eq!(reconstruct(&new), "let x = 1;");
    }

    #[test]
    fn test_word_emphasis_single_changed_word_emphasizes_only_that_token() {
        let (old, new) = word_emphasis("let x = 1;", "let y = 1;");
        assert_eq!(emphasized_text(&old), "x", "only the old token differs");
        assert_eq!(emphasized_text(&new), "y", "only the new token differs");
        assert_eq!(reconstruct(&old), "let x = 1;");
        assert_eq!(reconstruct(&new), "let y = 1;");
    }

    #[test]
    fn test_word_emphasis_pure_insertion_emphasizes_only_the_new_tokens() {
        let (old, new) = word_emphasis("foo", "foo bar");
        assert_eq!(emphasized_text(&old), "");
        assert_eq!(emphasized_text(&new), " bar");
        assert_eq!(reconstruct(&new), "foo bar");
    }

    #[test]
    fn test_word_emphasis_empty_old_falls_back_to_plain_new() {
        let (old, new) = word_emphasis("", "added");
        assert!(old.is_empty(), "an empty side has no spans");
        assert_eq!(
            new,
            vec![EmphasisSpan {
                text: "added".into(),
                emphasized: false,
            }]
        );
    }

    #[test]
    fn test_word_emphasis_over_token_cap_falls_back_to_plain() {
        // Far past MAX_EMPHASIS_TOKENS: the two sides are wholly different, yet
        // the guard returns one un-emphasized span per side instead of running
        // the DP.
        let old: String = (0..30).map(|i| format!("w{i} ")).collect();
        let new: String = (0..30).map(|i| format!("v{i} ")).collect();
        let (old_spans, new_spans) = word_emphasis(&old, &new);
        assert_eq!(
            old_spans,
            vec![EmphasisSpan {
                text: old.clone(),
                emphasized: false,
            }]
        );
        assert_eq!(
            new_spans,
            vec![EmphasisSpan {
                text: new.clone(),
                emphasized: false,
            }]
        );
    }

    // --- DiffViewState::from_payload ---

    #[test]
    fn test_from_payload_hunks_flattens_into_rows() {
        let payload = FileDiffPayload::Hunks {
            hunks: vec![DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            }],
        };
        match DiffViewState::from_payload(payload) {
            DiffViewState::Hunks {
                rows,
                split_rows,
                hunks,
            } => {
                assert_eq!(rows.len(), 2);
                assert_eq!(hunks.len(), 1);
                // One header + one context line, paired on both sides.
                assert_eq!(split_rows.len(), 2);
            }
            other => panic!("expected Hunks state, got {other:?}"),
        }
    }

    #[test]
    fn test_from_payload_empty_hunks_yields_empty_rows_not_a_sentinel() {
        // Empty `hunks` means "identical to HEAD", not a sentinel — the render
        // path shows "No changes" for this, distinct from binary/too-large.
        match DiffViewState::from_payload(FileDiffPayload::Hunks { hunks: vec![] }) {
            DiffViewState::Hunks {
                rows,
                split_rows,
                hunks,
            } => {
                assert!(rows.is_empty());
                assert!(hunks.is_empty());
                assert!(split_rows.is_empty());
            }
            other => panic!("expected an empty Hunks state, got {other:?}"),
        }
    }

    #[test]
    fn test_from_payload_binary_and_too_large_map_to_their_sentinels() {
        assert_eq!(
            DiffViewState::from_payload(FileDiffPayload::Binary),
            DiffViewState::Binary
        );
        assert_eq!(
            DiffViewState::from_payload(FileDiffPayload::TooLarge),
            DiffViewState::TooLarge
        );
    }

    // --- git_update_action ---

    #[test]
    fn test_git_update_action_refreshes_when_open_path_is_still_changed() {
        assert_eq!(
            git_update_action("a.rs", &["a.rs".to_owned(), "b.rs".to_owned()], &[]),
            GitUpdateAction::Refresh
        );
    }

    #[test]
    fn test_git_update_action_closes_when_open_path_left_the_changed_set() {
        assert_eq!(
            git_update_action("a.rs", &[], &["a.rs".to_owned()]),
            GitUpdateAction::Close
        );
    }

    #[test]
    fn test_git_update_action_close_wins_when_open_path_is_in_both_lists() {
        // Not a shape the daemon actually produces (`changed`/`cleared` are
        // disjoint by construction), but `Close` is the safer default if it
        // ever happened.
        assert_eq!(
            git_update_action("a.rs", &["a.rs".to_owned()], &["a.rs".to_owned()]),
            GitUpdateAction::Close
        );
    }

    #[test]
    fn test_git_update_action_none_when_open_path_is_not_mentioned() {
        assert_eq!(
            git_update_action("a.rs", &["b.rs".to_owned()], &["c.rs".to_owned()]),
            GitUpdateAction::None
        );
    }

    #[test]
    fn test_git_update_action_none_when_both_lists_are_empty() {
        assert_eq!(git_update_action("a.rs", &[], &[]), GitUpdateAction::None);
    }

    // --- DiffView::apply_git_update / close ---

    #[gpui::test]
    fn test_apply_git_update_refreshes_the_still_open_path(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        assert_eq!(
            rx.try_recv().expect("open_diff sends the initial request"),
            "a.rs"
        );

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["a.rs".to_owned()], &[], cx);
            });
        });

        assert_eq!(
            rx.try_recv()
                .expect("a change tick for the open path re-requests its diff"),
            "a.rs"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_git_update_closes_the_path_that_left_the_changed_set(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&[], &["a.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "leaving the changed set sends no new diff request"
        );
        cx.update(|cx| {
            let view = diff_view.read(cx);
            assert!(view.path.is_none());
            assert_eq!(view.state, DiffViewState::Empty);
        });
    }

    #[gpui::test]
    fn test_apply_git_update_is_a_no_op_when_no_diff_is_open(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["a.rs".to_owned()], &["b.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "no diff open means no request is sent"
        );
        cx.update(|cx| {
            assert!(diff_view.read(cx).path.is_none());
        });
    }

    // --- DiffView::apply_content_update (#488) ---

    #[gpui::test]
    fn test_apply_content_update_refreshes_the_open_path_on_a_content_only_change(
        cx: &mut TestAppContext,
    ) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        assert_eq!(
            rx.try_recv().expect("open_diff sends the initial request"),
            "a.rs"
        );

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["a.rs".to_owned()], cx);
            });
        });

        assert_eq!(
            rx.try_recv()
                .expect("a content-only tick for the open path re-requests its diff"),
            "a.rs"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_content_update_ignores_a_tick_that_does_not_mention_the_open_path(
        cx: &mut TestAppContext,
    ) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["b.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "a tick for an unrelated path sends no new diff request"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    #[gpui::test]
    fn test_apply_content_update_is_a_no_op_when_no_diff_is_open(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_content_update(&["a.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "no diff open means no request is sent"
        );
        cx.update(|cx| {
            assert!(diff_view.read(cx).path.is_none());
        });
    }

    // --- DiffView::open_diff refresh semantics (#487) ---

    fn one_context_line_payload() -> FileDiffPayload {
        FileDiffPayload::Hunks {
            hunks: vec![DiffHunk {
                old_start: 1,
                old_len: 1,
                new_start: 1,
                new_len: 1,
                lines: vec![line(DiffLineKind::Context, "a1")],
            }],
        }
    }

    #[gpui::test]
    fn test_open_diff_same_path_keeps_rendered_diff_until_reply(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
            });
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });

        assert_eq!(
            rx.try_recv().expect("re-opening the same path re-requests"),
            "a.rs"
        );
        cx.update(|cx| {
            assert!(
                matches!(diff_view.read(cx).state, DiffViewState::Hunks { .. }),
                "the rendered diff stays visible instead of flashing Loading"
            );
        });
    }

    #[gpui::test]
    fn test_open_diff_different_path_arms_loading(cx: &mut TestAppContext) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
                view.open_diff("b.rs".to_owned(), cx);
            });
        });

        rx.try_recv().expect("open_diff sends the initial request");
        assert_eq!(
            rx.try_recv().expect("opening a new path sends its request"),
            "b.rs"
        );
        cx.update(|cx| {
            let view = diff_view.read(cx);
            assert_eq!(view.path.as_deref(), Some("b.rs"));
            assert_eq!(
                view.state,
                DiffViewState::Loading,
                "a different path must not show the previous file's diff"
            );
        });
    }

    #[gpui::test]
    fn test_apply_git_update_refresh_keeps_rendered_diff_then_reply_swaps_it(
        cx: &mut TestAppContext,
    ) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.apply_file_diff("a.rs".to_owned(), one_context_line_payload(), cx);
                view.apply_git_update(&["a.rs".to_owned()], &[], cx);
            });
        });

        rx.try_recv().expect("open_diff sends the initial request");
        rx.try_recv()
            .expect("the refresh tick re-requests the diff");
        cx.update(|cx| {
            assert!(
                matches!(diff_view.read(cx).state, DiffViewState::Hunks { .. }),
                "the refresh tick keeps the rendered diff visible"
            );
        });

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_file_diff("a.rs".to_owned(), FileDiffPayload::Binary, cx);
            });
        });
        cx.update(|cx| {
            assert_eq!(
                diff_view.read(cx).state,
                DiffViewState::Binary,
                "the replacement reply still swaps the view"
            );
        });
    }

    #[gpui::test]
    fn test_apply_git_update_ignores_a_tick_that_does_not_mention_the_open_path(
        cx: &mut TestAppContext,
    ) {
        let (diff_view, rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| view.open_diff("a.rs".to_owned(), cx));
        });
        rx.try_recv().expect("open_diff sends the initial request");

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.apply_git_update(&["other.rs".to_owned()], &["another.rs".to_owned()], cx);
            });
        });

        assert!(
            rx.try_recv().is_err(),
            "an update naming unrelated paths sends no new request"
        );
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).path.as_deref(), Some("a.rs"));
        });
    }

    // --- DiffView::stage_hunk (#547) ---

    #[gpui::test]
    fn test_stage_hunk_sends_stage_hunk_for_the_open_path(cx: &mut TestAppContext) {
        let (diff_view, _rx, git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.open_diff("a.rs".to_owned(), cx);
                view.stage_hunk(42);
            });
        });

        assert_eq!(
            git_op_rx.drain().collect::<Vec<_>>(),
            vec![ClientMessage::StageHunk {
                path: "a.rs".to_owned(),
                hunk_id: 42,
            }]
        );
    }

    #[gpui::test]
    fn test_stage_hunk_is_a_no_op_when_no_diff_is_open(cx: &mut TestAppContext) {
        let (diff_view, _rx, git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, _cx| view.stage_hunk(42));
        });

        assert!(
            git_op_rx.drain().next().is_none(),
            "no diff open means no StageHunk is sent"
        );
    }

    // --- DiffView::set_view_mode / persisted restore (#547) ---

    #[gpui::test]
    fn test_set_view_mode_is_a_no_op_for_the_already_active_mode(cx: &mut TestAppContext) {
        let (diff_view, _rx, _git_op_rx) = new_test_diff_view(cx);

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                assert_eq!(view.view_mode, DiffViewMode::Unified, "the default mode");
                view.set_view_mode(DiffViewMode::Unified, cx);
                assert_eq!(view.view_mode, DiffViewMode::Unified);
            });
        });
    }

    #[gpui::test]
    fn test_set_view_mode_switches_and_persists_to_the_state_path(cx: &mut TestAppContext) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "rift-app-diff-view-mode-{}-{}.json",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);

        let (request_diff_tx, _request_diff_rx) = flume::unbounded();
        let (git_op_tx, _git_op_rx) = flume::unbounded();
        let diff_view = cx.update(|cx| {
            cx.new(|cx| DiffView::new(request_diff_tx, git_op_tx, Some(path.clone()), cx))
        });

        cx.update(|cx| {
            diff_view.update(cx, |view, cx| {
                view.set_view_mode(DiffViewMode::Split, cx);
                assert_eq!(view.view_mode, DiffViewMode::Split);
            });
        });

        assert_eq!(
            window_state::load(&path).diff_view_mode,
            DiffViewMode::Split,
            "the toggle persists into the window-state store"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[gpui::test]
    fn test_new_restores_a_persisted_view_mode(cx: &mut TestAppContext) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "rift-app-diff-view-mode-restore-{}-{}.json",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);
        window_state::save_diff_view_mode(&path, DiffViewMode::Split)
            .expect("seed a persisted Split preference");

        let (request_diff_tx, _request_diff_rx) = flume::unbounded();
        let (git_op_tx, _git_op_rx) = flume::unbounded();
        let diff_view = cx.update(|cx| {
            cx.new(|cx| DiffView::new(request_diff_tx, git_op_tx, Some(path.clone()), cx))
        });

        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).view_mode, DiffViewMode::Split);
        });

        let _ = std::fs::remove_file(&path);
    }

    #[gpui::test]
    fn test_new_defaults_to_unified_without_a_window_state_path(cx: &mut TestAppContext) {
        let (diff_view, _rx, _git_op_rx) = new_test_diff_view(cx);
        cx.update(|cx| {
            assert_eq!(diff_view.read(cx).view_mode, DiffViewMode::Unified);
        });
    }
}
