use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

/// Encode clipboard text for the PTY, honoring bracketed-paste mode.
///
/// Line endings (`\r\n` and `\n`) are normalized to `\r` in both modes, per
/// xterm paste behavior — `\r` is what the Enter key produces. With
/// `TermMode::BRACKETED_PASTE` active the payload is wrapped in
/// `ESC[200~ .. ESC[201~`; embedded ESC bytes are stripped so pasted text
/// cannot terminate the bracket early.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        let payload = normalized.replace('\x1b', "");
        let mut bytes = Vec::with_capacity(payload.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend_from_slice(payload.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        normalized.into_bytes()
    }
}

/// Encodes a keystroke into PTY bytes — control/nav/chord sequences AND
/// plain/AltGr-composed printable-character passthrough. This is the
/// original (pre-#501) behavior, unchanged; it stays the entry point for
/// every caller that owns the sole delivery channel for a keystroke's text.
pub fn encode_keystroke(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    encode_keystroke_impl(keystroke, mode, true)
}

/// The control/navigation/chord/escape-sequence subset of [`encode_keystroke`]
/// — never includes plain-printable or AltGr-composed-character passthrough.
///
/// Once a pane has a GPUI `EntityInputHandler` registered (#501), printable
/// text arrives a second time via the platform's own text-commit channel.
/// On Windows this is unconditional and synchronous: every character-
/// producing key fires both `WM_KEYDOWN` (`on_key_down`) and, immediately
/// after, `WM_CHAR` (`replace_text_in_range`) — so forwarding printable
/// bytes from `on_key_down` there double-inserts every typed character (the
/// PR #785 review's blocking defect). Nav/ctrl/basic keys are unaffected —
/// `WM_CHAR` never fires for those — so this still encodes them exactly like
/// [`encode_keystroke`].
///
/// `on_key_down`'s call site picks between the two based on target platform
/// (see `pane_view.rs`); this only needs to be strictly narrower than
/// [`encode_keystroke`] (never emit bytes it wouldn't), so a caller that
/// picks it unconditionally is always safe, just possibly over-cautious on
/// platforms where the second channel does not exist.
pub fn encode_control_keystroke(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    encode_keystroke_impl(keystroke, mode, false)
}

/// `on_key_down`'s actual entry point: [`encode_control_keystroke`] on
/// Windows (WM_CHAR independently and unconditionally delivers printable/
/// AltGr text there — see that function's doc), [`encode_keystroke`]
/// (unchanged, current behavior) everywhere else.
///
/// Linux/X11 is deliberately left on the full, printable-passthrough-
/// including path: unlike Windows' unconditional dual dispatch, a plain
/// key's delivery there is not confirmed to also reach the pane's
/// `EntityInputHandler` in the common (no active IME composition) case —
/// switching it to `encode_control_keystroke` risks under-delivery (typing
/// nothing) rather than the over-delivery bug this fixes on Windows.
pub fn encode_keystroke_for_key_down(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    if cfg!(target_os = "windows") {
        encode_control_keystroke(keystroke, mode)
    } else {
        encode_keystroke(keystroke, mode)
    }
}

