//! Stateless line parsing for the control-mode notification set.
//!
//! The `%begin`/`%end`/`%error` guards span multiple lines and are tracked by
//! [`Client`](crate::Client) as command blocks; everything else is a
//! self-contained line parsed here.

use crate::event::Event;
use crate::vis::decode_octal;

/// Parse the three-field guard tail `<epoch> <number> <flags>` shared by
/// `%begin`/`%end`/`%error`. Returns `(number, flags)`; the epoch is ignored.
/// `flags` is non-zero for blocks this control client requested (the only
/// signal that distinguishes a reply to our command from tmux's own internal
/// blocks — verified empirically against tmux 3.4 and matching `control.c`).
pub(crate) fn parse_guard(rest: &[u8]) -> Option<(u64, u64)> {
    let text = std::str::from_utf8(rest).ok()?;
    let mut fields = text.split(' ');
    let _epoch = fields.next()?;
    let number = fields.next()?.parse().ok()?;
    let flags = fields.next()?.parse().ok()?;
    Some((number, flags))
}

/// Parse a single top-level `%`-notification line (everything except the
/// command guards, which [`Client`](crate::Client) tracks as blocks). Returns
/// `None` for a malformed known notification or a non-`%` line; an unmodeled
/// `%`-notification becomes [`Event::Other`].
pub(crate) fn parse_notification(line: &[u8]) -> Option<Event> {
    let (head, rest) = split_first_space(line);
    match head {
        b"%output" => parse_output(rest),
        // tmux switches to `%extended-output %<pane> <age> : <text>` once flow
        // control (`pause-after`) is active on the client. Same pane bytes, just
        // annotated with output age (ms) — decode to the same [`Event::Output`],
        // dropping the age, so the byte stream is identical to the unpaused path.
        b"%extended-output" => parse_extended_output(rest),
        b"%layout-change" => parse_layout_change(rest),
        b"%window-add" => parse_window_id(rest).map(|window| Event::WindowAdd { window }),
        b"%window-close" | b"%unlinked-window-close" => {
            parse_window_id(rest).map(|window| Event::WindowClose { window })
        }
        b"%session-changed" => parse_session_changed(rest),
        b"%pane-mode-changed" => parse_pane_id(rest).map(|pane| Event::PaneModeChanged { pane }),
        b"%exit" => Some(Event::Exit {
            reason: (!rest.is_empty()).then(|| String::from_utf8_lossy(rest).into_owned()),
        }),
        _ if head.starts_with(b"%") => Some(Event::Other {
            name: String::from_utf8_lossy(head).into_owned(),
            args: String::from_utf8_lossy(rest).into_owned(),
        }),
        _ => None,
    }
}

/// `%<pane> <age> : <payload>` (flow-control form) — strip the pane id and the
/// age token, then decode the payload after the ` : ` separator. The payload may
/// contain ` : ` itself, so only the first separator is consumed.
fn parse_extended_output(rest: &[u8]) -> Option<Event> {
    let after_pct = rest.strip_prefix(b"%")?;
    let sp1 = after_pct.iter().position(|&b| b == b' ')?;
    let (id, after_pane) = after_pct.split_at(sp1);
    let pane = parse_u32(id)?;
    // after_pane = " <age> : <payload>"; skip the leading space, then the age.
    let after_age = &after_pane[1..];
    let sp2 = after_age.iter().position(|&b| b == b' ')?;
    let payload = after_age[sp2 + 1..].strip_prefix(b": ")?;
    Some(Event::Output {
        pane,
        data: decode_octal(payload),
    })
}

/// `%<pane> <payload>` — the space after the pane id is mandatory (an empty
/// payload still has it); the payload bytes are raw and may contain spaces.
fn parse_output(rest: &[u8]) -> Option<Event> {
    let after_pct = rest.strip_prefix(b"%")?;
    let sp = after_pct.iter().position(|&b| b == b' ')?;
    let (id, payload) = after_pct.split_at(sp);
    let pane = parse_u32(id)?;
    Some(Event::Output {
        pane,
        data: decode_octal(&payload[1..]),
    })
}

/// `@<window> <layout> [<visible_layout>] [<flags>]`. The layout strings hold
/// no spaces (commas/braces only), so whitespace splitting is unambiguous.
fn parse_layout_change(rest: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(rest).ok()?;
    let mut fields = text.split(' ');
    let window = fields.next()?.strip_prefix('@')?.parse().ok()?;
    let layout = fields.next()?.to_owned();
    let visible_layout = fields.next().map(str::to_owned);
    let flags = fields.next().map(str::to_owned);
    Some(Event::LayoutChange {
        window,
        layout,
        visible_layout,
        flags,
    })
}

/// `$<session> <name>` — the name is the remainder and may contain spaces.
fn parse_session_changed(rest: &[u8]) -> Option<Event> {
    let (id, name) = split_first_space(rest);
    let session = parse_u32(id.strip_prefix(b"$")?)?;
    Some(Event::SessionChanged {
        session,
        name: String::from_utf8_lossy(name).into_owned(),
    })
}

fn parse_window_id(rest: &[u8]) -> Option<u32> {
    parse_u32(rest.strip_prefix(b"@")?)
}

