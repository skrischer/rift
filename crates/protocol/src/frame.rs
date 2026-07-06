//! Length-delimited JSON frame codec for the client/daemon transport.
//!
//! Each frame is a `u32` big-endian byte length followed by that many bytes of
//! `serde_json`. [`encode_frame`] produces a single frame; [`FrameDecoder`]
//! accumulates raw bytes and yields complete frames, buffering any incomplete
//! tail so callers can feed arbitrary partial reads.

use serde::de::DeserializeOwned;
use serde::Serialize;

/// Number of bytes in the big-endian length prefix that precedes each frame.
const LEN_PREFIX: usize = std::mem::size_of::<u32>();

/// Maximum accepted payload length of a single frame, in bytes.
///
/// Legitimate protocol messages stay far below this generous bound; a length
/// prefix beyond it indicates a corrupted stream, so the decoder fails fast
/// instead of buffering toward 4 GiB. Both transport ends treat frame errors
/// as connection-fatal.
pub const MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// Errors raised while encoding or decoding a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The payload could not be (de)serialized as JSON.
    #[error("json codec error: {0}")]
    Json(#[from] serde_json::Error),
    /// A frame advertised a length that does not fit in `usize` on this target.
    #[error("frame length {0} exceeds platform addressable size")]
    LengthOverflow(u32),
    /// A frame advertised a length beyond [`MAX_FRAME_LEN`], indicating a
    /// corrupted length prefix.
    #[error("frame length {0} exceeds maximum {MAX_FRAME_LEN}")]
    FrameTooLarge(u32),
}

