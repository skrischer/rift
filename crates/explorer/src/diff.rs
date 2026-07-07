//! Per-file diff: the current on-disk worktree content of a path vs its HEAD
//! blob, computed with `gix`'s `blob-diff` feature (`gix-imara-diff`).
//!
//! [`compute`] always diffs worktree-vs-HEAD, regardless of staging state — the
//! same working-tree-vs-HEAD review diff for every path, matching
//! `docs/spec-source-control.md`. A path absent from HEAD (an added/untracked
//! file) diffs against empty content; a path absent from the worktree (a
//! deleted file) diffs to empty. Binary content and oversized diffs return a
//! sentinel instead of a structured diff, so neither side ever materializes an
//! unbounded structure.

use std::path::Path;

use gix::diff::blob::unified_diff::{
    ConsumeHunk, ContextSize, DiffLineKind as GixDiffLineKind, HunkHeader,
};
use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput, UnifiedDiff};

use crate::{ExplorerError, Result};

/// Diffs whose either side exceeds this many bytes return [`FileDiff::TooLarge`]
/// before a line diff is attempted.
const MAX_SIDE_BYTES: usize = 2 * 1024 * 1024;
/// Diffs with more changed (added + removed) lines than this return
/// [`FileDiff::TooLarge`] (the Hunk 25k-line perf bar; `spec-source-control.md`).
const MAX_CHANGED_LINES: usize = 20_000;
/// Git's own binary heuristic: a NUL byte within the first 8000 bytes.
const BINARY_SNIFF_LEN: usize = 8000;

/// One line's role within a [`DiffHunk`], mirroring `rift_protocol::DiffLineKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Present on both sides, shown for surrounding context.
    Context,
    /// Present only on the new (worktree) side.
    Add,
    /// Present only on the old (HEAD) side.
    Remove,
}

/// One line of a [`DiffHunk`]: its role plus content, with the line terminator
/// stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub content: String,
}

/// A contiguous run of unified-diff lines, addressed against both the old
/// (HEAD) and new (worktree) line numbering (both 1-based, matching unified
/// diff / `git diff` hunk headers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

/// The outcome of diffing one path's current worktree content against HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDiff {
    /// A structured line diff, empty when both sides are identical.
    Hunks(Vec<DiffHunk>),
    /// Either side is binary (non-UTF-8, or a NUL byte within the first 8000
    /// bytes) — no line diff is produced.
    Binary,
    /// The diff exceeds the size ceiling (~20k changed lines or ~2MB per
    /// side) — too large to stream as a structured diff.
    TooLarge,
}

/// Diff the current on-disk content of `relative` (a path relative to `root`,
/// the same key space as the worktree entries) against its blob at HEAD.
///
/// Returns [`ExplorerError::GitError`] only when the repository or its objects
/// cannot be read; a path new to HEAD or removed from the worktree is not an
/// error — it diffs against empty content on the missing side.
pub fn compute(root: &Path, relative: &Path) -> Result<FileDiff> {
    let repo = gix::open(root)
        .map_err(|e| ExplorerError::GitError(format!("open {}: {e}", root.display())))?;
    compute_in_repo(&repo, root, relative)
}

/// Like [`compute`], but reuses an already-open `repo` instead of opening one
/// per call — the worktree-status recompute (`git.rs`'s line totals) diffs
/// every changed path in one pass and must not reopen the repository each
/// time.
pub(crate) fn compute_in_repo(
    repo: &gix::Repository,
    root: &Path,
    relative: &Path,
) -> Result<FileDiff> {
    let old_bytes = head_blob(repo, relative)?;
    let new_bytes = read_worktree(root, relative)?;
    diff_bytes(&old_bytes, &new_bytes, relative)
}

