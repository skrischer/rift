//! Integration tests against real tmux 3.4 control-mode captures.
//!
//! Fixtures were recorded by driving `tmux -L <unique> -C new-session` over
//! plain pipes (single `-C` works over pipes; see `docs/tmux-reference.md`
//! pitfall 6) and capturing its stdout verbatim, including raw high bytes. They
//! exercise the full notification set, command guards (success and error), and
//! a UTF-8 character split across two `%output` notifications.

use rift_tmux_core::{Client, ConnectionState, Event};

const CONTROL_SESSION: &[u8] = include_bytes!("fixtures/control_session.ctrl");
const OUTPUT_SPLIT_MULTIBYTE: &[u8] = include_bytes!("fixtures/output_split_multibyte.ctrl");

#[test]
fn test_control_session_fixture_yields_full_notification_set() {
    let mut client = Client::new();
    let events = client.feed(CONTROL_SESSION);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WindowAdd { window: 0 })),
        "expected a %window-add for window 0"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WindowClose { .. })),
        "expected a window-close notification"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::WindowRenamed { window: 0, .. })),
        "expected a %window-renamed notification"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::SessionChanged { .. })),
        "expected a %session-changed notification"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::SessionRenamed { session: 0, .. })),
        "expected a %session-renamed notification"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::LayoutChange { .. })),
        "expected a %layout-change notification"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::PaneModeChanged { .. })),
        "expected a %pane-mode-changed notification"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::Output { .. })),
        "expected pane output"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::Exit { .. })),
        "expected an %exit notification"
    );
    // Both a successful and a failed command block are present (the fixture
    // issues a valid command and a deliberately invalid one).
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandReply { error: false, .. })),
        "expected a successful command reply"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandReply { error: true, .. })),
        "expected an error command reply"
    );
    assert_eq!(client.state(), ConnectionState::Exited);
}

#[test]
fn test_control_session_byte_by_byte_feed_matches_whole_feed() {
    let mut whole = Client::new();
    let whole_events = whole.feed(CONTROL_SESSION);

    let mut chunked = Client::new();
    let mut chunked_events = Vec::new();
    for byte in CONTROL_SESSION {
        chunked_events.extend(chunked.feed(&[*byte]));
    }

    // Splitting the stream at every byte boundary (mid-line, mid-escape) yields
    // exactly the same events and final state as one whole feed.
    assert_eq!(whole_events, chunked_events);
    assert_eq!(whole.state(), chunked.state());
}

#[test]
fn test_split_multibyte_fixture_reassembles_across_notifications() {
    let mut client = Client::new();
    let events = client.feed(OUTPUT_SPLIT_MULTIBYTE);

    let pane0: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::Output { pane: 0, data } => Some(data.clone()),
            _ => None,
        })
        .flatten()
        .collect();

    // The two orphan bytes of 'ä' (0xC3 0xA4), each delivered raw in its own
    // %output notification, are adjacent once the payloads are concatenated.
    assert!(
        pane0.windows(2).any(|w| w == [0xc3, 0xa4]),
        "expected the split 'ä' bytes 0xC3 0xA4 to reassemble; got {pane0:02x?}"
    );
}
