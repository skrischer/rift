//! tmux control-mode argument quoting (`docs/spec-session-management.md`).
//!
//! Rename/kill commands assembled client-side (issues #684/#685) embed an
//! untrusted session name into a raw tmux command line sent over the control
//! stream (`ClientMessage::TmuxCommand`). tmux parses that line with its own
//! lexer, so a name containing whitespace or lexer metacharacters (`"`, `'`,
//! `;`, a leading `-`, `$`, `#`) could otherwise break the argument
//! boundary or inject a second command. This module holds the quoting
//! helper in its own file (rather than `session_view.rs`, which #683 edits
//! in parallel) to keep the two changes conflict-free.

/// Wrap `value` as a single safe tmux command-line argument.
///
/// tmux's lexer treats a single-quoted span as fully literal — no variable
/// expansion, no metacharacter handling — so wrapping in `'...'` and
/// escaping any embedded single quote as `'\''` (close the quote, an
/// escaped literal quote, reopen the quote) is sufficient to make `value`
/// parse as exactly one argument, regardless of its content: whitespace,
/// `"`, `;`, a leading `-`, `$`, `#`, or unicode all stay inert inside the
/// quotes. Callers pass the result after `--` (end-of-options marker) so a
/// value that still starts with `-` after quoting is never mistaken for a
/// flag.
pub fn quote_tmux_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_tmux_arg_plain_wraps_in_single_quotes() {
        assert_eq!(quote_tmux_arg("build"), "'build'");
    }

    #[test]
    fn test_quote_tmux_arg_with_space_stays_single_argument() {
        assert_eq!(quote_tmux_arg("my session"), "'my session'");
    }

    #[test]
    fn test_quote_tmux_arg_with_double_quote_is_inert() {
        assert_eq!(quote_tmux_arg("a\"b"), "'a\"b'");
    }

    #[test]
    fn test_quote_tmux_arg_with_single_quote_is_escaped() {
        assert_eq!(quote_tmux_arg("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_quote_tmux_arg_with_semicolon_cannot_inject_a_second_command() {
        // Unquoted, `;` is tmux's command separator; quoted, it is a
        // literal character inside the single argument.
        assert_eq!(quote_tmux_arg("evil; kill-server"), "'evil; kill-server'");
    }

    #[test]
    fn test_quote_tmux_arg_with_leading_dash_is_not_a_flag() {
        assert_eq!(quote_tmux_arg("-t"), "'-t'");
    }

    #[test]
    fn test_quote_tmux_arg_with_dollar_sign_is_not_expanded() {
        assert_eq!(quote_tmux_arg("$HOME"), "'$HOME'");
    }

    #[test]
    fn test_quote_tmux_arg_with_hash_is_not_a_comment() {
        assert_eq!(quote_tmux_arg("#name"), "'#name'");
    }

    #[test]
    fn test_quote_tmux_arg_with_unicode_round_trips() {
        assert_eq!(quote_tmux_arg("séance-会話"), "'séance-会話'");
    }

    #[test]
    fn test_quote_tmux_arg_empty_string_produces_empty_argument() {
        assert_eq!(quote_tmux_arg(""), "''");
    }
}