fn encode_keystroke_impl(
    keystroke: &Keystroke,
    mode: TermMode,
    allow_printable_passthrough: bool,
) -> Option<Vec<u8>> {
    let key = keystroke.key.as_str();
    let ctrl = keystroke.modifiers.control;
    let alt = keystroke.modifiers.alt;
    let shift = keystroke.modifiers.shift;

    // Ignore bare modifier keys
    if matches!(key, "control" | "alt" | "shift" | "platform" | "function") {
        return None;
    }

    // AltGr on Windows/Linux is reported as Ctrl+Alt. When a key_char is present,
    // it's a composed character (e.g. AltGr+ß → \) — pass it through directly,
    // or (mirroring the plain-printable branch below) leave it to the
    // platform's separate text-commit channel when that channel is the sole
    // sender. Either way this returns unconditionally once `key_char` is
    // present: falling through to `encode_ctrl` below would treat the key's
    // UNMODIFIED base key (`keystroke.key`, e.g. "8" for German AltGr+8 → "[")
    // as a plain control combo, emitting a bogus control byte alongside the
    // character WM_CHAR already delivers (#785 review, B2).
    if ctrl && alt {
        if let Some(ch) = &keystroke.key_char {
            if !ch.is_empty() {
                return if allow_printable_passthrough {
                    Some(ch.as_bytes().to_vec())
                } else {
                    None
                };
            }
        }
    }

    // Nav/cursor/function keys encode all modifiers (including alt) internally
    if let Some(bytes) = encode_nav_key(key, ctrl, alt, shift, mode) {
        return Some(bytes);
    }

    // Ctrl+key (checked before basic keys so Ctrl+Space produces NUL, not 0x20)
    if ctrl {
        if let Some(code) = encode_ctrl(key) {
            let bytes = vec![code];
            return if alt {
                let mut prefixed = Vec::with_capacity(1 + bytes.len());
                prefixed.push(0x1b);
                prefixed.extend_from_slice(&bytes);
                Some(prefixed)
            } else {
                Some(bytes)
            };
        }
    }

    // Basic special keys (enter, tab, escape, backspace, space)
    if let Some(bytes) = encode_basic_key(key, ctrl, shift) {
        return if alt {
            let mut prefixed = Vec::with_capacity(1 + bytes.len());
            prefixed.push(0x1b);
            prefixed.extend_from_slice(&bytes);
            Some(prefixed)
        } else {
            Some(bytes)
        };
    }

    // Printable character passthrough
    if allow_printable_passthrough {
        if let Some(ch) = &keystroke.key_char {
            if !ch.is_empty() {
                let bytes = ch.as_bytes().to_vec();
                return if alt {
                    let mut prefixed = Vec::with_capacity(1 + bytes.len());
                    prefixed.push(0x1b);
                    prefixed.extend_from_slice(&bytes);
                    Some(prefixed)
                } else {
                    Some(bytes)
                };
            }
        }
    }

    None
}

fn encode_basic_key(key: &str, ctrl: bool, shift: bool) -> Option<Vec<u8>> {
    match key {
        "enter" if shift => Some(vec![b'\n']),
        "enter" => Some(vec![b'\r']),
        "tab" if shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(vec![b'\t']),
        "escape" => Some(vec![0x1b]),
        // Ctrl+Backspace deletes the previous word. ESC+DEL is readline's
        // backward-kill-word, sharing the word boundary of Ctrl+Left/Right
        // (`\x1b[1;5D`); it is also the sequence Alt+Backspace already emits.
        "backspace" if ctrl => Some(vec![0x1b, 0x7f]),
        "backspace" => Some(vec![0x7f]),
        // Space is intentionally NOT handled here: it must flow through the
        // `allow_printable_passthrough`-gated printable branch below (its
        // `key_char` is `" "`) like every other printable, so it is deduped
        // against the platform text-commit channel the same way. Handling it
        // here bypassed that gate and doubled every space keystroke (#791).
        // Ctrl+Space is unaffected — it is resolved earlier by `encode_ctrl`.
        _ => None,
    }
}

fn encode_nav_key(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    mode: TermMode,
) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let has_modifiers = ctrl || alt || shift;

    // xterm modifier parameter: 1 + bitmask (shift=1, alt=2, ctrl=4)
    let mod_param = if has_modifiers {
        1 + (shift as u8) + ((alt as u8) << 1) + ((ctrl as u8) << 2)
    } else {
        0
    };

    match key {
        // Cursor keys
        "up" => Some(cursor_key(b'A', app_cursor, mod_param)),
        "down" => Some(cursor_key(b'B', app_cursor, mod_param)),
        "right" => Some(cursor_key(b'C', app_cursor, mod_param)),
        "left" => Some(cursor_key(b'D', app_cursor, mod_param)),
        "home" => Some(cursor_key(b'H', app_cursor, mod_param)),
        "end" => Some(cursor_key(b'F', app_cursor, mod_param)),

        // Tilde-format keys
        "pageup" => Some(tilde_key(5, mod_param)),
        "pagedown" => Some(tilde_key(6, mod_param)),
        "insert" => Some(tilde_key(2, mod_param)),
        "delete" => Some(tilde_key(3, mod_param)),

        // Function keys: F1-F4 use SS3 unmodified, CSI with modifiers
        "f1" => Some(ss3_or_csi_fkey(b'P', mod_param)),
        "f2" => Some(ss3_or_csi_fkey(b'Q', mod_param)),
        "f3" => Some(ss3_or_csi_fkey(b'R', mod_param)),
        "f4" => Some(ss3_or_csi_fkey(b'S', mod_param)),
        // F5-F12 always use tilde format
        "f5" => Some(tilde_key(15, mod_param)),
        "f6" => Some(tilde_key(17, mod_param)),
        "f7" => Some(tilde_key(18, mod_param)),
        "f8" => Some(tilde_key(19, mod_param)),
        "f9" => Some(tilde_key(20, mod_param)),
        "f10" => Some(tilde_key(21, mod_param)),
        "f11" => Some(tilde_key(23, mod_param)),
        "f12" => Some(tilde_key(24, mod_param)),
        _ => None,
    }
}

