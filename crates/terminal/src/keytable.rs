//! tmux key-table lookup layer.
//!
//! Mirrors tmux's key tables client-side from the live config, so configured
//! bindings can be resolved while focus is in a rift pane. This module is the
//! pure data layer of the Phase 7 mirroring spec
//! (`docs/spec-tmux-keytable-mirroring.md`): it turns the request/response
//! output of rift's own `list-keys`/`show-options` queries into an in-memory
//! `(table, key) -> command` lookup plus the session prefix/repeat options, and
//! maps a GPUI keystroke to the tmux key name so a press can be matched against
//! parsed entries. The prefix state machine, dispatch, and command interception
//! (issues #211/#212) build on top of this layer; nothing here touches the seam
//! or pane content.

use std::collections::HashMap;

use gpui::Keystroke;

/// A single resolved key binding: the raw tmux command to dispatch and whether
/// it is a repeat binding (`bind -r`, honoring `repeat-time`).
///
/// `command` is kept verbatim as `list-keys` printed it (tmux quoting/escaping
/// intact) so the dispatch layer can forward or further parse it; this layer
/// never interprets the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub command: String,
    pub repeat: bool,
}

/// A mirror of tmux's key tables, keyed by table name then normalized key name.
///
/// Lookups are in-memory and allocation-free on the hot path. Mouse bindings are
/// consciously skipped (keyboard lookup only) and unparseable lines are skipped
/// and logged, never failing the whole table — a partial table beats a failed
/// one (spec risk table).
#[derive(Debug, Default, Clone)]
pub struct KeyTable {
    tables: HashMap<String, HashMap<String, Binding>>,
}

impl KeyTable {
    /// Resolve a binding for `key` (a normalized tmux key name, e.g. from
    /// [`keystroke_to_tmux_key`]) in `table`.
    pub fn get(&self, table: &str, key: &str) -> Option<&Binding> {
        self.tables.get(table)?.get(key)
    }

    /// All bindings of a single table, or `None` if the table is absent.
    pub fn table(&self, table: &str) -> Option<&HashMap<String, Binding>> {
        self.tables.get(table)
    }

    /// Total number of bindings across all tables.
    pub fn len(&self) -> usize {
        self.tables.values().map(HashMap::len).sum()
    }

    /// Whether no bindings were parsed.
    pub fn is_empty(&self) -> bool {
        self.tables.values().all(HashMap::is_empty)
    }
}

/// Parse the output of `list-keys` into a [`KeyTable`].
///
/// Each line has the form `bind-key [-r] -T <table> <key> <command...>`. The key
/// field is unescaped (tmux backslash/quote escaping); the command is preserved
/// raw. Mouse-binding entries are skipped (logged at trace level); lines that do
/// not parse are skipped and logged at warn level — the table is never failed.
pub fn parse_list_keys(output: &str) -> KeyTable {
    let mut table = KeyTable::default();
    for line in output.lines() {
        match parse_bind_line(line) {
            ParseOutcome::Binding {
                table: name,
                key,
                command,
                repeat,
            } => {
                if is_mouse_key(&key) {
                    tracing::trace!(table = %name, key = %key, "skipping mouse binding");
                    continue;
                }
                let normalized = normalize_tmux_key(&key);
                table
                    .tables
                    .entry(name)
                    .or_default()
                    .insert(normalized, Binding { command, repeat });
            }
            ParseOutcome::Blank => {}
            ParseOutcome::Malformed => {
                tracing::warn!(line = %line, "skipping unparseable list-keys line");
            }
        }
    }
    table
}

enum ParseOutcome {
    Binding {
        table: String,
        key: String,
        command: String,
        repeat: bool,
    },
    Blank,
    Malformed,
}

fn parse_bind_line(line: &str) -> ParseOutcome {
    let mut pos = 0;
    let (first, after_first) = match lex_token(line, pos) {
        Some(token) => token,
        None => return ParseOutcome::Blank,
    };
    if first != "bind-key" {
        return ParseOutcome::Malformed;
    }
    pos = after_first;

    let mut repeat = false;
    loop {
        let (token, after) = match lex_token(line, pos) {
            Some(token) => token,
            None => return ParseOutcome::Malformed,
        };
        match token.as_str() {
            "-r" => {
                repeat = true;
                pos = after;
            }
            "-T" => {
                let (table, after_table) = match lex_token(line, after) {
                    Some(token) => token,
                    None => return ParseOutcome::Malformed,
                };
                // tmux always emits the key immediately after the table name.
                let (key, cmd_start) = match lex_token(line, after_table) {
                    Some(token) => token,
                    None => return ParseOutcome::Malformed,
                };
                let command = line[cmd_start..].trim().to_string();
                return ParseOutcome::Binding {
                    table,
                    key,
                    command,
                    repeat,
                };
            }
            // Forward-compatible skip of any other boolean flag tmux may add.
            other if other.starts_with('-') && other.len() > 1 => {
                pos = after;
            }
            // A bareword before `-T` is not a shape we recognize.
            _ => return ParseOutcome::Malformed,
        }
    }
}

