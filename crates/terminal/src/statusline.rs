//! tmux status-line option discovery and style-run parsing.
//!
//! The pure data layer of the Phase 8 mirroring spec
//! (`docs/spec-tmux-statusline-mirroring.md`): parses the `show-options -A`
//! reply into the discovered `status-*` option set, detects dispatched
//! commands that could change it (the refresh-trigger contract), and turns
//! an already-expanded `status-left`/`status-right` string (the daemon's
//! separate server-side expansion fetch — `display-message -p
//! '#{T:status-left}'` / `'#{T:status-right}'`) into styled runs ready for
//! GPUI rendering (#221). The raw (unexpanded) option text is never carried
//! here, let alone interpolated into a command line; the expanded string is
//! parsed only for its `#[...]` style directives, never its content. Nothing
//! here touches the command seam or pane content.

use alacritty_terminal::vte::ansi::{Color, NamedColor};
use gpui::{Hsla, Rgba};

use crate::colors::to_gpui_color;
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

/// A color resolved from a tmux color token (`fg=`/`bg=`/`us=` value).
///
/// `Theme` covers both tmux's `default` (the option's own default) and
/// `terminal` (the terminal's default) — both defer to a surrounding base
/// rather than naming a concrete color. Kept as a distinct variant, not
/// resolved eagerly to a color, so the render layer (#221) can re-resolve it
/// against `cx.theme()` on a theme switch without re-parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedColor {
    Theme,
    Color(Hsla),
}

/// tmux style attributes (`STYLES` section), collapsed to on/off flags. The
/// four underline variants (`double-underscore`, `curly-underscore`,
/// `dotted-underscore`, `dashed-underscore`) and plain `underscore` all map to
/// `underline` — rift's status bar renders one underline style. `acs`
/// (terminal alternate character set) has no GUI equivalent and is accepted
/// as a no-op rather than tracked here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleAttrs {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
    pub overline: bool,
}

/// A resolved tmux style: the fg/bg colors and attributes in effect at a
/// point in the expanded status-line text. Used both as the base a style
/// option (`status-style`, `status-left-style`, `status-right-style`)
/// resolves to, and as each [`StyleRun`]'s own style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusStyle {
    pub fg: ResolvedColor,
    pub bg: ResolvedColor,
    pub attrs: StyleAttrs,
}

impl Default for StatusStyle {
    fn default() -> Self {
        Self {
            fg: ResolvedColor::Theme,
            bg: ResolvedColor::Theme,
            attrs: StyleAttrs::default(),
        }
    }
}

/// One contiguous run of status-line text sharing a single [`StatusStyle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyleRun {
    pub text: String,
    pub style: StatusStyle,
}

/// Parse a raw tmux style option value (`status-style`, `status-left-style`,
/// `status-right-style`, or a `#[...]` tag's content) into a [`StatusStyle`].
/// Per tmux's `STYLES` grammar, the value is either the single term `default`
/// (theme default, no attributes) or a space/comma-separated list of
/// `fg=`/`bg=`/attribute tokens. Unrecognized tokens are skipped and logged
/// individually — a partial style beats a blanked bar.
pub fn parse_style(raw: &str) -> StatusStyle {
    let raw = raw.trim();
    if raw == "default" {
        return StatusStyle::default();
    }
    let mut style = StatusStyle::default();
    for token in style_tokens(raw) {
        if !apply_style_token(token, &mut style) {
            tracing::warn!(token = %token, "skipping unknown tmux style token");
        }
    }
    style
}

