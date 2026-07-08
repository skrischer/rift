//! Round-trip integration tests for the daemon transport seam.
//!
//! Exercises [`rift_daemon::serve`] over a `tokio::io::duplex` loopback: a
//! framed `ClientMessage::Hello` written into the daemon side must produce a
//! framed `DaemonMessage::Welcome` back out, proving the full
//! `ClientMessage -> daemon -> DaemonMessage` round-trip over the channel.
//! A mismatched `Hello` must produce the daemon's own `Welcome` followed by a
//! clean close with no further frames (the version gate, #473).

use rift_daemon::serve;
use rift_protocol::{encode_frame, ClientMessage, DaemonMessage, FrameDecoder, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn test_serve_hello_over_duplex_returns_welcome() {
    // `client` is the test's end; `daemon` is handed to `serve`. Splitting each
    // duplex gives the daemon a reader (client -> daemon) and a writer
    // (daemon -> client).
    let (client, daemon) = tokio::io::duplex(64 * 1024);
    let (daemon_reader, daemon_writer) = tokio::io::split(daemon);
    let (mut client_reader, mut client_writer) = tokio::io::split(client);

    let server = tokio::spawn(async move { serve(daemon_reader, daemon_writer, None).await });

    let hello = encode_frame(&ClientMessage::Hello {
        version: PROTOCOL_VERSION,
    })
    .expect("encode Hello");
    client_writer.write_all(&hello).await.expect("send Hello");
    client_writer.flush().await.expect("flush Hello");

    // Read until the decoder yields a complete frame.
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 4096];
    let welcome = loop {
        if let Some(msg) = decoder
            .next_frame::<DaemonMessage>()
            .expect("decode DaemonMessage")
        {
            break msg;
        }
        let n = client_reader.read(&mut buf).await.expect("read reply");
        assert!(n > 0, "daemon closed before sending Welcome");
        decoder.push(&buf[..n]);
    };

    assert_eq!(
        welcome,
        DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        }
    );

    // Close the client side so `serve` observes EOF and returns cleanly.
    drop(client_writer);
    drop(client_reader);
    server
        .await
        .expect("serve task joins")
        .expect("serve returns Ok");
}

#[tokio::test]
async fn test_serve_mismatched_hello_over_duplex_returns_welcome_then_clean_close() {
    let (client, daemon) = tokio::io::duplex(64 * 1024);
    let (daemon_reader, daemon_writer) = tokio::io::split(daemon);
    let (mut client_reader, mut client_writer) = tokio::io::split(client);

    let server = tokio::spawn(async move { serve(daemon_reader, daemon_writer, None).await });

    let hello = encode_frame(&ClientMessage::Hello {
        version: PROTOCOL_VERSION + 1,
    })
    .expect("encode mismatched Hello");
    client_writer.write_all(&hello).await.expect("send Hello");
    client_writer.flush().await.expect("flush Hello");

    // First frame: the daemon's OWN version — the orderly early mismatch
    // signal a stale client can act on before any stream traffic.
    let mut decoder = FrameDecoder::new();
    let mut buf = vec![0u8; 4096];
    let welcome = loop {
        if let Some(msg) = decoder
            .next_frame::<DaemonMessage>()
            .expect("decode DaemonMessage")
        {
            break msg;
        }
        let n = client_reader.read(&mut buf).await.expect("read reply");
        assert!(n > 0, "daemon closed before sending Welcome");
        decoder.push(&buf[..n]);
    };
    assert_eq!(
        welcome,
        DaemonMessage::Welcome {
            version: PROTOCOL_VERSION,
        },
        "the mismatch reply must carry the daemon's own version"
    );

    // The daemon closes without the client hanging up: EOF with no state or
    // stream frames after the Welcome.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(msg) = decoder
                .next_frame::<DaemonMessage>()
                .expect("decode DaemonMessage")
            {
                panic!("unexpected frame after the mismatch Welcome: {msg:?}");
            }
            let n = client_reader.read(&mut buf).await.expect("read until EOF");
            if n == 0 {
                break;
            }
            decoder.push(&buf[..n]);
        }
    })
    .await
    .expect("clean close within the timeout");

    server
        .await
        .expect("serve task joins")
        .expect("the mismatch close is clean, not an error");
}