/// tmux mouse/wheel key names, optionally modifier-prefixed
/// (e.g. `M-MouseDown3Pane`). Matched by substring so every variant is caught.
fn is_mouse_key(key: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "Mouse",
        "Wheel",
        "DoubleClick",
        "TripleClick",
        "SecondClick",
    ];
    MARKERS.iter().any(|marker| key.contains(marker))
}

/// Session prefix and repeat options, discovered via `show-options`.
///
/// The prefix is a session option (`prefix`, plus optional `prefix2`), not a
/// `list-keys` entry, so it must be queried separately; `repeat-time` is likewise
/// an option. Values are normalized tmux key names; `None` means the option is
/// unset (`prefix2 None`) or disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixOptions {
    pub prefix: Option<String>,
    pub prefix2: Option<String>,
    pub repeat_time: u64,
}

impl Default for PrefixOptions {
    /// tmux's own defaults, used as the base so a partial `show-options` response
    /// still yields a usable set.
    fn default() -> Self {
        Self {
            prefix: Some(normalize_tmux_key("C-b")),
            prefix2: None,
            repeat_time: 500,
        }
    }
}

/// Parse `show-options` output into [`PrefixOptions`], overriding the tmux
/// defaults for any of `prefix`, `prefix2`, `repeat-time` that appear. Each line
/// is `name value`; unrelated options and unparseable values are ignored.
pub fn parse_options(output: &str) -> PrefixOptions {
    let mut options = PrefixOptions::default();
    for line in output.lines() {
        let Some((name, after_name)) = lex_token(line, 0) else {
            continue;
        };
        let value = lex_token(line, after_name).map(|(value, _)| value);
        match name.as_str() {
            "prefix" => options.prefix = parse_key_option(value.as_deref()),
            "prefix2" => options.prefix2 = parse_key_option(value.as_deref()),
            "repeat-time" => {
                if let Some(parsed) = value.and_then(|value| value.parse::<u64>().ok()) {
                    options.repeat_time = parsed;
                }
            }
            _ => {}
        }
    }
    options
}

fn parse_key_option(value: Option<&str>) -> Option<String> {
    match value {
        Some(value) if !value.is_empty() && value != "None" => Some(normalize_tmux_key(value)),
        _ => None,
    }
}

