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

    let bytes = encode_inner(key, ctrl, shift, mode, &keystroke.key_char)?;

    if alt && key != "alt" {
        let mut prefixed = Vec::with_capacity(1 + bytes.len());
        prefixed.push(0x1b);
        prefixed.extend_from_slice(&bytes);
        Some(prefixed)
    } else {
        Some(bytes)
    }
}

fn encode_inner(
    key: &str,
    ctrl: bool,
    shift: bool,
    mode: TermMode,
    key_char: &Option<String>,
) -> Option<Vec<u8>> {
    // 1. Ctrl+letter (a-z)
    if ctrl {
        if let Some(code) = encode_ctrl(key) {
            return Some(vec![code]);
        }
    }

    // 2. Special keys
    if let Some(bytes) = encode_special(key, shift, mode) {
        return Some(bytes);
    }

    // 3. Printable character passthrough
    if let Some(ch) = key_char {
        if !ch.is_empty() {
            return Some(ch.as_bytes().to_vec());
        }
    }

    None
}

fn encode_ctrl(key: &str) -> Option<u8> {
    let bytes = key.as_bytes();
    if bytes.len() == 1 {
        let b = bytes[0];
        if b.is_ascii_lowercase() {
            return Some(b - b'a' + 1);
        }
        return match b {
            b'[' => Some(0x1b),
            b']' => Some(0x1d),
            b'\\' => Some(0x1c),
            b'@' => Some(0x00),
            b'^' => Some(0x1e),
            b'_' => Some(0x1f),
            _ => None,
        };
    }
    None
}

fn encode_special(key: &str, shift: bool, mode: TermMode) -> Option<Vec<u8>> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    match key {
        "enter" if shift => Some(vec![b'\n']),
        "enter" => Some(vec![b'\r']),
        "tab" if shift => Some(b"\x1b[Z".to_vec()),
        "tab" => Some(vec![b'\t']),
        "escape" => Some(vec![0x1b]),
        "backspace" => Some(vec![0x7f]),
        "space" => Some(vec![0x20]),
        "up" => Some(arrow(b'A', app_cursor)),
        "down" => Some(arrow(b'B', app_cursor)),
        "right" => Some(arrow(b'C', app_cursor)),
        "left" => Some(arrow(b'D', app_cursor)),
        "home" => Some(b"\x1b[H".to_vec()),
        "end" => Some(b"\x1b[F".to_vec()),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "insert" => Some(b"\x1b[2~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),
        _ => None,
    }
}

fn arrow(suffix: u8, app_cursor: bool) -> Vec<u8> {
    if app_cursor {
        vec![0x1b, b'O', suffix]
    } else {
        vec![0x1b, b'[', suffix]
    }
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

    fn normal() -> TermMode {
        TermMode::empty()
    }

    fn app_cursor() -> TermMode {
        TermMode::APP_CURSOR
    }

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
    fn test_encode_arrow_normal_mode() {
        let cases = [
            ("up", b"\x1b[A"),
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
            ("up", b"\x1bOA"),
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
}
