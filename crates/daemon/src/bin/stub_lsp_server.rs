//! A minimal stub LSP server for the daemon's diagnostics integration test
//! (issue #177). Not a production component — it exists only so
//! `crates/daemon/tests/lsp_diagnostics.rs` can drive the full wiring against a
//! deterministic server instead of a real, environment-dependent one.
//!
//! It speaks just enough of the protocol for the diagnostics path:
//! `Content-Length`-framed JSON-RPC over stdio, a canned `initialize` result,
//! and — on every `didOpen` / `didChange` — a `publishDiagnostics` whose set is
//! decided purely by the document text it is sent:
//!
//! - text containing the marker `LSP_STUB_ERROR` publishes one diagnostic;
//! - text without it publishes an empty set (clearing the file).
//!
//! That lets the test introduce an error (write a file containing the marker)
//! and then clear it (rewrite without the marker) and observe the daemon's
//! diagnostics model converge. `shutdown` / `exit` end the loop cleanly.
//!
//! Parsing is deliberately hand-rolled and dependency-free (no `serde_json`):
//! the messages are tiny and fully controlled by the test, so scanning for the
//! `"method"`, `"id"`, and `"uri"` fields is sufficient and keeps the
//! production daemon crate's dependency tree untouched. The diagnostic marker
//! and the reported message are configurable via `--marker <m>` / `--message
//! <m>` args, so one test can run two stubs that report distinguishable
//! diagnostics (the aggregation case).

use std::io::{self, Read, Write};

/// The marker text that triggers a diagnostic and the message the stub reports,
/// resolved from `--marker` / `--message` args with sensible defaults.
struct Config {
    marker: String,
    message: String,
}

/// Parse `--marker <m>` and `--message <m>` from the process args. Defaults: the
/// marker is `LSP_STUB_ERROR`; the message echoes the marker so a single stub is
/// self-describing.
fn config() -> Config {
    let mut marker = None;
    let mut message = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--marker" => marker = args.next(),
            "--message" => message = args.next(),
            _ => {}
        }
    }
    let marker = marker.unwrap_or_else(|| "LSP_STUB_ERROR".to_string());
    let message = message.unwrap_or_else(|| format!("stub error: {marker}"));
    Config { marker, message }
}

fn main() -> io::Result<()> {
    let Config { marker, message } = config();
    let mut stdin = io::stdin().lock();
    let stdout = io::stdout();

    loop {
        let Some(body) = read_message(&mut stdin)? else {
            // stdin closed; the daemon dropped the server.
            return Ok(());
        };
        let Some(method) = scan_field(&body, "method") else {
            // A response (no method) — the stub issues no requests, so ignore.
            continue;
        };

        match method.as_str() {
            "initialize" => {
                let id = scan_field(&body, "id").unwrap_or_else(|| "0".to_string());
                // A bare, capability-light initialize result is enough: the
                // daemon's client only needs the handshake to complete.
                let result =
                    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{}}}}}}"#);
                write_message(&stdout, &result)?;
            }
            "shutdown" => {
                let id = scan_field(&body, "id").unwrap_or_else(|| "0".to_string());
                write_message(
                    &stdout,
                    &format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#),
                )?;
            }
            "exit" => return Ok(()),
            "textDocument/didOpen" | "textDocument/didChange" => {
                if let Some(uri) = scan_field(&body, "uri") {
                    let has_error = body.contains(&marker);
                    let notification = publish_diagnostics(&uri, has_error, &message);
                    write_message(&stdout, &notification)?;
                }
            }
            // `initialized`, `didClose`, and anything else need no reply.
            _ => {}
        }
    }
}

/// Build a `textDocument/publishDiagnostics` notification: one diagnostic when
/// `has_error`, an empty (clearing) set otherwise.
fn publish_diagnostics(uri: &str, has_error: bool, message: &str) -> String {
    let diagnostics = if has_error {
        format!(
            r#"[{{"range":{{"start":{{"line":0,"character":0}},"end":{{"line":0,"character":1}}}},"severity":1,"source":"stub","message":{message:?}}}]"#
        )
    } else {
        "[]".to_string()
    };
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{{"uri":{uri:?},"diagnostics":{diagnostics}}}}}"#
    )
}

/// Read one `Content-Length`-framed JSON-RPC message body from `reader`, or
/// `None` at clean EOF before any header.
fn read_message<R: Read>(reader: &mut R) -> io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    let mut line = Vec::new();

    // Read headers line by line until the blank separator.
    loop {
        line.clear();
        if !read_line(reader, &mut line)? {
            return Ok(None);
        }
        let header = String::from_utf8_lossy(&line);
        let trimmed = header.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().ok();
        }
    }

    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// Read one line (through `\n`) into `buf`. Returns `false` at EOF before any
/// byte was read.
fn read_line<R: Read>(reader: &mut R, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut byte = [0u8; 1];
    let mut any = false;
    loop {
        match reader.read(&mut byte)? {
            0 => return Ok(any),
            _ => {
                any = true;
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(true);
                }
            }
        }
    }
}

/// Write `body` to `stdout` with the LSP `Content-Length` frame, flushing so the
/// daemon's reader sees it immediately.
fn write_message(stdout: &io::Stdout, body: &str) -> io::Result<()> {
    let mut handle = stdout.lock();
    write!(handle, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    handle.flush()
}

/// Extract a JSON string-or-number field's value by key, scanning the raw body.
///
/// Handles `"key":"string"` (returning the unquoted, unescaped-enough value for
/// the test's controlled inputs) and `"key":123` (returning the number's
/// digits). Sufficient for the stub's tiny, test-authored messages — not a
/// general JSON parser. Returns the first match.
fn scan_field(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find(&needle) {
        let after_key = search_from + rel + needle.len();
        let rest = body[after_key..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            search_from = after_key;
            continue;
        };
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('"') {
            // String value: take up to the next unescaped quote.
            let mut value = String::new();
            let mut chars = rest.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => return Some(value),
                    other => value.push(other),
                }
            }
            return Some(value);
        } else {
            // Numeric (or literal) value: take while digit/sign.
            let value: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if !value.is_empty() {
                return Some(value);
            }
        }
        search_from = after_key;
    }
    None
}