/// Normalize a tmux key name into an order-independent canonical form.
///
/// tmux prints modifier order inconsistently across key kinds (`M-C-b`,
/// `S-C-x`, `C-M-S-F5`), so both sides of a lookup — parsed entries and mapped
/// keystrokes — pass through here to agree on one representation: modifiers in
/// fixed `C- M- S-` order followed by the base key.
pub fn normalize_tmux_key(raw: &str) -> String {
    let mut rest = raw;
    let (mut ctrl, mut meta, mut shift) = (false, false, false);
    loop {
        if let Some(stripped) = rest.strip_prefix("C-") {
            ctrl = true;
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix("M-") {
            meta = true;
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix("S-") {
            shift = true;
            rest = stripped;
        } else {
            break;
        }
    }
    canonical_key(ctrl, meta, shift, rest)
}

fn canonical_key(ctrl: bool, meta: bool, shift: bool, base: &str) -> String {
    let mut out = String::with_capacity(base.len() + 6);
    if ctrl {
        out.push_str("C-");
    }
    if meta {
        out.push_str("M-");
    }
    if shift {
        out.push_str("S-");
    }
    out.push_str(base);
    out
}

/// Map a GPUI [`Keystroke`] to the normalized tmux key name it would match in a
/// [`KeyTable`], or `None` when the key has no tmux representation (it then falls
/// through to the existing typing path). The reverse direction of the PTY
/// `encode_keystroke`.
pub fn keystroke_to_tmux_key(keystroke: &Keystroke) -> Option<String> {
    let key = keystroke.key.as_str();
    if matches!(key, "control" | "alt" | "shift" | "platform" | "function") {
        return None;
    }

    let modifiers = &keystroke.modifiers;
    let ctrl = modifiers.control;
    let meta = modifiers.alt;
    let shift = modifiers.shift;

    // Shift+Tab is tmux `BTab`, not `S-Tab`.
    if key == "tab" && shift && !ctrl && !meta {
        return Some(canonical_key(false, false, false, "BTab"));
    }

    // Special named keys keep Shift as an explicit S- modifier.
    if let Some(name) = special_key_name(key) {
        return Some(canonical_key(ctrl, meta, shift, name));
    }

    // A lone Shift on a letter folds into the glyph's case (`B`); combined with
    // Ctrl/Alt, tmux keeps S- (`S-C-x`).
    if let Some(letter) = single_ascii_alpha(key) {
        if shift && !ctrl && !meta {
            return Some(canonical_key(
                false,
                false,
                false,
                &letter.to_ascii_uppercase().to_string(),
            ));
        }
        return Some(canonical_key(
            ctrl,
            meta,
            shift,
            &letter.to_ascii_lowercase().to_string(),
        ));
    }

    // Other printable characters (digits, symbols): the glyph already encodes
    // Shift, so it is not emitted as a modifier.
    if let Some(glyph) = printable_glyph(keystroke) {
        return Some(canonical_key(ctrl, meta, false, &glyph));
    }

    None
}

/// GPUI key name -> tmux output key name for keys tmux denotes by name rather
/// than glyph (mirrors tmux's `list-keys` output spelling).
fn special_key_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PPage",
        "pagedown" => "NPage",
        "insert" => "IC",
        "delete" => "DC",
        "space" => "Space",
        "tab" => "Tab",
        "enter" => "Enter",
        "escape" => "Escape",
        "backspace" => "BSpace",
        "f1" => "F1",
        "f2" => "F2",
        "f3" => "F3",
        "f4" => "F4",
        "f5" => "F5",
        "f6" => "F6",
        "f7" => "F7",
        "f8" => "F8",
        "f9" => "F9",
        "f10" => "F10",
        "f11" => "F11",
        "f12" => "F12",
        _ => return None,
    })
}

fn single_ascii_alpha(key: &str) -> Option<char> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
        _ => None,
    }
}

fn printable_glyph(keystroke: &Keystroke) -> Option<String> {
    if let Some(glyph) = keystroke
        .key_char
        .as_deref()
        .and_then(single_printable_char)
    {
        return Some(glyph.to_string());
    }
    single_printable_char(keystroke.key.as_str()).map(|c| c.to_string())
}

fn single_printable_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(c),
        _ => None,
    }
}

/// How a resolved bound command should be handled once the prefix engine
/// decides to dispatch it — the command-interception taxonomy from
/// `docs/spec-tmux-keytable-mirroring.md` (command interception step, #212).
/// Classification runs on the command text alone (already parsed out of
/// `list-keys`); nothing here touches pane content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Send the command through the single command seam unchanged.
    Dispatch(String),
    /// A pane-mode (`copy-mode`, `choose-*`, `clock-mode`, `customize-mode`), a
    /// `switch-client -T` to an unmirrored table, or a client-interaction
    /// command that renders on the issuing client (`command-prompt`,
    /// `display-menu`, `display-panes`, `display-popup`): never dispatched —
    /// dispatching a pane mode would shove the *shared* pane into it, breaking
    /// co-attached native clients, and the others would silently no-op on a
    /// control client. `hint` names the GUI affordance that replaces it.
    Hint(&'static str),
    /// A bound `confirm-before` command: render a native confirm dialog with
    /// `prompt`, dispatching `wrapped` only if the user confirms.
    Confirm { prompt: String, wrapped: String },
}