/// Parse an already-expanded status-line segment (the output of `#219`'s
/// `display-message -p '#{T:status-left}'`/`status-right` fetch) into styled
/// runs, split at `#[...]` boundaries. `base` is the resolved base style
/// (typically `status-style`, per the spec's "status-style paints the bar's
/// base" decision) that the first run starts from and that a bare
/// `#[default]` tag resets to.
///
/// Mirrors tmux's own draw-time parser (`format-draw.c`), which normally
/// consumes this string but never runs under `-CC`: `#[...]` tags are style
/// directives and never appear in the rendered text; `##` is an escaped
/// literal `#` (data that legitimately contains `#[` — e.g. `window_flags` —
/// arrives double-hashed precisely so it survives this step as literal text
/// instead of being misread as a tag start); `range=`/`list=`/`align=`/
/// `fill=`/`us=`/`push-default`/`pop-default` are recognized directives with
/// no effect on a single mirrored run. An unterminated tag (no closing `]`)
/// degrades to literal text for its remainder, logged once — never a panic,
/// never dropped silently.
pub fn parse_style_runs(expanded: &str, base: StatusStyle) -> Vec<StyleRun> {
    let mut runs = Vec::new();
    let mut current = base;
    let mut text = String::new();
    let bytes = expanded.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'#') {
            text.push('#');
            i += 2;
            continue;
        }
        if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'[') {
            if let Some(rel_end) = expanded[i + 2..].find(']') {
                if !text.is_empty() {
                    runs.push(StyleRun {
                        text: std::mem::take(&mut text),
                        style: current,
                    });
                }
                apply_tag(&expanded[i + 2..i + 2 + rel_end], base, &mut current);
                i += 2 + rel_end + 1;
                continue;
            }
            tracing::warn!(
                remainder = %&expanded[i..],
                "unterminated tmux style tag, rendering remainder as literal text"
            );
            text.push_str(&expanded[i..]);
            break;
        }
        let len = expanded[i..]
            .chars()
            .next()
            .map(char::len_utf8)
            .unwrap_or(1);
        text.push_str(&expanded[i..i + len]);
        i += len;
    }
    if !text.is_empty() {
        runs.push(StyleRun {
            text,
            style: current,
        });
    }
    runs
}

/// Truncate parsed runs to `max_width` display cells — `status-left-length`/
/// `status-right-length`, honored client-side since tmux only truncates its
/// status line at draw time, which never happens under `-CC`. Width is
/// counted per Unicode scalar value, not bytes, so multi-byte UTF-8 is never
/// split mid-character; East-Asian wide-character doubling is a known,
/// documented gap (no cell-width table is vendored yet). A run that straddles
/// the boundary is cut, not dropped, keeping its style.
pub fn truncate_runs(runs: Vec<StyleRun>, max_width: u32) -> Vec<StyleRun> {
    let max_width = max_width as usize;
    let mut out = Vec::with_capacity(runs.len());
    let mut used = 0usize;
    for mut run in runs {
        if used >= max_width {
            break;
        }
        let remaining = max_width - used;
        let width = run.text.chars().count();
        if width <= remaining {
            used += width;
            out.push(run);
            continue;
        }
        run.text = run.text.chars().take(remaining).collect();
        out.push(run);
        break;
    }
    out
}

/// Apply one `#[...]` tag's content to `current`, honoring the single-term
/// `#[default]` reset to `base` (per tmux's `STYLES` grammar, same as
/// [`parse_style`]'s top-level `default`).
fn apply_tag(content: &str, base: StatusStyle, current: &mut StatusStyle) {
    let trimmed = content.trim();
    if trimmed == "default" {
        *current = base;
        return;
    }
    for token in style_tokens(trimmed) {
        if !apply_style_token(token, current) {
            tracing::warn!(token = %token, "skipping unknown tmux style token");
        }
    }
}

/// Split a tmux style value into tokens on its documented delimiters — a
/// space or comma separated list (`STYLES` section) — dropping empty tokens
/// from repeated delimiters.
fn style_tokens(content: &str) -> impl Iterator<Item = &str> {
    content
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
}

/// Apply one style-list token to `style`. Returns whether it was recognized —
/// `false` means "skip and log", not "error"; a handful of tokens
/// (`range=`/`list=`/`align=`/`fill=`/`us=`/push-pop-default) are recognized
/// but have no effect on a single mirrored run — they mark mouse ranges and
/// window-list layout in real tmux, none of which apply here.
fn apply_style_token(token: &str, style: &mut StatusStyle) -> bool {
    if let Some(color) = token.strip_prefix("fg=") {
        return match parse_color(color) {
            Some(resolved) => {
                style.fg = resolved;
                true
            }
            None => false,
        };
    }
    if let Some(color) = token.strip_prefix("bg=") {
        return match parse_color(color) {
            Some(resolved) => {
                style.bg = resolved;
                true
            }
            None => false,
        };
    }
    if token == "none" {
        style.attrs = StyleAttrs::default();
        return true;
    }
    if token.starts_with("us=")
        || token.starts_with("align=")
        || token.starts_with("fill=")
        || token.starts_with("list=")
        || token.starts_with("range=")
        || matches!(
            token,
            "noalign" | "nolist" | "norange" | "push-default" | "pop-default"
        )
    {
        return true;
    }
    let (set, name) = match token.strip_prefix("no") {
        Some(rest) if attr_field(rest).is_some() => (false, rest),
        _ => (true, token),
    };
    match attr_field(name) {
        Some(field) => {
            field(&mut style.attrs, set);
            true
        }
        None => false,
    }
}

