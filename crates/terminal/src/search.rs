//! Terminal scrollback search: match state over a raw `alacritty_terminal`
//! grid, driven entirely by `Term`'s own regex search primitives — the `Term`
//! already backs the pane's render, so no new dependency is needed
//! (`docs/spec-v1-hardening.md`). Operates only on grid cells, never on pane
//! content semantics: agent-agnostic by construction. Search only ever
//! targets the live `Term`'s own scrollback (bounded by `Config::scrolling_
//! history`, 10k lines) — the separately captured pre-attach history block
//! `PaneView` renders above it is a distinct, static concern out of scope
//! here.

use std::collections::HashMap;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Direction, Point};
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::Term;

/// One grid line's search-highlight ranges: `(start_col, end_col_inclusive,
/// is_current_match)`.
pub type LineMatches = Vec<(usize, usize, bool)>;

/// Search matches grouped by grid line, for O(1) per-row lookup while
/// painting.
pub type MatchIndex = HashMap<i32, LineMatches>;

/// Escape regex metacharacters so a query matches literally. `RegexSearch`
/// speaks the same metacharacter set as the `regex` crate; scrollback search
/// is a literal text search, not a regex tool, so every special character in
/// the user's query is escaped before compiling.
pub fn escape_literal(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for c in query.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Every match of `regex` in `term`'s live grid (topmost scrollback line
/// through the bottommost live row), left to right, oldest scrollback first.
fn find_all_matches<T>(term: &Term<T>, regex: &mut RegexSearch) -> Vec<Match> {
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    RegexIter::new(start, end, Direction::Right, term, regex).collect()
}

/// Group `matches` by grid line into per-column highlight ranges, marking
/// `current` as the current match. A match spanning a wrapped line (the
/// common case for long, soft-wrapped terminal output) is split across every
/// line it touches.
pub fn index_matches_by_line(
    matches: &[Match],
    current: Option<usize>,
    columns: usize,
) -> MatchIndex {
    let mut index: MatchIndex = HashMap::new();
    let last_col = columns.saturating_sub(1);

    for (i, m) in matches.iter().enumerate() {
        let is_current = current == Some(i);
        let start = *m.start();
        let end = *m.end();

        if start.line == end.line {
            index
                .entry(start.line.0)
                .or_default()
                .push((start.column.0, end.column.0, is_current));
            continue;
        }

        index
            .entry(start.line.0)
            .or_default()
            .push((start.column.0, last_col, is_current));
        for line in (start.line.0 + 1)..end.line.0 {
            index
                .entry(line)
                .or_default()
                .push((0, last_col, is_current));
        }
        index
            .entry(end.line.0)
            .or_default()
            .push((0, end.column.0, is_current));
    }

    index
}

/// Whether grid cell `(line, column)` is part of a match, and whether that
/// match is the current one. `(is_match, is_current)`.
pub fn cell_search_flags(index: &MatchIndex, line: i32, column: usize) -> (bool, bool) {
    let Some(ranges) = index.get(&line) else {
        return (false, false);
    };
    ranges.iter().fold(
        (false, false),
        |(matched, current), &(start, end, is_current)| {
            if column >= start && column <= end {
                (true, current || is_current)
            } else {
                (matched, current)
            }
        },
    )
}

/// Scrollback search state for one terminal pane: a compiled query and its
/// matches against the live `Term`. Knows nothing about GPUI or input focus —
/// `PaneView` drives it from the query text and the live `Term` on every
/// query change and navigation step.
#[derive(Default)]
pub struct SearchState {
    regex: Option<RegexSearch>,
    matches: Vec<Match>,
    current: Option<usize>,
}

impl SearchState {
    pub fn current(&self) -> Option<usize> {
        self.current
    }

    pub fn count(&self) -> usize {
        self.matches.len()
    }

    pub fn current_match(&self) -> Option<&Match> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// Recompile `query` (escaped to a literal match) and re-run it against
    /// `term`'s live grid, replacing the match list. An empty or
    /// uncompilable query clears the search. The first match (if any)
    /// becomes current.
    pub fn set_query<T>(&mut self, query: &str, term: &Term<T>) {
        if query.is_empty() {
            self.regex = None;
            self.matches.clear();
            self.current = None;
            return;
        }

        let Ok(mut regex) = RegexSearch::new(&escape_literal(query)) else {
            self.regex = None;
            self.matches.clear();
            self.current = None;
            return;
        };

        self.matches = find_all_matches(term, &mut regex);
        self.current = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
        self.regex = Some(regex);
    }

    /// Advance to the next match, wrapping past the last back to the first.
    pub fn select_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
    }

    /// Step back to the previous match, wrapping past the first to the last.
    pub fn select_prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
    }

    /// Build the per-line highlight index for the current match list, for
    /// [`super::pane_view`]'s cell painting.
    pub fn index_by_line(&self, columns: usize) -> MatchIndex {
        index_matches_by_line(&self.matches, self.current, columns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::Line;
    use alacritty_terminal::term::test::mock_term;

    #[test]
    fn test_escape_literal_regex_metachars_escaped() {
        assert_eq!(escape_literal("a.b*c"), r"a\.b\*c");
        assert_eq!(escape_literal("plain"), "plain");
    }

    #[test]
    fn test_find_all_matches_repeated_query_returns_every_occurrence() {
        let term = mock_term("foo bar foo baz foo");
        let mut regex = RegexSearch::new("foo").unwrap();
        assert_eq!(find_all_matches(&term, &mut regex).len(), 3);
    }

    #[test]
    fn test_find_all_matches_no_occurrence_returns_empty() {
        let term = mock_term("foo bar");
        let mut regex = RegexSearch::new("nope").unwrap();
        assert!(find_all_matches(&term, &mut regex).is_empty());
    }

    #[test]
    fn test_find_all_matches_hard_broken_lines_finds_matches_on_each_line() {
        let term = mock_term("foo\r\nbar\r\nfoo");
        let mut regex = RegexSearch::new("foo").unwrap();
        let matches = find_all_matches(&term, &mut regex);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start().line, Line(0));
        assert_eq!(matches[1].start().line, Line(2));
    }

    #[test]
    fn test_index_matches_by_line_single_line_match_marks_correct_columns() {
        let matches = vec![Point::new(Line(0), Column(1))..=Point::new(Line(0), Column(3))];
        let index = index_matches_by_line(&matches, Some(0), 10);
        assert_eq!(index[&0], vec![(1, 3, true)]);
    }

    #[test]
    fn test_index_matches_by_line_marks_current_flag_only_for_current_index() {
        let matches = vec![
            Point::new(Line(0), Column(0))..=Point::new(Line(0), Column(0)),
            Point::new(Line(1), Column(0))..=Point::new(Line(1), Column(0)),
        ];
        let index = index_matches_by_line(&matches, Some(1), 10);
        assert_eq!(index[&0], vec![(0, 0, false)]);
        assert_eq!(index[&1], vec![(0, 0, true)]);
    }

    #[test]
    fn test_index_matches_by_line_span_across_lines_marks_full_middle_rows() {
        let matches = vec![Point::new(Line(0), Column(3))..=Point::new(Line(2), Column(1))];
        let index = index_matches_by_line(&matches, Some(0), 5);
        assert_eq!(index[&0], vec![(3, 4, true)]);
        assert_eq!(index[&1], vec![(0, 4, true)]);
        assert_eq!(index[&2], vec![(0, 1, true)]);
    }

    #[test]
    fn test_cell_search_flags_outside_any_match_returns_unset() {
        let matches = vec![Point::new(Line(0), Column(2))..=Point::new(Line(0), Column(4))];
        let index = index_matches_by_line(&matches, Some(0), 10);
        assert_eq!(cell_search_flags(&index, 0, 1), (false, false));
        assert_eq!(cell_search_flags(&index, 0, 3), (true, true));
        assert_eq!(cell_search_flags(&index, 5, 3), (false, false));
    }

    #[test]
    fn test_search_state_set_query_selects_first_match() {
        let term = mock_term("foo bar foo");
        let mut state = SearchState::default();
        state.set_query("foo", &term);
        assert_eq!(state.count(), 2);
        assert_eq!(state.current(), Some(0));
    }

    #[test]
    fn test_search_state_set_query_empty_clears_matches() {
        let term = mock_term("foo bar foo");
        let mut state = SearchState::default();
        state.set_query("foo", &term);
        state.set_query("", &term);
        assert_eq!(state.count(), 0);
        assert_eq!(state.current(), None);
    }

    #[test]
    fn test_search_state_set_query_regex_metacharacter_matches_literally() {
        let term = mock_term("a.c abc");
        let mut state = SearchState::default();
        state.set_query("a.c", &term);
        // A literal "a.c" only matches the literal dot, not "abc".
        assert_eq!(state.count(), 1);
    }

    #[test]
    fn test_search_state_select_next_wraps_to_first_after_last() {
        let term = mock_term("foo foo foo");
        let mut state = SearchState::default();
        state.set_query("foo", &term);
        assert_eq!(state.count(), 3);
        state.select_next();
        state.select_next();
        assert_eq!(state.current(), Some(2));
        state.select_next();
        assert_eq!(state.current(), Some(0));
    }

    #[test]
    fn test_search_state_select_prev_wraps_to_last_before_first() {
        let term = mock_term("foo foo foo");
        let mut state = SearchState::default();
        state.set_query("foo", &term);
        state.select_prev();
        assert_eq!(state.current(), Some(2));
    }

    #[test]
    fn test_search_state_select_next_on_no_matches_stays_none() {
        let term = mock_term("foo bar");
        let mut state = SearchState::default();
        state.set_query("nope", &term);
        state.select_next();
        assert_eq!(state.current(), None);
    }
}