/// Cursor keys (arrows, home, end): SS3 when app_cursor and no modifiers, CSI otherwise.
/// With modifiers: `\x1b[1;{mod}{suffix}`
fn cursor_key(suffix: u8, app_cursor: bool, mod_param: u8) -> Vec<u8> {
    if mod_param == 0 {
        if app_cursor {
            vec![0x1b, b'O', suffix]
        } else {
            vec![0x1b, b'[', suffix]
        }
    } else {
        format!("\x1b[1;{}{}", mod_param, suffix as char).into_bytes()
    }
}

/// Tilde-format keys: `\x1b[{code}~` or `\x1b[{code};{mod}~`
fn tilde_key(code: u8, mod_param: u8) -> Vec<u8> {
    if mod_param == 0 {
        format!("\x1b[{}~", code).into_bytes()
    } else {
        format!("\x1b[{};{}~", code, mod_param).into_bytes()
    }
}

/// F1-F4 use SS3 format unmodified (`\x1bO{suffix}`), CSI with modifier (`\x1b[1;{mod}{suffix}`).
fn ss3_or_csi_fkey(suffix: u8, mod_param: u8) -> Vec<u8> {
    if mod_param == 0 {
        vec![0x1b, b'O', suffix]
    } else {
        format!("\x1b[1;{}{}", mod_param, suffix as char).into_bytes()
    }
}

