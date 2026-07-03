//! tmux status-line option discovery.
//!
//! The pure data layer of the Phase 8 mirroring spec
//! (`docs/spec-tmux-statusline-mirroring.md`): parses the `show-options -A`
//! reply into the discovered `status-*` option set, and detects dispatched
//! commands that could change it (the refresh-trigger contract). The actual
//! `status-left`/`status-right` *content* never flows through this module —
//! it comes from the daemon's separate server-side expansion fetch
//! (`display-message -p '#{T:status-left}'` / `'#{T:status-right}'`), so the
//! raw (unexpanded) option text is never even carried here, let alone
//! interpolated into a command line. Nothing here touches the command seam or
//! pane content.

use crate::keytable::lex_token;

/// The discovered `status-*` option set, session-resolved via `show-options
/// -A` — the flag that resolves options inherited from the global scope
/// (e.g. a `.tmux.conf` `set -g status-style ...`), unlike a plain
/// `show-options` which lists only session-level overrides.
///
/// `status-left`/`status-right` are deliberately not fields here: their
/// content comes from the server-side-expanded fetch, never from this raw,
/// unexpanded option text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusOptions {
    /// Raw `status` value: `"off"`, `"on"`, or `"2"`..`"5"` (multi-row).
    pub status: String,
    pub status_style: String,
    pub status_left_style: String,
    pub status_right_style: String,
    pub status_left_length: u32,
    pub status_right_length: u32,
    pub status_interval: u64,
}

impl Default for StatusOptions {
    /// tmux's own compiled-in defaults, used as the base so a partial
    /// `show-options -A` response still yields a usable set (same convention
    /// as [`crate::keytable::PrefixOptions`]).
    fn default() -> Self {
        Self {
            status: "on".to_owned(),
            status_style: "default".to_owned(),
            status_left_style: "default".to_owned(),
            status_right_style: "default".to_owned(),
            status_left_length: 10,
            status_right_length: 40,
            status_interval: 15,
        }
    }
}

/// Parse a `show-options -A` reply into [`StatusOptions`], overriding the
/// tmux defaults for any of the mirrored fields that appear. Each line is
/// `name value` (a trailing `*` on `name` marks a value inherited from a
/// higher scope, per `-A`, and is stripped before matching); unrelated
/// options — including `status-left`/`status-right` themselves, and indexed
/// options like `status-format[0]` — and unparseable values are ignored.
pub fn parse_status_options(output: &str) -> StatusOptions {
    let mut options = StatusOptions::default();
    for line in output.lines() {
        let Some((raw_name, after_name)) = lex_token(line, 0) else {
            continue;
        };
        let name = raw_name.strip_suffix('*').unwrap_or(&raw_name);
        let value = lex_token(line, after_name).map(|(value, _)| value);
        match name {
            "status" => {
                if let Some(value) = value {
                    options.status = value;
                }
            }
            "status-style" => {
                if let Some(value) = value {
                    options.status_style = value;
                }
            }
            "status-left-style" => {
                if let Some(value) = value {
                    options.status_left_style = value;
                }
            }
            "status-right-style" => {
                if let Some(value) = value {
                    options.status_right_style = value;
                }
            }
            "status-left-length" => {
                if let Some(parsed) = value.and_then(|v| v.parse().ok()) {
                    options.status_left_length = parsed;
                }
            }
            "status-right-length" => {
                if let Some(parsed) = value.and_then(|v| v.parse().ok()) {
                    options.status_right_length = parsed;
                }
            }
            "status-interval" => {
                if let Some(parsed) = value.and_then(|v| v.parse().ok()) {
                    options.status_interval = parsed;
                }
            }
            _ => {}
        }
    }
    options
}