/// Map an attribute's base name (tmux's `STYLES` list, `no`-prefix already
/// stripped) to the setter for the [`StyleAttrs`] field it controls. `acs`
/// matches but sets nothing (see [`StyleAttrs`]).
fn attr_field(name: &str) -> Option<fn(&mut StyleAttrs, bool)> {
    Some(match name {
        "acs" => |_: &mut StyleAttrs, _: bool| {},
        "bright" | "bold" => |a: &mut StyleAttrs, v: bool| a.bold = v,
        "dim" => |a: &mut StyleAttrs, v: bool| a.dim = v,
        "underscore" | "double-underscore" | "curly-underscore" | "dotted-underscore"
        | "dashed-underscore" => |a: &mut StyleAttrs, v: bool| a.underline = v,
        "blink" => |a: &mut StyleAttrs, v: bool| a.blink = v,
        "reverse" => |a: &mut StyleAttrs, v: bool| a.reverse = v,
        "hidden" => |a: &mut StyleAttrs, v: bool| a.hidden = v,
        "italics" => |a: &mut StyleAttrs, v: bool| a.italic = v,
        "overline" => |a: &mut StyleAttrs, v: bool| a.overline = v,
        "strikethrough" => |a: &mut StyleAttrs, v: bool| a.strikethrough = v,
        _ => return None,
    })
}

/// Parse one tmux color token (a `fg=`/`bg=`/`us=` value) into a
/// [`ResolvedColor`]. Covers the full grammar from tmux's `STYLES` section:
/// the 8 base names and their `bright` variants, `colourN`/`colorN` (0-255,
/// both spellings), `#rrggbb` hex, and `default`/`terminal`. `None` for
/// anything else, so the caller skips and logs the token instead of guessing.
fn parse_color(token: &str) -> Option<ResolvedColor> {
    if token == "default" || token == "terminal" {
        return Some(ResolvedColor::Theme);
    }
    if let Some(hex) = token.strip_prefix('#') {
        if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Rgba::try_from(token)
                .ok()
                .map(|rgba| ResolvedColor::Color(Hsla::from(rgba)));
        }
        return None;
    }
    if let Some(index) = token
        .strip_prefix("colour")
        .or_else(|| token.strip_prefix("color"))
    {
        let index: u8 = index.parse().ok()?;
        return Some(ResolvedColor::Color(Hsla::from(to_gpui_color(
            Color::Indexed(index),
        ))));
    }
    named_color(token)
        .map(|named| ResolvedColor::Color(Hsla::from(to_gpui_color(Color::Named(named)))))
}

