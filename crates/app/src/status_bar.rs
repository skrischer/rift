//! Status bar: a thin, read-only strip along the bottom of the window — a
//! `flex_col` sibling of the `DockArea`, not a dock `Panel`
//! (`docs/spec-status-bar.md`). This step renders the left group: the current
//! git branch plus its ahead/behind counts against the upstream, sourced from
//! `WorktreeModel::branch()` / `ahead_behind()` (`RepoState` folds). The
//! diagnostic-counts right group is a follow-on issue.

use gpui::{div, px, App, IntoElement, ParentElement as _, Styled as _};
use gpui_component::ActiveTheme as _;
use rift_protocol::AheadBehind;

/// Fixed height of the status bar strip, in pixels — a thin single row, never
/// competing with the dock area for vertical space.
const HEIGHT: f32 = 24.0;

/// Label shown when there is no branch to report: HEAD is detached, or the
/// worktree is not a git repo. The client cannot tell the two apart — both
/// collapse to a `None` `RepoState.branch` (`crates/protocol/src/lib.rs`) — so
/// one muted indicator covers both, per the spec's degrade-cleanly outcome.
const NO_BRANCH_LABEL: &str = "detached HEAD";

/// Format the branch + ahead/behind label. The ahead/behind counts are
/// appended only when there is something to show: a `None` `ahead_behind` (no
/// upstream) or an up-to-date `0`/`0` both omit them, mirroring git's own
/// porcelain output (`git status` drops the bracket when there is nothing to
/// report).
fn segment_text(branch: Option<&str>, ahead_behind: Option<AheadBehind>) -> String {
    let mut text = branch.unwrap_or(NO_BRANCH_LABEL).to_owned();
    if let Some(AheadBehind { ahead, behind }) = ahead_behind {
        if ahead > 0 || behind > 0 {
            text.push_str(&format!(" \u{2191}{ahead} \u{2193}{behind}"));
        }
    }
    text
}

/// Render the status bar strip's left group: branch + ahead/behind. A missing
/// branch (detached HEAD / no repo) renders muted, never a crash.
pub fn render(
    branch: Option<&str>,
    ahead_behind: Option<AheadBehind>,
    cx: &App,
) -> impl IntoElement {
    let text_color = if branch.is_some() {
        cx.theme().foreground
    } else {
        cx.theme().muted_foreground
    };

    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .w_full()
        .h(px(HEIGHT))
        .px(px(8.0))
        .border_t_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .text_xs()
        .text_color(text_color)
        .child(segment_text(branch, ahead_behind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_segment_text_shows_branch_name_when_present_with_no_upstream() {
        assert_eq!(segment_text(Some("main"), None), "main");
    }

    #[test]
    fn test_segment_text_shows_muted_indicator_when_detached_or_no_repo() {
        assert_eq!(segment_text(None, None), "detached HEAD");
    }

    #[test]
    fn test_segment_text_appends_ahead_behind_counts() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 2,
                    behind: 1
                })
            ),
            "main \u{2191}2 \u{2193}1"
        );
    }

    #[test]
    fn test_segment_text_omits_ahead_behind_when_up_to_date() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 0,
                    behind: 0
                })
            ),
            "main"
        );
    }

    #[test]
    fn test_segment_text_includes_both_counts_when_only_one_side_is_nonzero() {
        assert_eq!(
            segment_text(
                Some("main"),
                Some(AheadBehind {
                    ahead: 3,
                    behind: 0
                })
            ),
            "main \u{2191}3 \u{2193}0"
        );
    }

    #[test]
    fn test_segment_text_detached_with_ahead_behind_still_appends_counts() {
        // Defensive: the daemon never pairs a `None` branch with `Some`
        // ahead/behind in practice, but the formatter must not special-case
        // that combination away — it just composes the two independently.
        assert_eq!(
            segment_text(
                None,
                Some(AheadBehind {
                    ahead: 1,
                    behind: 0
                })
            ),
            "detached HEAD \u{2191}1 \u{2193}0"
        );
    }
}