/// Like [`compute_in_repo`], but diffs `relative`'s current worktree content
/// against a specific blob `old_id` instead of the HEAD blob at `relative`'s
/// own path. A rename's destination path has no HEAD-tree entry (the path is
/// new to HEAD), but the rewrite that produced it carries the *source*
/// blob's id (`git.rs`'s `Change::Rewrite::source_id`) — diffing against that
/// instead means a pure rename (identical content) yields no hunks, rather
/// than a full add against an empty old side.
pub(crate) fn compute_rewrite(
    repo: &gix::Repository,
    root: &Path,
    old_id: gix::ObjectId,
    relative: &Path,
) -> Result<FileDiff> {
    let old_bytes = blob_bytes(repo, old_id, relative)?;
    let new_bytes = read_worktree(root, relative)?;
    diff_bytes(&old_bytes, &new_bytes, relative)
}

/// Read `relative`'s current worktree content under `root`, or empty when the
/// path is absent (a deleted file diffs to empty on the new side).
pub(crate) fn read_worktree(root: &Path, relative: &Path) -> Result<Vec<u8>> {
    match std::fs::read(root.join(relative)) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ExplorerError::GitError(format!(
            "read {}: {e}",
            relative.display()
        ))),
    }
}

/// Diff two byte buffers into a [`FileDiff`], applying the size/binary
/// sentinels before attempting a line diff. `relative` is used only for error
/// messages.
pub(crate) fn diff_bytes(old_bytes: &[u8], new_bytes: &[u8], relative: &Path) -> Result<FileDiff> {
    if old_bytes.len().max(new_bytes.len()) > MAX_SIDE_BYTES {
        return Ok(FileDiff::TooLarge);
    }

    let (Some(old_text), Some(new_text)) = (as_text(old_bytes), as_text(new_bytes)) else {
        return Ok(FileDiff::Binary);
    };

    let input = InternedInput::new(old_text, new_text);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);

    let changed_lines = diff.count_removals() as usize + diff.count_additions() as usize;
    if changed_lines > MAX_CHANGED_LINES {
        return Ok(FileDiff::TooLarge);
    }

    let hunks = UnifiedDiff::new(
        &diff,
        &input,
        HunkCollector::default(),
        ContextSize::symmetrical(3),
    )
    .consume()
    .map_err(|e| ExplorerError::GitError(format!("render diff {}: {e}", relative.display())))?;

    Ok(FileDiff::Hunks(hunks))
}

/// Read `relative`'s blob content at HEAD, or an empty `Vec` when HEAD is
/// unborn or the path does not exist there (an added/untracked file).
pub(crate) fn head_blob(repo: &gix::Repository, relative: &Path) -> Result<Vec<u8>> {
    let tree_id = repo
        .head_tree_id_or_empty()
        .map_err(|e| ExplorerError::GitError(format!("head tree: {e}")))?;
    let tree = tree_id
        .object()
        .map_err(|e| ExplorerError::GitError(format!("read tree {tree_id}: {e}")))?
        .try_into_tree()
        .map_err(|e| ExplorerError::GitError(format!("{tree_id} is not a tree: {e}")))?;
    let Some(entry) = tree
        .lookup_entry_by_path(relative)
        .map_err(|e| ExplorerError::GitError(format!("tree lookup {}: {e}", relative.display())))?
    else {
        return Ok(Vec::new());
    };
    let mut blob = entry
        .object()
        .map_err(|e| ExplorerError::GitError(format!("read blob {}: {e}", relative.display())))?
        .try_into_blob()
        .map_err(|e| {
            ExplorerError::GitError(format!("{} is not a blob: {e}", relative.display()))
        })?;
    Ok(blob.take_data())
}

/// Read a specific blob's content by id — used by [`compute_rewrite`] to read
/// a rename's source content, which lives at a different path (or no longer
/// exists at any path) than the one being diffed.
fn blob_bytes(repo: &gix::Repository, id: gix::ObjectId, relative: &Path) -> Result<Vec<u8>> {
    let mut blob = repo
        .find_object(id)
        .map_err(|e| {
            ExplorerError::GitError(format!("read blob {id} ({}): {e}", relative.display()))
        })?
        .try_into_blob()
        .map_err(|e| {
            ExplorerError::GitError(format!("{id} ({}) is not a blob: {e}", relative.display()))
        })?;
    Ok(blob.take_data())
}