/// tmux's 8 base color names and their `bright` variants (`STYLES` section).
fn named_color(token: &str) -> Option<NamedColor> {
    Some(match token {
        "black" => NamedColor::Black,
        "red" => NamedColor::Red,
        "green" => NamedColor::Green,
        "yellow" => NamedColor::Yellow,
        "blue" => NamedColor::Blue,
        "magenta" => NamedColor::Magenta,
        "cyan" => NamedColor::Cyan,
        "white" => NamedColor::White,
        "brightblack" => NamedColor::BrightBlack,
        "brightred" => NamedColor::BrightRed,
        "brightgreen" => NamedColor::BrightGreen,
        "brightyellow" => NamedColor::BrightYellow,
        "brightblue" => NamedColor::BrightBlue,
        "brightmagenta" => NamedColor::BrightMagenta,
        "brightcyan" => NamedColor::BrightCyan,
        "brightwhite" => NamedColor::BrightWhite,
        _ => return None,
    })
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

    // --- color resolution ---

    fn named(named: NamedColor) -> ResolvedColor {
        ResolvedColor::Color(Hsla::from(to_gpui_color(Color::Named(named))))
    }

    fn indexed(index: u8) -> ResolvedColor {
        ResolvedColor::Color(Hsla::from(to_gpui_color(Color::Indexed(index))))
    }

    #[test]
    fn test_parse_color_named_and_bright_variants() {
        assert_eq!(parse_color("black"), Some(named(NamedColor::Black)));
        assert_eq!(parse_color("red"), Some(named(NamedColor::Red)));
        assert_eq!(parse_color("white"), Some(named(NamedColor::White)));
        assert_eq!(parse_color("brightred"), Some(named(NamedColor::BrightRed)));
        assert_eq!(
            parse_color("brightwhite"),
            Some(named(NamedColor::BrightWhite))
        );
    }

    #[test]
    fn test_parse_color_indexed_both_spellings_and_bounds() {
        assert_eq!(parse_color("colour0"), Some(indexed(0)));
        assert_eq!(parse_color("colour255"), Some(indexed(255)));
        assert_eq!(parse_color("color123"), Some(indexed(123)));
        assert_eq!(parse_color("colour256"), None);
        assert_eq!(parse_color("colour"), None);
        assert_eq!(parse_color("colour-1"), None);
    }

    #[test]
    fn test_parse_color_hex_valid_and_malformed() {
        let expected = Rgba::try_from("#ff00aa").unwrap();
        assert_eq!(
            parse_color("#ff00aa"),
            Some(ResolvedColor::Color(Hsla::from(expected)))
        );
        assert_eq!(parse_color("#zzzzzz"), None);
        assert_eq!(parse_color("#fff"), None);
        assert_eq!(parse_color("#ff00a"), None);
    }

    #[test]
    fn test_parse_color_default_and_terminal_resolve_to_theme() {
        assert_eq!(parse_color("default"), Some(ResolvedColor::Theme));
        assert_eq!(parse_color("terminal"), Some(ResolvedColor::Theme));
    }

    #[test]
    fn test_parse_color_unknown_token_is_none() {
        assert_eq!(parse_color("notacolor"), None);
        assert_eq!(parse_color(""), None);
    }

    // --- base style parsing ---

    #[test]
    fn test_parse_style_bare_default_is_theme_no_attrs() {
        assert_eq!(parse_style("default"), StatusStyle::default());
        assert_eq!(parse_style("  default  "), StatusStyle::default());
    }

    #[test]
    fn test_parse_style_fg_bg_list_comma_separated() {
        let style = parse_style("bg=colour234,fg=colour253");
        assert_eq!(style.bg, indexed(234));
        assert_eq!(style.fg, indexed(253));
    }

    #[test]
    fn test_parse_style_space_separated_attrs_from_man_page_example() {
        let style = parse_style("fg=yellow bold underscore blink");
        assert_eq!(style.fg, named(NamedColor::Yellow));
        assert!(style.attrs.bold);
        assert!(style.attrs.underline);
        assert!(style.attrs.blink);
    }

    #[test]
    fn test_parse_style_no_prefixed_attribute_from_man_page_example() {
        let style = parse_style("bg=black,fg=default,noreverse");
        assert_eq!(style.bg, named(NamedColor::Black));
        assert_eq!(style.fg, ResolvedColor::Theme);
        assert!(!style.attrs.reverse);
    }

    #[test]
    fn test_parse_style_unknown_token_skipped_rest_still_applied() {
        let style = parse_style("fg=red,bogus,bold");
        assert_eq!(style.fg, named(NamedColor::Red));
        assert!(style.attrs.bold);
    }

    #[test]
    fn test_parse_style_none_clears_attrs_only() {
        let style = parse_style("fg=red,bold,none");
        assert_eq!(style.fg, named(NamedColor::Red));
        assert!(!style.attrs.bold);
    }

    #[test]
    fn test_parse_style_range_list_align_fill_us_are_no_ops() {
        let style = parse_style("fg=red,range=left,list=on,align=centre,fill=blue,us=green");
        assert_eq!(style.fg, named(NamedColor::Red));
        assert_eq!(style.bg, ResolvedColor::Theme);
    }

    // --- style-run parsing ---

    #[test]
    fn test_parse_style_runs_multiple_runs_split_at_tags() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("#[fg=green]ok#[fg=yellow]warn", base);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "ok");
        assert_eq!(runs[0].style.fg, named(NamedColor::Green));
        assert_eq!(runs[1].text, "warn");
        assert_eq!(runs[1].style.fg, named(NamedColor::Yellow));
    }

    #[test]
    fn test_parse_style_runs_default_tag_resets_to_base() {
        let base = parse_style("bg=colour234,fg=colour253");
        let runs = parse_style_runs("#[fg=green,bold]ok#[default] plain", base);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].style.fg, named(NamedColor::Green));
        assert!(runs[0].style.attrs.bold);
        assert_eq!(runs[1].text, " plain");
        assert_eq!(runs[1].style, base);
    }

    #[test]
    fn test_parse_style_runs_leading_text_before_first_tag_uses_base() {
        let base = parse_style("fg=colour200");
        let runs = parse_style_runs("host #[fg=red]alert", base);
        assert_eq!(runs[0].text, "host ");
        assert_eq!(runs[0].style, base);
        assert_eq!(runs[1].text, "alert");
    }

    #[test]
    fn test_parse_style_runs_range_and_list_tags_are_no_ops() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("#[fg=red]a#[range=left]b#[list=on]c", base);
        // range=/list= never change style, so all three text segments stay red.
        assert_eq!(runs.len(), 3);
        for run in &runs {
            assert_eq!(run.style.fg, named(NamedColor::Red));
        }
        assert_eq!(runs[0].text, "a");
        assert_eq!(runs[1].text, "b");
        assert_eq!(runs[2].text, "c");
    }

    #[test]
    fn test_parse_style_runs_literal_hash_bracket_from_double_hash_escaping() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("pre##[post", base);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "pre#[post");
        assert_eq!(runs[0].style, base);
    }

    #[test]
    fn test_parse_style_runs_unknown_token_inside_tag_skipped_others_applied() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("#[fg=red,bogus=xyz]text", base);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "text");
        assert_eq!(runs[0].style.fg, named(NamedColor::Red));
    }

    #[test]
    fn test_parse_style_runs_unterminated_tag_degrades_to_literal_text() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("abc#[fg=red", base);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "abc#[fg=red");
        assert_eq!(runs[0].style, base);
    }

    #[test]
    fn test_parse_style_runs_multibyte_utf8_content_not_split() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("#[fg=green]\u{65e5}\u{672c}\u{8a9e}#[default]end", base);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "\u{65e5}\u{672c}\u{8a9e}");
        assert_eq!(runs[0].style.fg, named(NamedColor::Green));
        assert_eq!(runs[1].text, "end");
    }

    #[test]
    fn test_parse_style_runs_consecutive_tags_with_no_text_between_do_not_emit_empty_run() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("#[fg=red]#[bold]text", base);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "text");
        assert_eq!(runs[0].style.fg, named(NamedColor::Red));
        assert!(runs[0].style.attrs.bold);
    }

    #[test]
    fn test_parse_style_runs_empty_tag_is_no_op() {
        let base = StatusStyle::default();
        let runs = parse_style_runs("a#[]b", base);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "a");
        assert_eq!(runs[1].text, "b");
        assert_eq!(runs[0].style, base);
        assert_eq!(runs[1].style, base);
    }

    #[test]
    fn test_parse_style_runs_empty_input_yields_no_runs() {
        assert_eq!(parse_style_runs("", StatusStyle::default()), Vec::new());
    }

    // --- length truncation ---

    fn run(text: &str) -> StyleRun {
        StyleRun {
            text: text.to_string(),
            style: StatusStyle::default(),
        }
    }

    #[test]
    fn test_truncate_runs_under_length_is_unchanged() {
        let runs = vec![run("hi")];
        let truncated = truncate_runs(runs.clone(), 10);
        assert_eq!(truncated, runs);
    }

    #[test]
    fn test_truncate_runs_exact_length_is_unchanged() {
        let runs = vec![run("hello")];
        let truncated = truncate_runs(runs.clone(), 5);
        assert_eq!(truncated, runs);
    }

    #[test]
    fn test_truncate_runs_over_length_cuts_mid_run_keeps_style() {
        let mut styled = run("hello world");
        styled.style.fg = named(NamedColor::Red);
        let truncated = truncate_runs(vec![styled], 5);
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].text, "hello");
        assert_eq!(truncated[0].style.fg, named(NamedColor::Red));
    }

    #[test]
    fn test_truncate_runs_drops_runs_entirely_past_the_limit() {
        let runs = vec![run("hello"), run(" world")];
        let truncated = truncate_runs(runs, 5);
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].text, "hello");
    }

    #[test]
    fn test_truncate_runs_zero_length_yields_no_runs() {
        let runs = vec![run("hello")];
        assert_eq!(truncate_runs(runs, 0), Vec::new());
    }

    #[test]
    fn test_truncate_runs_multibyte_utf8_counted_per_char_not_byte() {
        let truncated = truncate_runs(vec![run("\u{65e5}\u{672c}\u{8a9e}!")], 3);
        assert_eq!(truncated[0].text, "\u{65e5}\u{672c}\u{8a9e}");
    }
}
