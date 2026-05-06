/// Mouse protocol encoding for terminal applications.
///
/// Implements the three standard mouse encoding formats (Normal/X10, UTF-8, SGR)
/// and four reporting modes (X10, Normal/VT200, ButtonEvent, AnyEvent) as defined
/// in the xterm control sequences specification.

// ---------------------------------------------------------------------------
// Button types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Release,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

// ---------------------------------------------------------------------------
// Event kinds (press, release, drag, motion)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseEventKind {
    Press(MouseButton),
    Release(MouseButton),
    Drag(MouseButton),
    Move,
}

// ---------------------------------------------------------------------------
// Encoding mode (how coordinates/buttons are serialized)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    /// X10-style: button + 32, coordinates + 32, max column/row 223.
    #[default]
    Normal,
    /// Like Normal but coordinates are encoded as UTF-8 codepoints, max 2015.
    Utf8,
    /// CSI < button ; col ; row M/m — no coordinate limit.
    Sgr,
}

// ---------------------------------------------------------------------------
// Reporting mode (which events to report)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseReportingMode {
    /// X10 compatibility: button presses only, no modifiers.
    #[default]
    X10,
    /// VT200 / Normal tracking: press + release.
    Normal,
    /// Button-event tracking: press + release + drag with a button held.
    ButtonEvent,
    /// Any-event tracking: press + release + drag + all motion.
    AnyEvent,
}

// ---------------------------------------------------------------------------
// Modifiers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseModifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

// ---------------------------------------------------------------------------
// Grid position (0-based)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MousePosition {
    pub col: usize,
    pub row: usize,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Encode a mouse event into the byte sequence expected by the terminal.
///
/// Returns `None` when the event is not reportable under the given reporting
/// mode, or when the coordinates exceed the encoding limit.
pub fn encode_mouse_event(
    event: MouseEventKind,
    position: MousePosition,
    modifiers: MouseModifiers,
    encoding: MouseEncoding,
    reporting: MouseReportingMode,
) -> Option<Vec<u8>> {
    if !is_reportable(event, reporting) {
        return None;
    }

    // X10 mode never includes modifiers.
    let effective_modifiers = if reporting == MouseReportingMode::X10 {
        MouseModifiers::default()
    } else {
        modifiers
    };

    match encoding {
        MouseEncoding::Normal => encode_normal(event, position, effective_modifiers),
        MouseEncoding::Utf8 => encode_utf8(event, position, effective_modifiers),
        MouseEncoding::Sgr => Some(encode_sgr(event, position, effective_modifiers)),
    }
}

// ---------------------------------------------------------------------------
// Reporting filter
// ---------------------------------------------------------------------------

fn is_reportable(event: MouseEventKind, mode: MouseReportingMode) -> bool {
    match mode {
        MouseReportingMode::X10 => matches!(
            event,
            MouseEventKind::Press(MouseButton::Left)
                | MouseEventKind::Press(MouseButton::Middle)
                | MouseEventKind::Press(MouseButton::Right)
                | MouseEventKind::Press(MouseButton::WheelUp)
                | MouseEventKind::Press(MouseButton::WheelDown)
                | MouseEventKind::Press(MouseButton::WheelLeft)
                | MouseEventKind::Press(MouseButton::WheelRight)
        ),
        MouseReportingMode::Normal => {
            matches!(event, MouseEventKind::Press(_) | MouseEventKind::Release(_))
        }
        MouseReportingMode::ButtonEvent => matches!(
            event,
            MouseEventKind::Press(_) | MouseEventKind::Release(_) | MouseEventKind::Drag(_)
        ),
        MouseReportingMode::AnyEvent => true,
    }
}

// ---------------------------------------------------------------------------
// Button code computation
// ---------------------------------------------------------------------------

fn base_button_code(event: MouseEventKind) -> u8 {
    match event {
        MouseEventKind::Press(button) | MouseEventKind::Release(button) => match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Release => 3,
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
            MouseButton::WheelLeft => 66,
            MouseButton::WheelRight => 67,
        },
        MouseEventKind::Drag(button) => {
            let base = match button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                // Drag with these is unusual but encode defensively.
                MouseButton::Release => 3,
                MouseButton::WheelUp => 64,
                MouseButton::WheelDown => 65,
                MouseButton::WheelLeft => 66,
                MouseButton::WheelRight => 67,
            };
            base + 32
        }
        MouseEventKind::Move => 35, // 3 (no button) + 32 (motion flag)
    }
}