/// `bytes` as UTF-8 text, or `None` when it looks binary: a NUL byte within
/// the first [`BINARY_SNIFF_LEN`] bytes (git's own heuristic), or invalid
/// UTF-8 anywhere.
fn as_text(bytes: &[u8]) -> Option<&str> {
    let sniff_len = bytes.len().min(BINARY_SNIFF_LEN);
    if bytes[..sniff_len].contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

/// Collects [`gix`]'s unified-diff hunks into [`DiffHunk`]s.
#[derive(Default)]
struct HunkCollector {
    hunks: Vec<DiffHunk>,
}

impl ConsumeHunk for HunkCollector {
    type Out = Vec<DiffHunk>;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(GixDiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        self.hunks.push(DiffHunk {
            old_start: header.before_hunk_start,
            old_len: header.before_hunk_len,
            new_start: header.after_hunk_start,
            new_len: header.after_hunk_len,
            lines: lines
                .iter()
                .map(|(kind, content)| DiffLine {
                    kind: line_kind(*kind),
                    content: strip_newline(content),
                })
                .collect(),
        });
        Ok(())
    }

    fn finish(self) -> Self::Out {
        self.hunks
    }
}

fn line_kind(kind: GixDiffLineKind) -> DiffLineKind {
    match kind {
        GixDiffLineKind::Context => DiffLineKind::Context,
        GixDiffLineKind::Add => DiffLineKind::Add,
        GixDiffLineKind::Remove => DiffLineKind::Remove,
    }
}

/// Split `bytes` into lines the exact way `gix`'s line diff tokenizes them
/// (`gix-imara-diff`'s `ByteLines`): break after every `\n`, keeping the
/// terminator on its line, and emit a final unterminated line for trailing
/// content. Empty input yields no lines. Because the line indexing is
/// identical to the tokenizer that produced a [`DiffHunk`]'s header numbers,
/// slicing a real buffer by those numbers reconstructs bytes exactly —
/// including the `\ No newline at end of file` case, where the last line
/// carries no terminator. Used by the hunk-staging reapply to splice real
/// HEAD / worktree line bytes rather than reconstructing from the hunk's
/// newline-stripped [`DiffLine`] content.
pub(crate) fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            lines.push(&bytes[start..=i]);
            start = i + 1;
        }
    }
    if start < bytes.len() {
        lines.push(&bytes[start..]);
    }
    lines
}

