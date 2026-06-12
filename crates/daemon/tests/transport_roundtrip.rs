//! Round-trip integration test for the daemon transport seam.
//!
//! Exercises [`rift_daemon::serve`] over a `tokio::io::duplex` loopback: a
//! framed `ClientMessage::Hello` written into the daemon side must produce a
//! framed `DaemonMessage::Welcome` back out, proving the full
//! `ClientMessage -> daemon -> DaemonMessage` round-trip over the channel.

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