fn modifier_bits(modifiers: MouseModifiers) -> u8 {
    let mut bits = 0u8;
    if modifiers.shift {
        bits |= 4;
    }
    if modifiers.alt {
        bits |= 8;
    }
    if modifiers.ctrl {
        bits |= 16;
    }
    bits
}

// ---------------------------------------------------------------------------
// Normal (X10) encoding
// ---------------------------------------------------------------------------

fn encode_normal(
    event: MouseEventKind,
    position: MousePosition,
    modifiers: MouseModifiers,
) -> Option<Vec<u8>> {
    const MAX_COORD: usize = 223;

    if position.col >= MAX_COORD || position.row >= MAX_COORD {
        return None;
    }

    let button_byte = compute_legacy_button(event, modifiers);
    let col_byte = (position.col as u8) + 32 + 1;
    let row_byte = (position.row as u8) + 32 + 1;

    Some(vec![0x1b, b'[', b'M', 32 + button_byte, col_byte, row_byte])
}

// ---------------------------------------------------------------------------
// UTF-8 encoding
// ---------------------------------------------------------------------------

fn encode_utf8(
    event: MouseEventKind,
    position: MousePosition,
    modifiers: MouseModifiers,
) -> Option<Vec<u8>> {
    const MAX_COORD: usize = 2015;

    if position.col >= MAX_COORD || position.row >= MAX_COORD {
        return None;
    }

    let button_byte = compute_legacy_button(event, modifiers);
    let mut buf = vec![0x1b, b'[', b'M', 32 + button_byte];
    push_utf8_coord(&mut buf, position.col);
    push_utf8_coord(&mut buf, position.row);

    Some(buf)
}

fn push_utf8_coord(buf: &mut Vec<u8>, coord: usize) {
    let value = coord + 32 + 1;
    if value < 128 {
        buf.push(value as u8);
    } else {
        // Encode as a two-byte UTF-8 sequence: 110xxxxx 10xxxxxx
        let first = 0xC0 | (value >> 6) as u8;
        let second = 0x80 | (value & 0x3F) as u8;
        buf.push(first);
        buf.push(second);
    }
}

// ---------------------------------------------------------------------------
// SGR encoding
// ---------------------------------------------------------------------------

