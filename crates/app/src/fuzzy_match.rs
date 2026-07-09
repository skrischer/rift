//! Fuzzy match substrate for the explorer filter bar and quick-open
//! (`docs/spec-explorer-search.md`, issue #678): a pure, headless function
//! that scores a query against a single candidate path and reports the
//! matched character positions for emphasis rendering. Both the in-panel
//! filter bar (`file_tree.rs`) and the jump-to-file quick-open consume this
//! module; neither talks to `nucleo-matcher` directly.
//!
//! Wraps `nucleo-matcher`'s **low-level synchronous** `Matcher` + `Pattern`
//! API (`docs/spec-explorer-search.md` constraints), never the `nucleo` crate's
//! threaded driver: matching runs over the already-in-memory streamed
//! `WorktreeModel::entries()`, so no background worker, injector, or extra
//! thread is warranted.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// The outcome of fuzzy-matching a query against a single candidate path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// `nucleo-matcher`'s rank score. Higher is a better match; the scale has
    /// no fixed meaning outside comparing scores from the same matcher
    /// configuration, so it is only ever used to sort candidates against one
    /// another.
    pub score: u32,
    /// 0-based **character** positions into `candidate.chars()` that
    /// `nucleo-matcher` reports as matched, ascending and deduplicated.
    ///
    /// These are never UTF-8 byte offsets: `nucleo-matcher` matches over an
    /// internal codepoint array and `Pattern::indices` reports positions into
    /// that array, not byte offsets into the original `&str` (a `char` can be
    /// 1-4 bytes). A caller that wants to emphasize the matched characters
    /// must walk `candidate.chars().enumerate()` and test membership by
    /// position — indexing `candidate` by byte with one of these numbers
    /// directly would panic (or silently land mid-character) on a non-ASCII
    /// name.
    pub matched_indices: Vec<usize>,
}

/// Fuzzy-match `query` against `candidate` (a root-relative path), returning
/// `None` when `query`'s characters do not all appear in `candidate`, in
/// order (a plain, case-insensitive subsequence with `nucleo`'s path-aware
/// scoring bonuses — see [`Matcher`] / `Config::match_paths`).
///
/// An empty (or whitespace-only) query matches every candidate with score `0`
/// and no matched indices — `nucleo_matcher::pattern::Pattern` already treats
/// an empty pattern as having no atoms and short-circuits to a trivial match
/// before touching `candidate` at all, so this falls out of the library's own
/// behavior rather than a special case here. This is the substrate half of
/// "an empty query is identical to no filter" (`docs/spec-explorer-search.md`);
/// the filter bar and quick-open decide what an unfiltered/full listing looks
/// like.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut haystack_buf = Vec::new();
    let haystack = Utf32Str::new(candidate, &mut haystack_buf);

    let mut raw_indices = Vec::new();
    let score = pattern.indices(haystack, &mut matcher, &mut raw_indices)?;

    // `Pattern::indices` appends each atom's indices without sorting or
    // deduplicating across atoms (its own doc comment recommends exactly this
    // follow-up pass).
    raw_indices.sort_unstable();
    raw_indices.dedup();

    Some(FuzzyMatch {
        score,
        matched_indices: raw_indices
            .into_iter()
            .map(|index| index as usize)
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzy_match_query_matching_prefix_returns_score_and_indices() {
        let result = fuzzy_match("main", "main.rs").expect("prefix subsequence matches");

        assert_eq!(result.matched_indices, vec![0, 1, 2, 3]);
        assert!(result.score > 0);
    }

    #[test]
    fn test_fuzzy_match_empty_query_matches_every_candidate_with_no_indices() {
        let result = fuzzy_match("", "main.rs").expect("empty query matches everything");

        assert_eq!(result.score, 0);
        assert!(result.matched_indices.is_empty());

        // Even a candidate that a non-empty query would never match still
        // matches an empty one: "empty query lists all"
        // (`docs/spec-explorer-search.md`).
        let result = fuzzy_match("   ", "").expect("whitespace-only query still matches");
        assert!(result.matched_indices.is_empty());
    }

    /// The matched character positions must index correctly into a candidate
    /// containing multi-byte UTF-8 characters — never a raw byte offset,
    /// which would panic or mis-split mid-character.
    #[test]
    fn test_fuzzy_match_non_ascii_candidate_reports_correct_character_positions() {
        let candidate = "café/main.rs";
        let result = fuzzy_match("café", candidate).expect("exact-order subsequence matches");

        let matched: String = result
            .matched_indices
            .iter()
            .map(|&index| candidate.chars().nth(index).expect("index is in range"))
            .collect();
        assert_eq!(matched, "café");
    }

    #[test]
    fn test_fuzzy_match_out_of_order_characters_does_not_match() {
        // In "main.rs" the characters appear m, a, i, n, ., r, s — a query
        // asking for 'r' before 'm' is not a subsequence.
        assert!(fuzzy_match("rma", "main.rs").is_none());
    }

    #[test]
    fn test_fuzzy_match_unrelated_query_does_not_match() {
        assert!(fuzzy_match("xyz", "main.rs").is_none());
    }

    /// The rank score is used to order candidates, not just to signal a
    /// match: a tight, contiguous match should outrank a scattered one for
    /// the same query.
    #[test]
    fn test_fuzzy_match_ranks_a_contiguous_match_higher_than_a_scattered_one() {
        let tight = fuzzy_match("main", "main.rs").expect("contiguous match");
        let scattered = fuzzy_match("main", "m_a_i_n.rs").expect("scattered match");

        assert!(tight.score > scattered.score);
    }
}