fn parse_pane_id(rest: &[u8]) -> Option<u32> {
    parse_u32(rest.strip_prefix(b"%")?)
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Split a line into the head (up to the first space) and the remainder (after
/// it). With no space the head is the whole line and the remainder is empty.
fn split_first_space(line: &[u8]) -> (&[u8], &[u8]) {
    match line.iter().position(|&b| b == b' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, &[]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_guard_extracts_number_and_flags() {
        assert_eq!(parse_guard(b"1781354147 265 0"), Some((265, 0)));
        assert_eq!(parse_guard(b"1781354147 271 1"), Some((271, 1)));
    }

    #[test]
    fn test_parse_guard_malformed_returns_none() {
        assert_eq!(parse_guard(b""), None);
        assert_eq!(parse_guard(b"1781354147"), None); // no number/flags
        assert_eq!(parse_guard(b"1781354147 abc 0"), None); // non-numeric number
    }

    #[test]
    fn test_parse_notification_output_decodes_payload() {
        assert_eq!(
            parse_notification(b"%output %3 ls\\015\\012"),
            Some(Event::Output {
                pane: 3,
                data: b"ls\r\n".to_vec(),
            })
        );
    }

    #[test]
    fn test_parse_notification_output_empty_payload_is_valid() {
        assert_eq!(
            parse_notification(b"%output %12 "),
            Some(Event::Output {
                pane: 12,
                data: Vec::new(),
            })
        );
    }

    #[test]
    fn test_parse_notification_extended_output_decodes_payload_dropping_age() {
        // Flow-control form (real tmux 3.4): "%extended-output %<pane> <age> :
        // <text>". The age is dropped; the text decodes like %output.
        assert_eq!(
            parse_notification(b"%extended-output %0 0 : ls\\015\\012"),
            Some(Event::Output {
                pane: 0,
                data: b"ls\r\n".to_vec(),
            })
        );
        // A payload containing " : " keeps everything after the first separator.
        assert_eq!(
            parse_notification(b"%extended-output %2 17 : a : b"),
            Some(Event::Output {
                pane: 2,
                data: b"a : b".to_vec(),
            })
        );
    }

    #[test]
    fn test_parse_notification_extended_output_malformed_returns_none() {
        assert_eq!(parse_notification(b"%extended-output 0 0 : hi"), None); // pane no %
        assert_eq!(parse_notification(b"%extended-output %0 0 hi"), None); // no ": " separator
        assert_eq!(parse_notification(b"%extended-output %0"), None); // truncated
    }

    #[test]
    fn test_parse_notification_output_malformed_returns_none() {
        // Missing percent, missing payload separator, non-numeric pane.
        assert_eq!(parse_notification(b"%output 3 hi"), None);
        assert_eq!(parse_notification(b"%output %3"), None);
        assert_eq!(parse_notification(b"%output %x hi"), None);
    }

    #[test]
    fn test_parse_notification_layout_change_full_and_minimal() {
        assert_eq!(
            parse_notification(b"%layout-change @0 8205,80x24,0,0 8205,80x24,0,0 *"),
            Some(Event::LayoutChange {
                window: 0,
                layout: "8205,80x24,0,0".to_owned(),
                visible_layout: Some("8205,80x24,0,0".to_owned()),
                flags: Some("*".to_owned()),
            })
        );
        assert_eq!(
            parse_notification(b"%layout-change @2 abcd,80x24,0,0"),
            Some(Event::LayoutChange {
                window: 2,
                layout: "abcd,80x24,0,0".to_owned(),
                visible_layout: None,
                flags: None,
            })
        );
    }

    #[test]
    fn test_parse_notification_window_add_and_close() {
        assert_eq!(
            parse_notification(b"%window-add @0"),
            Some(Event::WindowAdd { window: 0 })
        );
        assert_eq!(
            parse_notification(b"%window-close @4"),
            Some(Event::WindowClose { window: 4 })
        );
        assert_eq!(
            parse_notification(b"%unlinked-window-close @4"),
            Some(Event::WindowClose { window: 4 })
        );
    }

    #[test]
    fn test_parse_notification_session_changed_with_and_without_name() {
        assert_eq!(
            parse_notification(b"%session-changed $0 my session"),
            Some(Event::SessionChanged {
                session: 0,
                name: "my session".to_owned(),
            })
        );
        assert_eq!(
            parse_notification(b"%session-changed $1"),
            Some(Event::SessionChanged {
                session: 1,
                name: String::new(),
            })
        );
    }

    #[test]
    fn test_parse_notification_pane_mode_changed() {
        assert_eq!(
            parse_notification(b"%pane-mode-changed %1"),
            Some(Event::PaneModeChanged { pane: 1 })
        );
    }

    #[test]
    fn test_parse_notification_exit_with_and_without_reason() {
        assert_eq!(
            parse_notification(b"%exit"),
            Some(Event::Exit { reason: None })
        );
        assert_eq!(
            parse_notification(b"%exit server exited"),
            Some(Event::Exit {
                reason: Some("server exited".to_owned()),
            })
        );
    }

    #[test]
    fn test_parse_notification_unmodeled_becomes_other() {
        assert_eq!(
            parse_notification(b"%pause %1"),
            Some(Event::Other {
                name: "%pause".to_owned(),
                args: "%1".to_owned(),
            })
        );
        assert_eq!(
            parse_notification(b"%sessions-changed"),
            Some(Event::Other {
                name: "%sessions-changed".to_owned(),
                args: String::new(),
            })
        );
    }

    #[test]
    fn test_parse_notification_malformed_known_notification_returns_none() {
        assert_eq!(parse_notification(b"%window-add nope"), None);
        assert_eq!(parse_notification(b"%pane-mode-changed @1"), None);
        assert_eq!(parse_notification(b"%session-changed 0 name"), None);
    }

    #[test]
    fn test_parse_notification_non_percent_line_returns_none() {
        assert_eq!(parse_notification(b"plain text"), None);
        assert_eq!(parse_notification(b""), None);
    }
}