fn encode_sgr(
    event: MouseEventKind,
    position: MousePosition,
    modifiers: MouseModifiers,
) -> Vec<u8> {
    let button = base_button_code(event) + modifier_bits(modifiers);
    let suffix = match event {
        MouseEventKind::Release(_) => 'm',
        _ => 'M',
    };
    // SGR uses 1-based coordinates.
    format!(
        "\x1b[<{};{};{}{}",
        button,
        position.col + 1,
        position.row + 1,
        suffix
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// Legacy button byte (shared by Normal and UTF-8)
// ---------------------------------------------------------------------------

/// In legacy encodings, release always uses button code 3 (regardless of which
/// button was released).
fn compute_legacy_button(event: MouseEventKind, modifiers: MouseModifiers) -> u8 {
    let base = match event {
        MouseEventKind::Release(_) => 3,
        _ => base_button_code(event),
    };
    base + modifier_bits(modifiers)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- helpers -----------------------------------------------------------

    fn all_report() -> MouseReportingMode {
        MouseReportingMode::AnyEvent
    }

    fn no_mods() -> MouseModifiers {
        MouseModifiers::default()
    }

    fn pos(col: usize, row: usize) -> MousePosition {
        MousePosition { col, row }
    }

    // =====================================================================
    // Normal encoding
    // =====================================================================

    #[test]
    fn test_normal_left_press_origin() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // button=0+32=32, col=0+32+1=33, row=0+32+1=33
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 32, 33, 33]));
    }

    #[test]
    fn test_normal_right_press() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Right),
            pos(5, 10),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // button=2+32=34, col=5+33=38, row=10+33=43
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 34, 38, 43]));
    }

    #[test]
    fn test_normal_middle_press() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Middle),
            pos(1, 1),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // button=1+32=33, col=1+33=34, row=1+33=34
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 33, 34, 34]));
    }

    #[test]
    fn test_normal_release_uses_button_three() {
        let bytes = encode_mouse_event(
            MouseEventKind::Release(MouseButton::Left),
            pos(5, 8),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // release button=3+32=35, col=5+33=38, row=8+33=41
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 35, 38, 41]));
    }

    #[test]
    fn test_normal_rejects_col_out_of_range() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(223, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        assert!(bytes.is_none());
    }

    #[test]
    fn test_normal_rejects_row_out_of_range() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 300),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        assert!(bytes.is_none());
    }

    #[test]
    fn test_normal_max_valid_coord() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(222, 222),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        assert!(bytes.is_some());
    }

    #[test]
    fn test_normal_wheel_up() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelUp),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // button=64+32=96
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 96, 33, 33]));
    }

    #[test]
    fn test_normal_wheel_down() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelDown),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // button=65+32=97
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 97, 33, 33]));
    }

    // =====================================================================
    // Normal encoding with modifiers
    // =====================================================================

    #[test]
    fn test_normal_shift_modifier() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                shift: true,
                ..no_mods()
            },
            MouseEncoding::Normal,
            all_report(),
        );
        // button=0+4(shift)+32=36
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 36, 33, 33]));
    }

    #[test]
    fn test_normal_alt_modifier() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                alt: true,
                ..no_mods()
            },
            MouseEncoding::Normal,
            all_report(),
        );
        // button=0+8(alt)+32=40
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 40, 33, 33]));
    }

    #[test]
    fn test_normal_ctrl_modifier() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                ctrl: true,
                ..no_mods()
            },
            MouseEncoding::Normal,
            all_report(),
        );
        // button=0+16(ctrl)+32=48
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 48, 33, 33]));
    }

    #[test]
    fn test_normal_all_modifiers() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                shift: true,
                alt: true,
                ctrl: true,
            },
            MouseEncoding::Normal,
            all_report(),
        );
        // button=0+4+8+16+32=60
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 60, 33, 33]));
    }

    // =====================================================================
    // UTF-8 encoding
    // =====================================================================

    #[test]
    fn test_utf8_small_coord_same_as_normal() {
        let normal = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(10, 5),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        let utf8 = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(10, 5),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        );
        assert_eq!(normal, utf8);
    }

    #[test]
    fn test_utf8_extended_coordinates() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(200, 150),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        );
        let bytes = bytes.expect("should encode");
        // Header: ESC [ M button
        assert_eq!(&bytes[..3], &[0x1b, b'[', b'M']);
        // Both coordinates > 95 → each is 2 bytes
        assert_eq!(bytes.len(), 4 + 2 + 2); // header(4) + col(2) + row(2)
    }

    #[test]
    fn test_utf8_rejects_beyond_2015() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(2015, 0),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        );
        assert!(bytes.is_none());
    }

    #[test]
    fn test_utf8_max_valid_coord() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(2014, 2014),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        );
        assert!(bytes.is_some());
    }

    #[test]
    fn test_utf8_boundary_95_uses_two_bytes() {
        // coord 95 → value = 95 + 32 + 1 = 128 → needs 2-byte UTF-8
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(95, 0),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        )
        .expect("should encode");
        // col at 95 should use 2 bytes, row at 0 should use 1 byte
        assert_eq!(bytes.len(), 4 + 2 + 1);
    }

    #[test]
    fn test_utf8_boundary_94_uses_one_byte() {
        // coord 94 → value = 94 + 32 + 1 = 127 → fits in single byte
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(94, 0),
            no_mods(),
            MouseEncoding::Utf8,
            all_report(),
        )
        .expect("should encode");
        assert_eq!(bytes.len(), 4 + 1 + 1);
    }

    // =====================================================================
    // SGR encoding
    // =====================================================================

    #[test]
    fn test_sgr_left_press() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(4, 2),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<0;5;3M".to_vec()));
    }

    #[test]
    fn test_sgr_right_release() {
        let bytes = encode_mouse_event(
            MouseEventKind::Release(MouseButton::Right),
            pos(1, 1),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<2;2;2m".to_vec()));
    }

    #[test]
    fn test_sgr_left_release() {
        let bytes = encode_mouse_event(
            MouseEventKind::Release(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        // SGR release keeps original button code (0 for left), suffix 'm'
        assert_eq!(bytes, Some(b"\x1b[<0;1;1m".to_vec()));
    }

    #[test]
    fn test_sgr_middle_drag_with_modifiers() {
        let bytes = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Middle),
            pos(0, 0),
            MouseModifiers {
                shift: true,
                alt: true,
                ctrl: false,
            },
            MouseEncoding::Sgr,
            all_report(),
        );
        // base: 1 + 32(drag) = 33, mods: 4+8=12, total: 45
        assert_eq!(bytes, Some(b"\x1b[<45;1;1M".to_vec()));
    }

    #[test]
    fn test_sgr_large_coordinates() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(500, 1000),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<0;501;1001M".to_vec()));
    }

    #[test]
    fn test_sgr_wheel_up() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelUp),
            pos(10, 5),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<64;11;6M".to_vec()));
    }

    #[test]
    fn test_sgr_wheel_down() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelDown),
            pos(3, 7),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<65;4;8M".to_vec()));
    }

    #[test]
    fn test_sgr_wheel_left() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelLeft),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<66;1;1M".to_vec()));
    }

    #[test]
    fn test_sgr_wheel_right() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelRight),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        assert_eq!(bytes, Some(b"\x1b[<67;1;1M".to_vec()));
    }

    #[test]
    fn test_sgr_all_modifiers() {
        let bytes = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                shift: true,
                alt: true,
                ctrl: true,
            },
            MouseEncoding::Sgr,
            all_report(),
        );
        // 0 + 4 + 8 + 16 = 28
        assert_eq!(bytes, Some(b"\x1b[<28;1;1M".to_vec()));
    }

    // =====================================================================
    // Reporting mode filtering
    // =====================================================================

    #[test]
    fn test_x10_reports_press_only() {
        // Press is reported
        let press = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert!(press.is_some());

        // Release is NOT reported
        let release = encode_mouse_event(
            MouseEventKind::Release(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert!(release.is_none());

        // Drag is NOT reported
        let drag = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert!(drag.is_none());

        // Move is NOT reported
        let motion = encode_mouse_event(
            MouseEventKind::Move,
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert!(motion.is_none());
    }

    #[test]
    fn test_x10_reports_wheel() {
        let wheel = encode_mouse_event(
            MouseEventKind::Press(MouseButton::WheelUp),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert!(wheel.is_some());
    }

    #[test]
    fn test_x10_ignores_modifiers() {
        let with_mods = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            MouseModifiers {
                shift: true,
                alt: true,
                ctrl: true,
            },
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        let without_mods = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::X10,
        );
        assert_eq!(with_mods, without_mods);
    }

    #[test]
    fn test_normal_mode_reports_press_and_release() {
        let press = encode_mouse_event(
            MouseEventKind::Press(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::Normal,
        );
        assert!(press.is_some());

        let release = encode_mouse_event(
            MouseEventKind::Release(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::Normal,
        );
        assert!(release.is_some());

        let drag = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::Normal,
        );
        assert!(drag.is_none());

        let motion = encode_mouse_event(
            MouseEventKind::Move,
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::Normal,
        );
        assert!(motion.is_none());
    }

    #[test]
    fn test_button_event_mode_includes_drag() {
        let drag = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::ButtonEvent,
        );
        assert!(drag.is_some());

        let motion = encode_mouse_event(
            MouseEventKind::Move,
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::ButtonEvent,
        );
        assert!(motion.is_none());
    }

    #[test]
    fn test_any_event_mode_includes_motion() {
        let motion = encode_mouse_event(
            MouseEventKind::Move,
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            MouseReportingMode::AnyEvent,
        );
        assert!(motion.is_some());
    }

    // =====================================================================
    // Drag encoding
    // =====================================================================

    #[test]
    fn test_sgr_left_drag() {
        let bytes = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            pos(10, 5),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        // base: 0 + 32(drag) = 32
        assert_eq!(bytes, Some(b"\x1b[<32;11;6M".to_vec()));
    }

    #[test]
    fn test_sgr_right_drag() {
        let bytes = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Right),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        // base: 2 + 32(drag) = 34
        assert_eq!(bytes, Some(b"\x1b[<34;1;1M".to_vec()));
    }

    #[test]
    fn test_normal_left_drag() {
        let bytes = encode_mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            pos(0, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // drag button = 0 + 32 = 32, + 32(offset) = 64
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 64, 33, 33]));
    }

    // =====================================================================
    // Motion encoding
    // =====================================================================

    #[test]
    fn test_sgr_move() {
        let bytes = encode_mouse_event(
            MouseEventKind::Move,
            pos(5, 3),
            no_mods(),
            MouseEncoding::Sgr,
            all_report(),
        );
        // button: 35 (3 + 32 motion flag)
        assert_eq!(bytes, Some(b"\x1b[<35;6;4M".to_vec()));
    }

    #[test]
    fn test_normal_move() {
        let bytes = encode_mouse_event(
            MouseEventKind::Move,
            pos(0, 0),
            no_mods(),
            MouseEncoding::Normal,
            all_report(),
        );
        // move button = 35, + 32(offset) = 67
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 67, 33, 33]));
    }
}