/// Classify a resolved bound command (verbatim `list-keys` text) into a
/// [`DispatchDecision`]. `send-prefix` and every other bound command —
/// including one that itself runs `bind-key`/`set-option`/`source-file` —
/// dispatch unchanged; the refresh-trigger check on those is
/// [`mutates_bindings`], a separate concern from this taxonomy.
pub fn classify_command(command: &str) -> DispatchDecision {
    let verb = command.split_whitespace().next().unwrap_or("");
    match verb {
        "copy-mode" => DispatchDecision::Hint("copy-mode: use the mouse-wheel scrollback instead"),
        "clock-mode" => DispatchDecision::Hint("clock-mode is not available in rift"),
        "customize-mode" => DispatchDecision::Hint("customize-mode is not available in rift"),
        _ if verb.starts_with("choose-") => {
            DispatchDecision::Hint("choose-mode has no GUI picker yet")
        }
        "command-prompt" => DispatchDecision::Hint("command-prompt has no GUI form yet"),
        "display-menu" => DispatchDecision::Hint("display-menu has no GUI form yet"),
        "display-panes" => DispatchDecision::Hint("display-panes has no GUI form yet"),
        "display-popup" => DispatchDecision::Hint("display-popup has no GUI form yet"),
        "confirm-before" => match parse_confirm_before(command) {
            Some(confirm) => DispatchDecision::Confirm {
                prompt: confirm.prompt,
                wrapped: confirm.wrapped,
            },
            // Never silently no-op a bound confirm-before just because this
            // one didn't parse — dispatch it verbatim instead.
            None => DispatchDecision::Dispatch(command.to_string()),
        },
        "switch-client" => match switch_client_table(command) {
            Some(table) if table != "prefix" && table != "root" => {
                DispatchDecision::Hint("that key table is not mirrored")
            }
            _ => DispatchDecision::Dispatch(command.to_string()),
        },
        _ => DispatchDecision::Dispatch(command.to_string()),
    }
}

/// A parsed `confirm-before` bound command: the `-p` prompt text and the
/// wrapped command to dispatch on confirm, kept verbatim as `list-keys`
/// printed it (the same raw convention as [`Binding::command`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmBefore {
    pub prompt: String,
    pub wrapped: String,
}

/// Parse a bound `confirm-before` command — tmux's shape is `confirm-before
/// [-y] [-p prompt] command`. The wrapped command is kept raw (it may itself
/// carry further tmux quoting the dispatch seam re-parses); `prompt` falls
/// back to a placeholder when `-p` is absent, since tmux itself renders a
/// default prompt in that case — a stock kill confirmation must never
/// silently no-op just because a config bound `confirm-before` bare. `None`
/// when the line is not a `confirm-before` or carries no wrapped command.
pub fn parse_confirm_before(command: &str) -> Option<ConfirmBefore> {
    let (first, mut pos) = lex_token(command, 0)?;
    if first != "confirm-before" {
        return None;
    }
    let mut prompt: Option<String> = None;
    loop {
        let token_start = pos;
        let (token, after) = lex_token(command, pos)?;
        match token.as_str() {
            "-p" => {
                let (value, after_value) = lex_token(command, after)?;
                prompt = Some(value);
                pos = after_value;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                pos = after;
            }
            _ => {
                pos = token_start;
                break;
            }
        }
    }
    let wrapped = command[pos..].trim().to_string();
    if wrapped.is_empty() {
        return None;
    }
    Some(ConfirmBefore {
        prompt: prompt.unwrap_or_else(|| "confirm?".to_string()),
        wrapped,
    })
}

/// Extract the `-T <table>` target from a `switch-client` bound command, if
/// present. `None` when the command is not `switch-client`, or carries no
/// `-T` (a plain pane/window switch, not a key-table switch).
fn switch_client_table(command: &str) -> Option<String> {
    let (first, mut pos) = lex_token(command, 0)?;
    if first != "switch-client" {
        return None;
    }
    loop {
        let (token, after) = lex_token(command, pos)?;
        if token == "-T" {
            let (table, _) = lex_token(command, after)?;
            return Some(table);
        }
        pos = after;
    }
}

/// Whether a dispatched command could change the mirrored key table or the
/// prefix/repeat options — the refresh-trigger check from
/// `docs/spec-tmux-keytable-mirroring.md` ("Table refresh/invalidation").
/// `bind-key`/`unbind-key`/`source-file` always mutate bindings;
/// `set-option`/`set` only matters when it targets `prefix`, `prefix2`, or
/// `repeat-time` — an unrelated option (`set -g mouse on`) must not trigger a
/// refresh. Matching is a plain token scan (not tmux-quote-aware): a false
/// positive costs one harmless extra refresh, but a false negative would miss
/// a real prefix change, so the scan errs toward matching.
pub fn mutates_bindings(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    match tokens.next().unwrap_or("") {
        "bind-key" | "unbind-key" | "source-file" => true,
        "set-option" | "set" => {
            tokens.any(|token| matches!(token, "prefix" | "prefix2" | "repeat-time"))
        }
        _ => false,
    }
}