/// Encode a value as a single length-delimited JSON frame.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let payload = serde_json::to_vec(value)?;
    let mut frame = Vec::with_capacity(LEN_PREFIX + payload.len());
    let len = u32::try_from(payload.len()).map_err(|_| {
        // A single JSON message larger than u32::MAX is not a real protocol
        // case; surface it as a serializer error rather than silently truncate.
        FrameError::Json(serde::ser::Error::custom("frame payload exceeds u32::MAX"))
    })?;
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Accumulates bytes and yields complete frames, retaining partial tails.
///
/// Bytes arrive via [`FrameDecoder::push`] in whatever chunks the transport
/// delivers; [`FrameDecoder::next_frame`] decodes one buffered frame per call
/// and returns `Ok(None)` once only an incomplete frame remains.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// Create an empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append freshly read bytes to the internal buffer.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Decode and remove the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` when the buffer does not yet hold a full frame. The
    /// decoded payload is parsed as `T`; a JSON error leaves the buffer
    /// unchanged so the caller can decide how to recover.
    pub fn next_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        if self.buffer.len() < LEN_PREFIX {
            return Ok(None);
        }

        let mut len_bytes = [0u8; LEN_PREFIX];
        len_bytes.copy_from_slice(&self.buffer[..LEN_PREFIX]);
        let payload_len = u32::from_be_bytes(len_bytes);
        if u64::from(payload_len) > MAX_FRAME_LEN as u64 {
            return Err(FrameError::FrameTooLarge(payload_len));
        }
        let payload_len =
            usize::try_from(payload_len).map_err(|_| FrameError::LengthOverflow(payload_len))?;

        let frame_end = LEN_PREFIX + payload_len;
        if self.buffer.len() < frame_end {
            return Ok(None);
        }

        let value = serde_json::from_slice(&self.buffer[LEN_PREFIX..frame_end])?;
        self.buffer.drain(..frame_end);
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Sample {
        id: u32,
        name: String,
    }

    fn sample(id: u32, name: &str) -> Sample {
        Sample {
            id,
            name: name.to_owned(),
        }
    }

    #[test]
    fn test_next_frame_split_across_two_pushes_decodes_after_completion() {
        let frame = encode_frame(&sample(1, "hello")).expect("encode");
        let split = frame.len() / 2;

        let mut decoder = FrameDecoder::new();
        decoder.push(&frame[..split]);
        assert_eq!(decoder.next_frame::<Sample>().expect("partial"), None);

        decoder.push(&frame[split..]);
        assert_eq!(
            decoder.next_frame::<Sample>().expect("complete"),
            Some(sample(1, "hello"))
        );
        assert_eq!(decoder.next_frame::<Sample>().expect("drained"), None);
    }

    #[test]
    fn test_next_frame_two_frames_in_one_push_decodes_both_in_order() {
        let mut bytes = encode_frame(&sample(1, "first")).expect("encode first");
        bytes.extend(encode_frame(&sample(2, "second")).expect("encode second"));

        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);

        assert_eq!(
            decoder.next_frame::<Sample>().expect("first"),
            Some(sample(1, "first"))
        );
        assert_eq!(
            decoder.next_frame::<Sample>().expect("second"),
            Some(sample(2, "second"))
        );
        assert_eq!(decoder.next_frame::<Sample>().expect("drained"), None);
    }

    #[test]
    fn test_next_frame_boundary_split_inside_length_prefix_buffers_until_complete() {
        let frame = encode_frame(&sample(7, "boundary")).expect("encode");

        let mut decoder = FrameDecoder::new();
        // Split inside the 4-byte length prefix so even the header is partial.
        decoder.push(&frame[..2]);
        assert_eq!(decoder.next_frame::<Sample>().expect("no header"), None);

        decoder.push(&frame[2..LEN_PREFIX]);
        assert_eq!(decoder.next_frame::<Sample>().expect("header only"), None);

        decoder.push(&frame[LEN_PREFIX..]);
        assert_eq!(
            decoder.next_frame::<Sample>().expect("complete"),
            Some(sample(7, "boundary"))
        );
    }

    #[test]
    fn test_next_frame_length_prefix_above_max_errors_immediately() {
        let corrupt_len = u32::MAX;

        let mut decoder = FrameDecoder::new();
        decoder.push(&corrupt_len.to_be_bytes());

        let err = decoder
            .next_frame::<Sample>()
            .expect_err("corrupted length prefix must error before buffering");
        assert!(matches!(err, FrameError::FrameTooLarge(len) if len == corrupt_len));
    }

    #[test]
    fn test_next_frame_length_prefix_just_above_max_errors_immediately() {
        let corrupt_len = MAX_FRAME_LEN as u32 + 1;

        let mut decoder = FrameDecoder::new();
        decoder.push(&corrupt_len.to_be_bytes());

        let err = decoder
            .next_frame::<Sample>()
            .expect_err("length one past the bound must error");
        assert!(matches!(err, FrameError::FrameTooLarge(len) if len == corrupt_len));
    }

    #[test]
    fn test_next_frame_length_prefix_at_max_waits_for_payload() {
        let max_len = MAX_FRAME_LEN as u32;

        let mut decoder = FrameDecoder::new();
        decoder.push(&max_len.to_be_bytes());

        // A frame exactly at the bound is legal: the decoder keeps waiting for
        // the payload instead of rejecting the prefix.
        assert_eq!(decoder.next_frame::<Sample>().expect("at bound"), None);
    }

    #[test]
    fn test_next_frame_trailing_partial_frame_is_retained() {
        let mut bytes = encode_frame(&sample(1, "whole")).expect("encode whole");
        let partial = encode_frame(&sample(2, "partial")).expect("encode partial");
        bytes.extend_from_slice(&partial[..partial.len() - 3]);

        let mut decoder = FrameDecoder::new();
        decoder.push(&bytes);

        assert_eq!(
            decoder.next_frame::<Sample>().expect("whole"),
            Some(sample(1, "whole"))
        );
        assert_eq!(decoder.next_frame::<Sample>().expect("partial held"), None);

        decoder.push(&partial[partial.len() - 3..]);
        assert_eq!(
            decoder.next_frame::<Sample>().expect("partial completed"),
            Some(sample(2, "partial"))
        );
    }
}