/// Strip a hunk line's trailing line terminator. The bytes are guaranteed
/// UTF-8: they are interned tokens of the `&str` sources handed to
/// [`InternedInput::new`] in [`compute`].
fn strip_newline(content: &[u8]) -> String {
    let text = std::str::from_utf8(content)
        .expect("diff hunk lines are interned from validated UTF-8 input");
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A self-cleaning temp dir, mirroring `git.rs`'s test helper so this file
    /// stays self-contained without a `tempfile` dev-dependency.
    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("rift-diff-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run a git command in `dir`, asserting success — ground truth fixtures,
    /// mirroring `git.rs`.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("run git");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// An initialized repo with one committed file on `main`, so HEAD exists.
    fn init_repo(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write(&tmp.path.join("tracked.txt"), b"one\ntwo\nthree\n");
        git(&tmp.path, &["add", "tracked.txt"]);
        git(&tmp.path, &["commit", "-q", "-m", "init"]);
        tmp
    }

    fn only_hunks(diff: FileDiff) -> Vec<DiffHunk> {
        match diff {
            FileDiff::Hunks(hunks) => hunks,
            other => panic!("expected FileDiff::Hunks, got {other:?}"),
        }
    }

    #[test]
    fn test_modified_file_yields_add_remove_and_context_lines() {
        let repo = init_repo("modified");
        write(&repo.path.join("tracked.txt"), b"one\nTWO\nthree\n");

        let hunks = only_hunks(compute(&repo.path, Path::new("tracked.txt")).expect("compute"));
        assert_eq!(hunks.len(), 1);
        let lines: Vec<(DiffLineKind, &str)> = hunks[0]
            .lines
            .iter()
            .map(|l| (l.kind, l.content.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (DiffLineKind::Context, "one"),
                (DiffLineKind::Remove, "two"),
                (DiffLineKind::Add, "TWO"),
                (DiffLineKind::Context, "three"),
            ]
        );
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[0].new_start, 1);
    }

    #[test]
    fn test_unchanged_file_yields_no_hunks() {
        let repo = init_repo("unchanged");
        let hunks = only_hunks(compute(&repo.path, Path::new("tracked.txt")).expect("compute"));
        assert!(hunks.is_empty(), "identical content must yield no hunks");
    }

    #[test]
    fn test_added_file_diffs_against_empty_head_side() {
        let repo = init_repo("added");
        write(&repo.path.join("new.txt"), b"fresh content\n");

        let hunks = only_hunks(compute(&repo.path, Path::new("new.txt")).expect("compute"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_len, 0, "added file has no old-side lines");
        assert!(hunks[0].lines.iter().all(|l| l.kind == DiffLineKind::Add));
        assert_eq!(hunks[0].lines[0].content, "fresh content");
    }

    #[test]
    fn test_deleted_file_diffs_to_empty_worktree_side() {
        let repo = init_repo("deleted");
        std::fs::remove_file(repo.path.join("tracked.txt")).expect("remove");

        let hunks = only_hunks(compute(&repo.path, Path::new("tracked.txt")).expect("compute"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].new_len, 0, "deleted file has no new-side lines");
        assert!(hunks[0]
            .lines
            .iter()
            .all(|l| l.kind == DiffLineKind::Remove));
    }

    #[test]
    fn test_untracked_file_diffs_against_empty_head_blob() {
        // Never added to git at all — HEAD has no tree entry for it, distinct
        // from a staged-but-uncommitted add.
        let repo = init_repo("untracked");
        write(&repo.path.join("loose.txt"), b"loose\n");

        let hunks = only_hunks(compute(&repo.path, Path::new("loose.txt")).expect("compute"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_len, 0);
    }

    #[test]
    fn test_binary_file_returns_binary_sentinel() {
        let repo = init_repo("binary");
        write(&repo.path.join("blob.bin"), &[0u8, 1, 2, 3, 0, 255]);
        git(&repo.path, &["add", "-f", "blob.bin"]);
        git(&repo.path, &["commit", "-q", "-m", "add binary"]);
        write(&repo.path.join("blob.bin"), &[0u8, 9, 9, 9, 0, 255]);

        let diff = compute(&repo.path, Path::new("blob.bin")).expect("compute");
        assert_eq!(diff, FileDiff::Binary);
    }

    #[test]
    fn test_oversized_file_returns_too_large_sentinel() {
        let repo = init_repo("oversized");
        // Exceed the byte ceiling directly — cheaper than exceeding the
        // changed-line ceiling and exercises the same sentinel path.
        let big = "x".repeat(MAX_SIDE_BYTES + 1);
        write(&repo.path.join("big.txt"), big.as_bytes());
        git(&repo.path, &["add", "big.txt"]);
        git(&repo.path, &["commit", "-q", "-m", "add big"]);
        write(&repo.path.join("big.txt"), format!("{big}y").as_bytes());

        let diff = compute(&repo.path, Path::new("big.txt")).expect("compute");
        assert_eq!(diff, FileDiff::TooLarge);
    }

    #[test]
    fn test_clean_repo_head_lookup_survives_unborn_head() {
        // A freshly `git init`'d repo with no commits: HEAD is unborn, so the
        // path must diff against empty HEAD content rather than error.
        let tmp = TempDir::new("unborn");
        git(&tmp.path, &["init", "-q", "-b", "main"]);
        write(&tmp.path.join("only.txt"), b"content\n");

        let hunks = only_hunks(compute(&tmp.path, Path::new("only.txt")).expect("compute"));
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].old_len, 0);
    }

    /// The blob id of `relative` at HEAD, via `git rev-parse` — ground truth
    /// for the [`compute_rewrite`] tests, which need a real object id to diff
    /// against.
    fn head_blob_id(repo_root: &Path, relative: &str) -> gix::ObjectId {
        let output = Command::new("git")
            .args(["rev-parse", &format!("HEAD:{relative}")])
            .current_dir(repo_root)
            .output()
            .expect("run git rev-parse");
        assert!(
            output.status.success(),
            "rev-parse {relative} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let hex = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        gix::ObjectId::from_hex(hex.as_bytes()).expect("parse blob id")
    }

    #[test]
    fn test_compute_in_repo_reuses_open_repo_matches_compute() {
        let repo = init_repo("in-repo");
        write(&repo.path.join("tracked.txt"), b"one\nTWO\nthree\n");

        let open = gix::open(&repo.path).expect("open repo");
        let hunks = only_hunks(
            compute_in_repo(&open, &repo.path, Path::new("tracked.txt")).expect("compute"),
        );
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines[1].content, "two");
    }

    #[test]
    fn test_compute_rewrite_pure_rename_yields_no_hunks() {
        // A rename with identical content: diffing the new path's worktree
        // content against the rewrite's *source* blob must yield no hunks —
        // diffing against the (nonexistent) HEAD entry at the new path would
        // instead show a full add.
        let repo = init_repo("rewrite-pure");
        let source_id = head_blob_id(&repo.path, "tracked.txt");
        std::fs::rename(repo.path.join("tracked.txt"), repo.path.join("renamed.txt"))
            .expect("rename on disk");

        let open = gix::open(&repo.path).expect("open repo");
        let hunks = only_hunks(
            compute_rewrite(&open, &repo.path, source_id, Path::new("renamed.txt"))
                .expect("compute"),
        );
        assert!(hunks.is_empty(), "a pure rename must yield no hunks");
    }

    #[test]
    fn test_split_lines_matches_git_line_model_including_trailing_newline() {
        // The tokenizer keeps the terminator on each line, emits a final
        // unterminated line for trailing content, and yields nothing for empty
        // input — the exact model `gix`'s hunk headers are numbered against.
        assert_eq!(split_lines(b""), Vec::<&[u8]>::new());
        assert_eq!(split_lines(b"a\nb\n"), vec![&b"a\n"[..], &b"b\n"[..]]);
        assert_eq!(split_lines(b"a\nb"), vec![&b"a\n"[..], &b"b"[..]]);
        assert_eq!(split_lines(b"\n"), vec![&b"\n"[..]]);
        // Concatenating the split is byte-identical to the source (the reapply
        // relies on this to round-trip exactly).
        let src = b"one\r\ntwo\nthree";
        assert_eq!(split_lines(src).concat(), src);
    }

    #[test]
    fn test_compute_rewrite_renamed_and_edited_yields_only_the_content_diff() {
        // A rename plus a content edit: the diff must reflect only the edit,
        // not a full add — proving the source blob (not empty) is the old side.
        let repo = init_repo("rewrite-edited");
        let source_id = head_blob_id(&repo.path, "tracked.txt");
        std::fs::rename(repo.path.join("tracked.txt"), repo.path.join("renamed.txt"))
            .expect("rename on disk");
        write(&repo.path.join("renamed.txt"), b"one\nTWO\nthree\n");

        let open = gix::open(&repo.path).expect("open repo");
        let hunks = only_hunks(
            compute_rewrite(&open, &repo.path, source_id, Path::new("renamed.txt"))
                .expect("compute"),
        );
        assert_eq!(hunks.len(), 1);
        let lines: Vec<(DiffLineKind, &str)> = hunks[0]
            .lines
            .iter()
            .map(|l| (l.kind, l.content.as_str()))
            .collect();
        assert_eq!(
            lines,
            vec![
                (DiffLineKind::Context, "one"),
                (DiffLineKind::Remove, "two"),
                (DiffLineKind::Add, "TWO"),
                (DiffLineKind::Context, "three"),
            ]
        );
    }
}