/// Lex one whitespace-delimited token of `list-keys`/`show-options` output
/// starting at or after `start`, honoring tmux's escaping: single quotes are
/// literal, double quotes allow backslash escapes, and a bare backslash escapes
/// the next character. Returns the unescaped token and the byte offset just past
/// it, or `None` at end of input.
///
/// `pub(crate)` so [`crate::statusline`]'s `show-options` parser shares one
/// tmux-quoting-aware lexer instead of a second implementation.
pub(crate) fn lex_token(line: &str, start: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let mut i = start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }

    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => break,
            b'\'' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'\'' {
                    i = push_char(line, i, &mut out);
                }
                if i < bytes.len() {
                    i += 1; // closing quote
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1;
                    }
                    i = push_char(line, i, &mut out);
                }
                if i < bytes.len() {
                    i += 1; // closing quote
                }
            }
            b'\\' => {
                i += 1;
                if i < bytes.len() {
                    i = push_char(line, i, &mut out);
                }
            }
            _ => i = push_char(line, i, &mut out),
        }
    }
    Some((out, i))
}

/// Push the UTF-8 character at byte index `i` onto `out` and return the next
/// byte index. Defensive against a non-boundary index so it can never panic.
fn push_char(line: &str, i: usize, out: &mut String) -> usize {
    if !line.is_char_boundary(i) {
        return i + 1;
    }
    match line[i..].chars().next() {
        Some(c) => {
            out.push(c);
            i + c.len_utf8()
        }
        None => i + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    // Real `tmux 3.4` output captured with `tmux list-keys` (default config),
    // covering the cases the spec/issue require: quoted/escaped keys and
    // commands, `-r` repeat binds, `-n` root (`-T root`) binds, and mouse
    // bindings (consciously skipped).
    const LIST_KEYS_FIXTURE: &str = r#"
bind-key    -T prefix C-b     send-prefix
bind-key    -T prefix Space   next-layout
bind-key    -T prefix !       break-pane
bind-key    -T prefix \"      split-window
bind-key    -T prefix \#      list-buffers
bind-key    -T prefix \%      split-window -h
bind-key    -T prefix &       confirm-before -p "kill-window #W? (y/n)" kill-window
bind-key    -T prefix \'      command-prompt -T window-target -p index { select-window -t ":%%" }
bind-key    -T prefix -       delete-buffer
bind-key    -T prefix \;      last-pane
bind-key    -T prefix c       new-window
bind-key    -T prefix x       confirm-before -p "kill-pane #P? (y/n)" kill-pane
bind-key    -T prefix [       copy-mode
bind-key -r -T prefix Up      select-pane -U
bind-key -r -T prefix Left    select-pane -L
bind-key -r -T prefix M-Left  resize-pane -L 5
bind-key    -T prefix M-1     select-layout even-horizontal
bind-key    -T copy-mode "M-{" send-keys -X previous-paragraph
bind-key    -T root  M-Left   select-pane -L
bind-key    -T root  MouseDown1Pane         select-pane -t = \; send-keys -M
bind-key    -T root  M-MouseDown3Pane       display-menu -T title
bind-key    -T copy-mode WheelUpPane        select-pane \; send-keys -X -N 5 scroll-up
"#;

    fn keystroke(key: &str, modifiers: Modifiers, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: key_char.map(Into::into),
        }
    }

    fn mods(control: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            control,
            alt,
            shift,
            ..Modifiers::none()
        }
    }

    // --- list-keys parser ---

    #[test]
    fn test_parse_list_keys_resolves_plain_binding() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        let binding = table.get("prefix", "c").expect("prefix c");
        assert_eq!(binding.command, "new-window");
        assert!(!binding.repeat);
    }

    #[test]
    fn test_parse_list_keys_unescapes_quoted_keys() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        assert_eq!(table.get("prefix", "\"").unwrap().command, "split-window");
        assert_eq!(table.get("prefix", "#").unwrap().command, "list-buffers");
        assert_eq!(table.get("prefix", "%").unwrap().command, "split-window -h");
        assert_eq!(table.get("prefix", ";").unwrap().command, "last-pane");
        assert_eq!(table.get("prefix", "-").unwrap().command, "delete-buffer");
    }

    #[test]
    fn test_parse_list_keys_preserves_command_quoting_raw() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        assert_eq!(
            table.get("prefix", "&").unwrap().command,
            r#"confirm-before -p "kill-window #W? (y/n)" kill-window"#
        );
        // Brace blocks and `%%` placeholders survive verbatim.
        assert_eq!(
            table.get("prefix", "'").unwrap().command,
            r#"command-prompt -T window-target -p index { select-window -t ":%%" }"#
        );
    }

    #[test]
    fn test_parse_list_keys_marks_repeat_bindings() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        assert!(table.get("prefix", "Up").unwrap().repeat);
        assert!(table.get("prefix", "Left").unwrap().repeat);
        assert!(table.get("prefix", "M-Left").unwrap().repeat);
        // Non-repeat binding in the same table.
        assert!(!table.get("prefix", "c").unwrap().repeat);
    }

    #[test]
    fn test_parse_list_keys_normalizes_modifier_keys() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        // `"M-{"` (double-quoted) unescapes and normalizes to `M-{`.
        assert_eq!(
            table.get("copy-mode", "M-{").unwrap().command,
            "send-keys -X previous-paragraph"
        );
        assert_eq!(
            table.get("prefix", "M-1").unwrap().command,
            "select-layout even-horizontal"
        );
    }

    #[test]
    fn test_parse_list_keys_skips_mouse_bindings() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        // Root keeps its keyboard binding...
        assert_eq!(
            table.get("root", "M-Left").unwrap().command,
            "select-pane -L"
        );
        // ...but every mouse/wheel entry is skipped.
        assert!(table.get("root", "MouseDown1Pane").is_none());
        assert!(table.get("root", "M-MouseDown3Pane").is_none());
        assert!(table.get("copy-mode", "WheelUpPane").is_none());
    }

    #[test]
    fn test_parse_list_keys_skips_malformed_without_failing_table() {
        let input = "\
bind-key    -T prefix c       new-window
this is not a bind-key line
bind-key
bind-key    -T prefix
bind-key    -T prefix d       detach-client
";
        let table = parse_list_keys(input);
        // Good lines on both sides of the malformed ones still resolve.
        assert_eq!(table.get("prefix", "c").unwrap().command, "new-window");
        assert_eq!(table.get("prefix", "d").unwrap().command, "detach-client");
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn test_parse_list_keys_empty_input() {
        let table = parse_list_keys("");
        assert!(table.is_empty());
        assert!(table.get("prefix", "c").is_none());
    }

    // --- show-options discovery ---

    #[test]
    fn test_parse_options_defaults() {
        let options = parse_options("");
        assert_eq!(options.prefix.as_deref(), Some("C-b"));
        assert_eq!(options.prefix2, None);
        assert_eq!(options.repeat_time, 500);
    }

    #[test]
    fn test_parse_options_custom_prefix() {
        let input = "\
prefix C-a
prefix2 C-b
repeat-time 300
";
        let options = parse_options(input);
        assert_eq!(options.prefix.as_deref(), Some("C-a"));
        assert_eq!(options.prefix2.as_deref(), Some("C-b"));
        assert_eq!(options.repeat_time, 300);
    }

    #[test]
    fn test_parse_options_none_clears_prefix2() {
        let options = parse_options("prefix2 None\n");
        assert_eq!(options.prefix2, None);
    }

    #[test]
    fn test_parse_options_ignores_unrelated_and_malformed() {
        let input = "\
status on
prefix C-a
repeat-time not-a-number
";
        let options = parse_options(input);
        assert_eq!(options.prefix.as_deref(), Some("C-a"));
        // Unparseable repeat-time keeps the default.
        assert_eq!(options.repeat_time, 500);
    }

    // --- normalize ---

    #[test]
    fn test_normalize_is_order_independent() {
        // tmux prints these orders for the same physical chords; all collapse.
        assert_eq!(normalize_tmux_key("S-C-x"), normalize_tmux_key("C-S-x"));
        assert_eq!(normalize_tmux_key("M-C-b"), normalize_tmux_key("C-M-b"));
        assert_eq!(
            normalize_tmux_key("C-M-S-F5"),
            normalize_tmux_key("S-M-C-F5")
        );
    }

    #[test]
    fn test_normalize_plain_and_modifier_keys() {
        assert_eq!(normalize_tmux_key("c"), "c");
        assert_eq!(normalize_tmux_key("C-b"), "C-b");
        assert_eq!(normalize_tmux_key("M-{"), "M-{");
        assert_eq!(normalize_tmux_key("-"), "-");
    }

    // --- keystroke -> tmux key mapping ---

    #[test]
    fn test_map_ctrl_letter() {
        let ks = keystroke("b", mods(true, false, false), None);
        assert_eq!(keystroke_to_tmux_key(&ks).as_deref(), Some("C-b"));
    }

    #[test]
    fn test_map_alt_arrow() {
        let ks = keystroke("left", mods(false, true, false), None);
        assert_eq!(keystroke_to_tmux_key(&ks).as_deref(), Some("M-Left"));
    }

    #[test]
    fn test_map_shift_function_key() {
        let ks = keystroke("f5", mods(false, false, true), None);
        assert_eq!(keystroke_to_tmux_key(&ks).as_deref(), Some("S-F5"));
    }

    #[test]
    fn test_map_special_key_names() {
        let cases = [
            ("up", "Up"),
            ("pageup", "PPage"),
            ("pagedown", "NPage"),
            ("delete", "DC"),
            ("insert", "IC"),
            ("backspace", "BSpace"),
            ("space", "Space"),
            ("escape", "Escape"),
        ];
        for (key, expected) in cases {
            let ks = keystroke(key, Modifiers::none(), None);
            assert_eq!(
                keystroke_to_tmux_key(&ks).as_deref(),
                Some(expected),
                "{key}"
            );
        }
    }

    #[test]
    fn test_map_shift_tab_is_btab() {
        let ks = keystroke("tab", mods(false, false, true), None);
        assert_eq!(keystroke_to_tmux_key(&ks).as_deref(), Some("BTab"));
    }

    #[test]
    fn test_map_plain_letter_and_symbol() {
        let letter = keystroke("c", Modifiers::none(), Some("c"));
        assert_eq!(keystroke_to_tmux_key(&letter).as_deref(), Some("c"));
        // Shift+5 reports the shifted glyph; Shift is folded into it, not S-.
        let percent = keystroke("5", mods(false, false, true), Some("%"));
        assert_eq!(keystroke_to_tmux_key(&percent).as_deref(), Some("%"));
    }

    #[test]
    fn test_map_lone_shift_letter_uppercases() {
        let ks = keystroke("b", mods(false, false, true), Some("B"));
        assert_eq!(keystroke_to_tmux_key(&ks).as_deref(), Some("B"));
    }

    #[test]
    fn test_map_ctrl_shift_letter_keeps_shift() {
        // tmux prints `S-C-x`; mapping must normalize to the same canonical key.
        let ks = keystroke("x", mods(true, false, true), None);
        assert_eq!(
            keystroke_to_tmux_key(&ks),
            Some(normalize_tmux_key("S-C-x"))
        );
    }

    #[test]
    fn test_map_unmappable_keys_fall_through() {
        for bare in ["control", "alt", "shift", "platform", "function"] {
            let ks = keystroke(bare, Modifiers::none(), None);
            assert_eq!(keystroke_to_tmux_key(&ks), None, "{bare}");
        }
        // A named key with no glyph and no tmux name does not map.
        let unknown = keystroke("medianext", Modifiers::none(), None);
        assert_eq!(keystroke_to_tmux_key(&unknown), None);
    }

    // --- end-to-end: a mapped keystroke resolves a parsed binding ---

    #[test]
    fn test_mapped_keystroke_resolves_parsed_binding() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);

        let ctrl_b = keystroke("b", mods(true, false, false), None);
        let key = keystroke_to_tmux_key(&ctrl_b).expect("C-b maps");
        assert_eq!(table.get("prefix", &key).unwrap().command, "send-prefix");

        let alt_left = keystroke("left", mods(false, true, false), None);
        let key = keystroke_to_tmux_key(&alt_left).expect("M-Left maps");
        assert!(table.get("root", &key).unwrap().repeat == false);
        assert_eq!(table.get("root", &key).unwrap().command, "select-pane -L");
    }

    // --- command classification (#212) ---

    #[test]
    fn test_classify_command_dispatches_plain_commands() {
        for cmd in [
            "new-window",
            "split-window -h",
            "select-pane -L",
            "send-prefix",
        ] {
            assert_eq!(
                classify_command(cmd),
                DispatchDecision::Dispatch(cmd.to_string()),
                "{cmd}"
            );
        }
    }

    #[test]
    fn test_classify_command_hints_pane_modes() {
        for cmd in ["copy-mode", "copy-mode -u", "clock-mode", "customize-mode"] {
            assert!(
                matches!(classify_command(cmd), DispatchDecision::Hint(_)),
                "{cmd}"
            );
        }
    }

    #[test]
    fn test_classify_command_hints_choose_mode_variants() {
        for cmd in [
            "choose-tree",
            "choose-window",
            "choose-client",
            "choose-buffer",
        ] {
            assert!(
                matches!(classify_command(cmd), DispatchDecision::Hint(_)),
                "{cmd}"
            );
        }
    }

    #[test]
    fn test_classify_command_hints_client_interaction_display_forms() {
        for cmd in [
            "command-prompt",
            "display-menu -T title",
            "display-panes",
            "display-popup",
        ] {
            assert!(
                matches!(classify_command(cmd), DispatchDecision::Hint(_)),
                "{cmd}"
            );
        }
    }

    #[test]
    fn test_classify_command_confirm_before_becomes_confirm_decision() {
        let table = parse_list_keys(LIST_KEYS_FIXTURE);
        let kill_window = &table.get("prefix", "&").unwrap().command;
        match classify_command(kill_window) {
            DispatchDecision::Confirm { prompt, wrapped } => {
                assert_eq!(prompt, "kill-window #W? (y/n)");
                assert_eq!(wrapped, "kill-window");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_command_confirm_before_without_wrapped_command_dispatches_raw() {
        // Never silently no-op: an unparseable confirm-before still dispatches.
        assert_eq!(
            classify_command("confirm-before -p prompt"),
            DispatchDecision::Dispatch("confirm-before -p prompt".to_string())
        );
    }

    #[test]
    fn test_classify_command_confirm_before_wrapped_pane_mode_reclassifies_to_hint() {
        // A user binding can wrap a pane-mode command in `confirm-before`
        // (e.g. `bind X confirm-before -p "copy?" copy-mode`). On confirm the
        // GUI must re-run `wrapped` through `classify_command` rather than
        // dispatch it unconditionally — dispatching `copy-mode` would shove
        // the shared pane into it, breaking co-attached native clients
        // (`docs/spec-tmux-keytable-mirroring.md`).
        let confirm = match classify_command(r#"confirm-before -p "copy?" copy-mode"#) {
            DispatchDecision::Confirm { wrapped, .. } => wrapped,
            other => panic!("expected Confirm, got {other:?}"),
        };
        assert_eq!(confirm, "copy-mode");
        assert!(matches!(
            classify_command(&confirm),
            DispatchDecision::Hint(_)
        ));
    }

    #[test]
    fn test_classify_command_switch_client_unmirrored_table_hints() {
        assert!(matches!(
            classify_command("switch-client -T mytable"),
            DispatchDecision::Hint(_)
        ));
    }

    #[test]
    fn test_classify_command_switch_client_mirrored_table_dispatches() {
        for cmd in [
            "switch-client -T prefix",
            "switch-client -T root",
            "switch-client -n",
        ] {
            assert_eq!(
                classify_command(cmd),
                DispatchDecision::Dispatch(cmd.to_string()),
                "{cmd}"
            );
        }
    }

    // --- confirm-before parser ---

    #[test]
    fn test_parse_confirm_before_extracts_prompt_and_wrapped_command() {
        let confirm =
            parse_confirm_before(r#"confirm-before -p "kill-pane #P? (y/n)" kill-pane"#).unwrap();
        assert_eq!(confirm.prompt, "kill-pane #P? (y/n)");
        assert_eq!(confirm.wrapped, "kill-pane");
    }

    #[test]
    fn test_parse_confirm_before_without_prompt_flag_falls_back() {
        let confirm = parse_confirm_before("confirm-before kill-server").unwrap();
        assert_eq!(confirm.prompt, "confirm?");
        assert_eq!(confirm.wrapped, "kill-server");
    }

    #[test]
    fn test_parse_confirm_before_preserves_wrapped_command_quoting_raw() {
        let confirm = parse_confirm_before(
            r#"confirm-before -p "detach? (y/n)" detach-client -P "some session""#,
        )
        .unwrap();
        assert_eq!(confirm.wrapped, r#"detach-client -P "some session""#);
    }

    #[test]
    fn test_parse_confirm_before_rejects_non_confirm_before_and_missing_command() {
        assert_eq!(parse_confirm_before("kill-pane"), None);
        assert_eq!(parse_confirm_before("confirm-before -p prompt"), None);
        assert_eq!(parse_confirm_before(""), None);
    }

    // --- refresh-trigger detection ---

    #[test]
    fn test_mutates_bindings_true_for_binding_commands() {
        for cmd in [
            "bind-key -T prefix r source-file ~/.tmux.conf",
            "unbind-key -T prefix r",
            "source-file ~/.tmux.conf",
        ] {
            assert!(mutates_bindings(cmd), "{cmd}");
        }
    }

    #[test]
    fn test_mutates_bindings_true_for_prefix_and_repeat_time_options() {
        for cmd in [
            "set-option -g prefix C-a",
            "set -g prefix2 C-b",
            "set-option -g repeat-time 300",
        ] {
            assert!(mutates_bindings(cmd), "{cmd}");
        }
    }

    #[test]
    fn test_mutates_bindings_false_for_unrelated_option_and_other_commands() {
        for cmd in ["set-option -g mouse on", "new-window", "select-pane -L", ""] {
            assert!(!mutates_bindings(cmd), "{cmd}");
        }
    }
}