fn encode_ctrl(key: &str) -> Option<u8> {
    // Handle multi-char key names
    if key == "space" {
        return Some(0x00);
    }

    let bytes = key.as_bytes();
    if bytes.len() == 1 {
        let b = bytes[0];
        if b.is_ascii_lowercase() {
            return Some(b - b'a' + 1);
        }
        if b.is_ascii_uppercase() {
            return Some(b - b'A' + 1);
        }
        return match b {
            b'[' => Some(0x1b),
            b']' => Some(0x1d),
            b'\\' => Some(0x1c),
            b'@' => Some(0x00),
            b'^' => Some(0x1e),
            b'_' => Some(0x1f),
            // Ctrl+digit mappings
            b'2' => Some(0x00),
            b'3' => Some(0x1b),
            b'4' => Some(0x1c),
            b'5' => Some(0x1d),
            b'6' => Some(0x1e),
            b'7' => Some(0x1f),
            b'8' => Some(0x7f),
            _ => None,
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    fn key(name: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers::none(),
            key: name.into(),
            key_char: None,
        }
    }

    fn key_with_char(name: &str, ch: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers::none(),
            key: name.into(),
            key_char: Some(ch.into()),
        }
    }

    fn ctrl_key(name: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                control: true,
                ..Modifiers::none()
            },
            key: name.into(),
            key_char: None,
        }
    }

    fn alt_key(name: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::none()
            },
            key: name.into(),
            key_char: None,
        }
    }

    fn alt_key_with_char(name: &str, ch: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                alt: true,
                ..Modifiers::none()
            },
            key: name.into(),
            key_char: Some(ch.into()),
        }
    }

    fn shift_key(name: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::none()
            },
            key: name.into(),
            key_char: None,
        }
    }

    fn ctrl_shift_key(name: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                control: true,
                shift: true,
                ..Modifiers::none()
            },
            key: name.into(),
            key_char: None,
        }
    }

    fn normal() -> TermMode {
        TermMode::empty()
    }

    fn app_cursor() -> TermMode {
        TermMode::APP_CURSOR
    }

    // --- Basic key tests ---

    #[test]
    fn test_encode_printable_passthrough() {
        let ks = key_with_char("a", "a");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x61]));
    }

    #[test]
    fn test_encode_space_emits_single_space_via_printable_passthrough() {
        // Regression (#791): space used to be special-cased in
        // `encode_basic_key`, bypassing the `allow_printable_passthrough`
        // gate that dedups every other printable against the platform's
        // text-commit channel and doubling the keystroke. It must now flow
        // through the same printable branch as any other character and
        // produce exactly one 0x20 byte.
        let ks = key_with_char("space", " ");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x20]));
    }

    #[test]
    fn test_encode_control_keystroke_space_returns_none() {
        // Mirrors `test_encode_control_keystroke_plain_printable_returns_none`:
        // on the on_key_down path that coexists with a platform text-commit
        // channel (Windows), space must be suppressed just like any other
        // printable so it is not delivered twice.
        let ks = key_with_char("space", " ");
        assert_eq!(encode_control_keystroke(&ks, normal()), None);
    }

    #[test]
    fn test_encode_ctrl_c() {
        let ks = ctrl_key("c");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x03]));
    }

    #[test]
    fn test_encode_ctrl_a_through_z() {
        for (i, letter) in (b'a'..=b'z').enumerate() {
            let name = String::from(letter as char);
            let ks = ctrl_key(&name);
            let expected = (i as u8) + 1;
            assert_eq!(
                encode_keystroke(&ks, normal()),
                Some(vec![expected]),
                "Ctrl+{name} should produce 0x{expected:02x}"
            );
        }
    }

    #[test]
    fn test_encode_alt_a() {
        let ks = alt_key_with_char("a", "a");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x1b, 0x61]));
    }

    #[test]
    fn test_encode_enter() {
        let ks = key("enter");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![b'\r']));
    }

    #[test]
    fn test_encode_shift_enter() {
        let ks = shift_key("enter");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![b'\n']));
    }

    #[test]
    fn test_encode_shift_tab() {
        let ks = shift_key("tab");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn test_encode_backspace() {
        let ks = key("backspace");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x7f]));
    }

    #[test]
    fn test_encode_ctrl_backspace() {
        // Ctrl+Backspace deletes the previous word: ESC+DEL (backward-kill-word).
        let ks = ctrl_key("backspace");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x1b, 0x7f]));
    }

    // --- Arrow keys ---

    #[test]
    fn test_encode_arrow_normal_mode() {
        let cases = [
            ("up", b"\x1b[A".as_slice()),
            ("down", b"\x1b[B"),
            ("left", b"\x1b[D"),
            ("right", b"\x1b[C"),
        ];
        for (name, expected) in cases {
            let ks = key(name);
            assert_eq!(
                encode_keystroke(&ks, normal()),
                Some(expected.to_vec()),
                "arrow {name} in normal mode"
            );
        }
    }

    #[test]
    fn test_encode_arrow_app_cursor_mode() {
        let cases = [
            ("up", b"\x1bOA".as_slice()),
            ("down", b"\x1bOB"),
            ("left", b"\x1bOD"),
            ("right", b"\x1bOC"),
        ];
        for (name, expected) in cases {
            let ks = key(name);
            assert_eq!(
                encode_keystroke(&ks, app_cursor()),
                Some(expected.to_vec()),
                "arrow {name} in app cursor mode"
            );
        }
    }

    // --- Function keys ---

    #[test]
    fn test_encode_function_keys() {
        let cases: &[(&str, &[u8])] = &[
            ("f1", b"\x1bOP"),
            ("f2", b"\x1bOQ"),
            ("f3", b"\x1bOR"),
            ("f4", b"\x1bOS"),
            ("f5", b"\x1b[15~"),
            ("f6", b"\x1b[17~"),
            ("f7", b"\x1b[18~"),
            ("f8", b"\x1b[19~"),
            ("f9", b"\x1b[20~"),
            ("f10", b"\x1b[21~"),
            ("f11", b"\x1b[23~"),
            ("f12", b"\x1b[24~"),
        ];
        for (name, expected) in cases {
            let ks = key(name);
            assert_eq!(
                encode_keystroke(&ks, normal()),
                Some(expected.to_vec()),
                "function key {name}"
            );
        }
    }

    // --- C1: Home/End APP_CURSOR ---

    #[test]
    fn test_encode_home_end_normal_mode() {
        assert_eq!(
            encode_keystroke(&key("home"), normal()),
            Some(b"\x1b[H".to_vec())
        );
        assert_eq!(
            encode_keystroke(&key("end"), normal()),
            Some(b"\x1b[F".to_vec())
        );
    }

    #[test]
    fn test_encode_home_end_app_cursor_mode() {
        assert_eq!(
            encode_keystroke(&key("home"), app_cursor()),
            Some(b"\x1bOH".to_vec())
        );
        assert_eq!(
            encode_keystroke(&key("end"), app_cursor()),
            Some(b"\x1bOF".to_vec())
        );
    }

    // --- C2: Modified arrows ---

    #[test]
    fn test_encode_shift_up() {
        // shift=1, mod_param = 1+1 = 2
        let ks = shift_key("up");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;2A".to_vec()));
    }

    #[test]
    fn test_encode_ctrl_right() {
        // ctrl=4, mod_param = 1+4 = 5
        let ks = ctrl_key("right");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;5C".to_vec()));
    }

    #[test]
    fn test_encode_alt_down() {
        // alt=2, mod_param = 1+2 = 3
        let ks = alt_key("down");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;3B".to_vec()));
    }

    #[test]
    fn test_encode_ctrl_shift_left() {
        // shift=1, ctrl=4, mod_param = 1+1+4 = 6
        let ks = ctrl_shift_key("left");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;6D".to_vec()));
    }

    #[test]
    fn test_encode_modified_arrow_ignores_app_cursor() {
        // Even in APP_CURSOR mode, modified arrows use CSI format
        let ks = ctrl_key("up");
        assert_eq!(
            encode_keystroke(&ks, app_cursor()),
            Some(b"\x1b[1;5A".to_vec())
        );
    }

    // --- C3: Ctrl+Space ---

    #[test]
    fn test_encode_ctrl_space() {
        let ks = ctrl_key("space");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x00]));
    }

    // --- C4: Ctrl+digit ---

    #[test]
    fn test_encode_ctrl_digits() {
        let cases = [
            ("2", 0x00u8),
            ("3", 0x1b),
            ("4", 0x1c),
            ("5", 0x1d),
            ("6", 0x1e),
            ("7", 0x1f),
            ("8", 0x7f),
        ];
        for (digit, expected) in cases {
            let ks = ctrl_key(digit);
            assert_eq!(
                encode_keystroke(&ks, normal()),
                Some(vec![expected]),
                "Ctrl+{digit} should produce 0x{expected:02x}"
            );
        }
    }

    // --- C5: Ctrl+uppercase ---

    #[test]
    fn test_encode_ctrl_uppercase() {
        let ks = Keystroke {
            modifiers: Modifiers {
                control: true,
                ..Modifiers::none()
            },
            key: "C".into(),
            key_char: None,
        };
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x03]));
    }

    // --- C6: Modified function keys ---

    #[test]
    fn test_encode_shift_f3() {
        // shift=1, mod_param = 2, F3 suffix = 'R'
        let ks = shift_key("f3");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;2R".to_vec()));
    }

    #[test]
    fn test_encode_ctrl_f5() {
        // ctrl=4, mod_param = 5, F5 code = 15
        let ks = ctrl_key("f5");
        assert_eq!(
            encode_keystroke(&ks, normal()),
            Some(b"\x1b[15;5~".to_vec())
        );
    }

    #[test]
    fn test_encode_shift_delete() {
        // shift=1, mod_param = 2, delete code = 3
        let ks = shift_key("delete");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[3;2~".to_vec()));
    }

    #[test]
    fn test_encode_modified_home() {
        // ctrl+home: mod_param = 5
        let ks = ctrl_key("home");
        assert_eq!(encode_keystroke(&ks, normal()), Some(b"\x1b[1;5H".to_vec()));
    }

    // --- AltGr (Ctrl+Alt) produces composed character ---

    #[test]
    fn test_encode_altgr_passthrough() {
        let ks = Keystroke {
            modifiers: Modifiers {
                control: true,
                alt: true,
                ..Modifiers::none()
            },
            key: "ß".into(),
            key_char: Some("\\".into()),
        };
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![b'\\']));
    }

    // --- encode_control_keystroke: printable passthrough excluded (#501/#785) ---

    #[test]
    fn test_encode_control_keystroke_plain_printable_returns_none() {
        // The regression from PR #785's review: on Windows this same "a"
        // also arrives via WM_CHAR -> replace_text_in_range, so on_key_down
        // must not independently forward it too.
        let ks = key_with_char("a", "a");
        assert_eq!(encode_control_keystroke(&ks, normal()), None);
    }

    #[test]
    fn test_encode_control_keystroke_altgr_composed_char_returns_none() {
        // AltGr-composed characters also arrive via WM_CHAR on Windows —
        // excluded for the same reason as plain printables. "ß" alone would
        // pass vacuously (its base key is 2-byte UTF-8, so `encode_ctrl`
        // already returns `None` for it and never reaches the fallthrough
        // bug); this must stay `None` too.
        let ks = Keystroke {
            modifiers: Modifiers {
                control: true,
                alt: true,
                ..Modifiers::none()
            },
            key: "ß".into(),
            key_char: Some("\\".into()),
        };
        assert_eq!(encode_control_keystroke(&ks, normal()), None);
    }

    #[test]
    fn test_encode_control_keystroke_altgr_base_key_in_encode_ctrl_returns_none_not_control_byte() {
        // Regression (#785 review, B2): German keyboard AltGr+8 -> "[".
        // GPUI reports `key` as the UNMODIFIED base key ("8"), which maps in
        // `encode_ctrl` (Ctrl+8 -> 0x7f). Before the fix, the AltGr branch
        // being skipped on the control-only path let this fall through to
        // the generic `ctrl` handling, emitting a bogus `ESC 0x7f`
        // (backward-kill-word) alongside the "[" WM_CHAR delivers.
        let altgr_8 = Keystroke {
            modifiers: Modifiers {
                control: true,
                alt: true,
                ..Modifiers::none()
            },
            key: "8".into(),
            key_char: Some("[".into()),
        };
        assert_eq!(encode_control_keystroke(&altgr_8, normal()), None);

        // AltGr+Q -> "@": base key "q" maps in `encode_ctrl` (Ctrl+Q -> 0x11).
        let altgr_q = Keystroke {
            modifiers: Modifiers {
                control: true,
                alt: true,
                ..Modifiers::none()
            },
            key: "q".into(),
            key_char: Some("@".into()),
        };
        assert_eq!(encode_control_keystroke(&altgr_q, normal()), None);
    }

    #[test]
    fn test_encode_keystroke_altgr_base_key_in_encode_ctrl_still_passes_through() {
        // `encode_keystroke` (the unchanged, non-Windows-on_key_down path)
        // must keep composing AltGr+8 into "[", not a control byte — the
        // invariant `encode_control_keystroke` stays narrower than, this
        // does not change.
        let altgr_8 = Keystroke {
            modifiers: Modifiers {
                control: true,
                alt: true,
                ..Modifiers::none()
            },
            key: "8".into(),
            key_char: Some("[".into()),
        };
        assert_eq!(encode_keystroke(&altgr_8, normal()), Some(b"[".to_vec()));
    }

    #[test]
    fn test_encode_control_keystroke_ctrl_combo_still_encodes() {
        // Ctrl+C never triggers WM_CHAR on Windows (ToUnicode yields no
        // character for it), so control sequences are unaffected.
        let ks = ctrl_key("c");
        assert_eq!(encode_control_keystroke(&ks, normal()), Some(vec![0x03]));
    }

    #[test]
    fn test_encode_control_keystroke_nav_key_still_encodes() {
        let ks = key("up");
        assert_eq!(
            encode_control_keystroke(&ks, normal()),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn test_encode_control_keystroke_basic_key_still_encodes() {
        let ks = key("enter");
        assert_eq!(encode_control_keystroke(&ks, normal()), Some(vec![b'\r']));
    }

    // --- Alt on basic keys gets ESC prefix ---

    #[test]
    fn test_encode_alt_enter() {
        let ks = alt_key("enter");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x1b, b'\r']));
    }

    #[test]
    fn test_encode_alt_backspace() {
        let ks = alt_key("backspace");
        assert_eq!(encode_keystroke(&ks, normal()), Some(vec![0x1b, 0x7f]));
    }

    // --- Paste encoding ---

    fn bracketed_paste() -> TermMode {
        TermMode::BRACKETED_PASTE
    }

    #[test]
    fn test_encode_paste_bracketed_multiline_wrapped_and_normalized() {
        assert_eq!(
            encode_paste("echo a\r\necho b\necho c", bracketed_paste()),
            b"\x1b[200~echo a\recho b\recho c\x1b[201~".to_vec()
        );
    }

    #[test]
    fn test_encode_paste_plain_multiline_normalized_without_envelope() {
        assert_eq!(
            encode_paste("echo a\r\necho b\necho c", normal()),
            b"echo a\recho b\recho c".to_vec()
        );
    }

    #[test]
    fn test_encode_paste_bracketed_embedded_escape_stripped() {
        // A payload containing ESC[201~ must not terminate the bracket early.
        assert_eq!(
            encode_paste("a\x1b[201~b", bracketed_paste()),
            b"\x1b[200~a[201~b\x1b[201~".to_vec()
        );
    }

    #[test]
    fn test_encode_paste_plain_empty_passthrough() {
        assert_eq!(encode_paste("", normal()), Vec::<u8>::new());
    }
}
