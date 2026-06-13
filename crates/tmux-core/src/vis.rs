//! OpenBSD-vis octal-escape decoding for tmux control-mode `%output` payloads.

/// Decode tmux's `VIS_OCTAL` escaping: each `\ooo` (a backslash followed by
/// exactly three octal digits, the first `0`–`3`) becomes the byte it names;
/// every other byte passes through untouched, including raw high bytes that
/// form UTF-8 and malformed escapes (a lone backslash, a too-short or
/// out-of-range escape).
///
/// tmux octal-escapes control bytes (`< 0x20`) and the backslash itself (as
/// `\134`) but leaves printable and UTF-8 bytes raw — so an incomplete
/// multi-byte sequence split across two `%output` notifications arrives as raw
/// orphan bytes, reassembled by concatenating successive
/// [`Event::Output`](crate::Event::Output) payloads. Decoding each notification
/// independently is correct because an escape unit (`\ooo`) never straddles a
/// notification boundary: tmux emits one notification per buffered chunk and
/// escapes whole bytes within it.
pub(crate) fn decode_octal(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut i = 0;
    while i < escaped.len() {
        if escaped[i] == b'\\'
            && i + 3 < escaped.len()
            && matches!(escaped[i + 1], b'0'..=b'3')
            && matches!(escaped[i + 2], b'0'..=b'7')
            && matches!(escaped[i + 3], b'0'..=b'7')
        {
            let value = (escaped[i + 1] - b'0') * 64
                + (escaped[i + 2] - b'0') * 8
                + (escaped[i + 3] - b'0');
            out.push(value);
            i += 4;
        } else {
            out.push(escaped[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_octal_plain_ascii_unchanged() {
        assert_eq!(decode_octal(b"hello world"), b"hello world".to_vec());
    }

    #[test]
    fn test_decode_octal_empty_returns_empty() {
        assert_eq!(decode_octal(b""), Vec::<u8>::new());
    }

    #[test]
    fn test_decode_octal_control_bytes_decoded() {
        // CR LF and an ANSI escape introducer.
        assert_eq!(decode_octal(b"ls\\015\\012"), b"ls\r\n".to_vec());
        assert_eq!(decode_octal(b"\\033[1m"), b"\x1b[1m".to_vec());
    }

    #[test]
    fn test_decode_octal_full_byte_range() {
        assert_eq!(decode_octal(b"\\000"), vec![0u8]);
        assert_eq!(decode_octal(b"\\134"), vec![b'\\']);
        assert_eq!(decode_octal(b"\\377"), vec![0xffu8]);
    }

    #[test]
    fn test_decode_octal_high_bytes_pass_through_raw() {
        // tmux leaves valid UTF-8 raw; the decoder must not touch it.
        assert_eq!(decode_octal("käse".as_bytes()), "käse".as_bytes().to_vec());
        assert_eq!(decode_octal(&[0xc3, 0xa4]), vec![0xc3, 0xa4]);
    }

    #[test]
    fn test_decode_octal_malformed_escapes_kept_literally() {
        assert_eq!(decode_octal(b"ab\\"), b"ab\\".to_vec()); // trailing backslash
        assert_eq!(decode_octal(b"\\01"), b"\\01".to_vec()); // too short
        assert_eq!(decode_octal(b"\\477"), b"\\477".to_vec()); // first digit out of range
        assert_eq!(decode_octal(b"\\089"), b"\\089".to_vec()); // non-octal digits
    }

    #[test]
    fn test_decode_octal_escape_adjacent_to_literals() {
        assert_eq!(decode_octal(b"x\\015y"), b"x\ry".to_vec());
    }
}
