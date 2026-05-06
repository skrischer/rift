use alacritty_terminal::term::TermMode;
use gpui::Keystroke;

pub fn encode_keystroke(keystroke: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    let key = keystroke.key.as_str();
    let ctrl = keystroke.modifiers.control;
    let alt = keystroke.modifiers.alt;
    let shift = keystroke.modifiers.shift;

    // Ignore bare modifier keys
    if matches!(key, "control" | "alt" | "shift" | "platform" | "function") {
        return None;
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
    if let Some(bytes) = encode_basic_key(key, shift) {
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

    None
}

fn encode_basic_key(key: &str, shift: bool) -> Option<Vec<u8>> {
    match key {
        "enter" if shift => Some(vec![b'\n']),
        "enter" => Some(vec![b'\r']),
        "tab" if shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(vec![b'\t']),
        "escape" => Some(vec![0x1b]),
        "backspace" => Some(vec![0x7f]),
        "space" => Some(vec![0x20]),
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
}