/// Whether a dispatched command could change a mirrored `status-*` option —
/// the change-trigger half of the refresh contract (`status-interval`'s own
/// timer and attach/reconnect are the other two,
/// `docs/spec-tmux-statusline-mirroring.md`). Only `set-option`/`set`
/// targeting one of the mirrored option names triggers a refresh; matching is
/// a plain token scan (not tmux-quote-aware, matching
/// [`crate::keytable::mutates_bindings`]): a false positive costs one
/// harmless extra refresh, a false negative would miss a real status change.
pub fn mutates_status_options(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    match tokens.next().unwrap_or("") {
        "set-option" | "set" => tokens.any(|token| {
            matches!(
                token,
                "status"
                    | "status-left"
                    | "status-right"
                    | "status-style"
                    | "status-left-style"
                    | "status-right-style"
                    | "status-left-length"
                    | "status-right-length"
                    | "status-interval"
            )
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `tmux 3.4` `show-options -A` output (session-resolved: entries
    // inherited from the global scope carry a trailing `*`), covering the
    // mirrored fields plus noise this parser must ignore: indexed
    // `status-format[N]`, unrelated options, and the raw (never-consumed)
    // `status-left`/`status-right` text.
    const SHOW_OPTIONS_A_FIXTURE: &str = r##"
prefix* C-a
prefix2* None
status* on
status-bg* default
status-fg* default
status-format[0]* "#[align=left]#{T;=/10:status-left}"
status-format[1]* "#[align=centre]#{P:...}"
status-interval* 15
status-justify* left
status-keys* emacs
status-left "#{?#{==:#{host_short},x},yes,no}-#H"
status-left-length* 10
status-left-style* default
status-position* bottom
status-right "#[fg=green]%H:%M#[default]"
status-right-length* 40
status-right-style* default
status-style bg=green,fg=black
"##;

    #[test]
    fn test_parse_status_options_defaults_on_empty_input() {
        let options = parse_status_options("");
        assert_eq!(options, StatusOptions::default());
    }

    #[test]
    fn test_parse_status_options_resolves_inherited_and_local_values() {
        // `status-style` was set at session level (no `*`); `status-interval`
        // is inherited from global (`*`) — both must resolve identically.
        let options = parse_status_options(SHOW_OPTIONS_A_FIXTURE);
        assert_eq!(options.status, "on");
        assert_eq!(options.status_interval, 15);
        assert_eq!(options.status_style, "bg=green,fg=black");
        assert_eq!(options.status_left_style, "default");
        assert_eq!(options.status_right_style, "default");
        assert_eq!(options.status_left_length, 10);
        assert_eq!(options.status_right_length, 40);
    }

    #[test]
    fn test_parse_status_options_ignores_indexed_and_unrelated_options() {
        let input = "\
status-format[0] \"whatever\"
prefix C-b
window-status-style default
status-interval 30
";
        let options = parse_status_options(input);
        assert_eq!(options.status_interval, 30);
        // Untouched fields keep their defaults.
        assert_eq!(options.status, "on");
    }

    #[test]
    fn test_parse_status_options_malformed_length_and_interval_keep_defaults() {
        let input = "\
status-left-length not-a-number
status-interval also-not-a-number
";
        let options = parse_status_options(input);
        assert_eq!(options.status_left_length, 10);
        assert_eq!(options.status_interval, 15);
    }

    #[test]
    fn test_parse_status_options_quoted_style_value_with_spaces() {
        let input = r#"status-style "bg=colour234,fg=colour253""#;
        let options = parse_status_options(input);
        assert_eq!(options.status_style, "bg=colour234,fg=colour253");
    }

    #[test]
    fn test_parse_status_options_blank_and_short_lines_skip_without_failing() {
        let input = "\n\nstatus\nstatus-interval 5\n";
        let options = parse_status_options(input);
        // A `status` line with no value keeps the default rather than panicking.
        assert_eq!(options.status, "on");
        assert_eq!(options.status_interval, 5);
    }

    // --- refresh-trigger detection ---

    #[test]
    fn test_mutates_status_options_true_for_mirrored_option_names() {
        for cmd in [
            "set-option -g status-style bg=red",
            "set -g status-left-length 20",
            "set-option status-interval 30",
            "set-option -g status off",
            "set-option -g status-left '#H'",
            "set-option -g status-right '#H'",
            "set -g status-right-style default",
        ] {
            assert!(mutates_status_options(cmd), "{cmd}");
        }
    }

    #[test]
    fn test_mutates_status_options_false_for_unrelated_option_and_other_commands() {
        for cmd in [
            "set-option -g mouse on",
            "set -g prefix C-a",
            "new-window",
            "select-pane -L",
            "",
        ] {
            assert!(!mutates_status_options(cmd), "{cmd}");
        }
    }
}
