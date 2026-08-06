use std::fmt;

use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

/// Absolute protocol-frame ceiling enforced by every Maestro adapter. Vendor
/// CLIs may accept more, but Maestro does not allocate unbounded frames.
pub const MAXIMUM_JSONL_FRAME_BYTES: usize = 1024 * 1024;

const MAXIMUM_JSONL_FRAMES_PER_PUSH: usize = 1024;
const MAXIMUM_JSONL_BATCH_BYTES: usize = (MAXIMUM_JSONL_FRAME_BYTES + 1) * 4;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsonLineError {
    #[error("JSONL frame limit {requested} is invalid; it must be between 1 and {maximum} bytes")]
    InvalidFrameLimit { requested: usize, maximum: usize },
    #[error("JSONL frame exceeds the configured {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("JSONL input chunk exceeds the configured {limit}-byte batch limit")]
    BatchTooLarge { limit: usize },
    #[error("JSONL input chunk contains more than {limit} complete frames")]
    TooManyFrames { limit: usize },
    #[error("JSONL frame is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("JSONL stream ended with an incomplete frame")]
    IncompleteFrame,
    #[error("JSONL decoder is poisoned after an earlier protocol error")]
    Poisoned,
}

/// Frames accepted before a terminal protocol error are returned to the
/// caller so they can be persisted before the run is failed. Once a terminal
/// error is present, the decoder is poisoned and accepts no more input.
#[derive(PartialEq)]
pub struct DecodedJsonLines {
    frames: Vec<Value>,
    terminal_error: Option<JsonLineError>,
}

impl DecodedJsonLines {
    fn clean(frames: Vec<Value>) -> Self {
        Self {
            frames,
            terminal_error: None,
        }
    }

    fn failed(frames: Vec<Value>, error: JsonLineError) -> Self {
        Self {
            frames,
            terminal_error: Some(error),
        }
    }

    pub fn frames(&self) -> &[Value] {
        &self.frames
    }

    pub fn terminal_error(&self) -> Option<&JsonLineError> {
        self.terminal_error.as_ref()
    }

    pub fn into_parts(self) -> (Vec<Value>, Option<JsonLineError>) {
        (self.frames, self.terminal_error)
    }
}

impl fmt::Debug for DecodedJsonLines {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedJsonLines")
            .field("frame_count", &self.frames.len())
            .field("terminal_error", &self.terminal_error)
            .finish()
    }
}

/// Incremental newline-delimited JSON decoder with hard frame, read-batch, and
/// fan-out bounds. Protocol violations poison the decoder so callers cannot
/// continue from an ambiguous byte boundary.
pub struct BoundedJsonLineDecoder {
    maximum_frame_bytes: usize,
    maximum_batch_bytes: usize,
    maximum_frames_per_push: usize,
    buffer: Vec<u8>,
    poisoned: bool,
}

impl fmt::Debug for BoundedJsonLineDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedJsonLineDecoder")
            .field("maximum_frame_bytes", &self.maximum_frame_bytes)
            .field("maximum_batch_bytes", &self.maximum_batch_bytes)
            .field("maximum_frames_per_push", &self.maximum_frames_per_push)
            .field("buffered_bytes", &self.buffer.len())
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl BoundedJsonLineDecoder {
    /// Creates a decoder. The frame limit excludes the trailing newline and
    /// cannot exceed [`MAXIMUM_JSONL_FRAME_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns a limit error when the configured limit is zero or above the
    /// global Maestro ceiling.
    pub fn new(maximum_frame_bytes: usize) -> Result<Self, JsonLineError> {
        validate_frame_limit(maximum_frame_bytes)?;
        let maximum_batch_bytes = maximum_frame_bytes
            .saturating_add(1)
            .saturating_mul(4)
            .min(MAXIMUM_JSONL_BATCH_BYTES);
        Ok(Self {
            maximum_frame_bytes,
            maximum_batch_bytes,
            maximum_frames_per_push: MAXIMUM_JSONL_FRAMES_PER_PUSH,
            buffer: Vec::new(),
            poisoned: false,
        })
    }

    /// Decodes every complete frame in `chunk`, retaining any valid prefix
    /// frames even if a later frame terminates the protocol stream.
    pub fn push(&mut self, chunk: &[u8]) -> DecodedJsonLines {
        if self.poisoned {
            return DecodedJsonLines::failed(Vec::new(), JsonLineError::Poisoned);
        }
        let batch_too_large = chunk.len() > self.maximum_batch_bytes;
        let mut frames = Vec::new();
        let mut remaining = &chunk[..chunk.len().min(self.maximum_batch_bytes)];
        while let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') {
            if frames.len() >= self.maximum_frames_per_push {
                let limit = self.maximum_frames_per_push;
                self.poison();
                return DecodedJsonLines::failed(frames, JsonLineError::TooManyFrames { limit });
            }

            let segment = &remaining[..newline];
            if let Err(error) = self.extend_checked(segment) {
                return DecodedJsonLines::failed(frames, error);
            }
            if self.buffer.last() == Some(&b'\r') {
                self.buffer.pop();
            }
            match serde_json::from_slice(&self.buffer) {
                Ok(frame) => frames.push(frame),
                Err(error) => {
                    self.poison();
                    return DecodedJsonLines::failed(
                        frames,
                        JsonLineError::InvalidJson(error.to_string()),
                    );
                }
            }
            self.clear_buffer();
            remaining = &remaining[newline + 1..];
        }

        if batch_too_large {
            let limit = self.maximum_batch_bytes;
            self.poison();
            return DecodedJsonLines::failed(frames, JsonLineError::BatchTooLarge { limit });
        }
        if let Err(error) = self.extend_checked(remaining) {
            return DecodedJsonLines::failed(frames, error);
        }
        DecodedJsonLines::clean(frames)
    }

    /// Validates that EOF occurred at a frame boundary.
    ///
    /// # Errors
    ///
    /// Returns an incomplete-frame or poisoned error when the stream cannot be
    /// considered cleanly closed.
    pub fn finish(&mut self) -> Result<(), JsonLineError> {
        if self.poisoned {
            return Err(JsonLineError::Poisoned);
        }
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.poison();
        Err(JsonLineError::IncompleteFrame)
    }

    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }

    fn extend_checked(&mut self, bytes: &[u8]) -> Result<(), JsonLineError> {
        if bytes.len() > self.maximum_frame_bytes.saturating_sub(self.buffer.len()) {
            let limit = self.maximum_frame_bytes;
            self.poison();
            return Err(JsonLineError::FrameTooLarge { limit });
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn clear_buffer(&mut self) {
        self.buffer.zeroize();
        self.buffer.clear();
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.clear_buffer();
    }
}

impl Drop for BoundedJsonLineDecoder {
    fn drop(&mut self) {
        self.buffer.zeroize();
    }
}

/// A wire frame whose Debug output never reveals protocol content.
#[derive(PartialEq, Eq)]
pub struct SensitiveWireFrame(Vec<u8>);

impl SensitiveWireFrame {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }
}

impl fmt::Debug for SensitiveWireFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveWireFrame")
            .field("bytes", &"[REDACTED]")
            .field("length", &self.0.len())
            .finish()
    }
}

impl Drop for SensitiveWireFrame {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Encodes one bounded JSONL frame.
///
/// # Errors
///
/// Returns a limit or size error when the configured limit is invalid or the
/// serialized value exceeds it.
pub fn encode_json_line(
    value: &Value,
    maximum_frame_bytes: usize,
) -> Result<SensitiveWireFrame, JsonLineError> {
    validate_frame_limit(maximum_frame_bytes)?;
    let mut bytes =
        serde_json::to_vec(value).map_err(|error| JsonLineError::InvalidJson(error.to_string()))?;
    if bytes.len() > maximum_frame_bytes {
        bytes.zeroize();
        return Err(JsonLineError::FrameTooLarge {
            limit: maximum_frame_bytes,
        });
    }
    bytes.push(b'\n');
    Ok(SensitiveWireFrame(bytes))
}

fn validate_frame_limit(maximum_frame_bytes: usize) -> Result<(), JsonLineError> {
    if maximum_frame_bytes == 0 || maximum_frame_bytes > MAXIMUM_JSONL_FRAME_BYTES {
        return Err(JsonLineError::InvalidFrameLimit {
            requested: maximum_frame_bytes,
            maximum: MAXIMUM_JSONL_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn fragmented_and_batched_frames_decode_in_order() {
        let mut decoder = BoundedJsonLineDecoder::new(128).expect("decoder");
        assert!(decoder.push(b"{\"id\":1").frames().is_empty());
        let batch = decoder.push(b"}\n{\"method\":\"ready\"}\r\n");
        assert_eq!(
            batch.frames(),
            &[json!({ "id": 1 }), json!({ "method": "ready" })]
        );
        assert_eq!(batch.terminal_error(), None);
        assert_eq!(decoder.finish(), Ok(()));
    }

    #[test]
    fn oversized_frame_is_rejected_before_it_is_buffered() {
        let mut decoder = BoundedJsonLineDecoder::new(8).expect("decoder");
        let batch = decoder.push(b"123456789");
        assert_eq!(
            batch.terminal_error(),
            Some(&JsonLineError::FrameTooLarge { limit: 8 })
        );
        assert_eq!(decoder.buffered_bytes(), 0);
        assert_eq!(
            decoder.push(b"{}\n").terminal_error(),
            Some(&JsonLineError::Poisoned)
        );
    }

    #[test]
    fn valid_prefix_survives_a_malformed_or_oversized_later_frame() {
        let mut malformed = BoundedJsonLineDecoder::new(128).expect("decoder");
        let batch = malformed.push(b"{\"id\":1}\n{not-json}\n");
        assert_eq!(batch.frames(), &[json!({ "id": 1 })]);
        assert!(matches!(
            batch.terminal_error(),
            Some(JsonLineError::InvalidJson(_))
        ));

        let mut oversized = BoundedJsonLineDecoder::new(8).expect("decoder");
        let batch = oversized.push(b"{}\n123456789\n");
        assert_eq!(batch.frames(), &[json!({})]);
        assert_eq!(
            batch.terminal_error(),
            Some(&JsonLineError::FrameTooLarge { limit: 8 })
        );
    }

    #[test]
    fn tiny_frame_fan_out_is_bounded_without_losing_the_accepted_prefix() {
        let input = "{}\n".repeat(MAXIMUM_JSONL_FRAMES_PER_PUSH + 1);
        let mut decoder = BoundedJsonLineDecoder::new(input.len()).expect("decoder");
        let batch = decoder.push(input.as_bytes());

        assert_eq!(batch.frames().len(), MAXIMUM_JSONL_FRAMES_PER_PUSH);
        assert_eq!(
            batch.terminal_error(),
            Some(&JsonLineError::TooManyFrames {
                limit: MAXIMUM_JSONL_FRAMES_PER_PUSH
            })
        );
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn oversized_input_chunk_preserves_complete_frames_in_its_bounded_prefix() {
        let mut decoder = BoundedJsonLineDecoder::new(8).expect("decoder");
        let limit = decoder.maximum_batch_bytes;
        let mut input = b"{}\n".to_vec();
        input.resize(limit + 1, b' ');
        let batch = decoder.push(&input);

        assert_eq!(batch.frames(), &[json!({})]);
        assert_eq!(
            batch.terminal_error(),
            Some(&JsonLineError::BatchTooLarge { limit })
        );
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn incomplete_final_frame_fails_closed() {
        let mut decoder = BoundedJsonLineDecoder::new(128).expect("decoder");
        assert_eq!(decoder.push(b"{\"id\":1}").terminal_error(), None);
        assert_eq!(decoder.finish(), Err(JsonLineError::IncompleteFrame));
    }

    #[test]
    fn decoder_debug_never_exposes_partial_raw_bytes() {
        let mut decoder = BoundedJsonLineDecoder::new(128).expect("decoder");
        decoder.push(b"{\"token\":\"partial-secret");
        let debug = format!("{decoder:?}");
        assert!(!debug.contains("partial-secret"));
        assert!(debug.contains("buffered_bytes"));
    }

    #[test]
    fn one_mib_boundary_is_accepted_and_one_byte_more_is_rejected() {
        let exact = Value::String("a".repeat(MAXIMUM_JSONL_FRAME_BYTES - 2));
        let frame = encode_json_line(&exact, MAXIMUM_JSONL_FRAME_BYTES).expect("exact boundary");
        assert_eq!(frame.as_bytes().len(), MAXIMUM_JSONL_FRAME_BYTES + 1);
        let bytes = frame.into_bytes();
        let mut decoder = BoundedJsonLineDecoder::new(MAXIMUM_JSONL_FRAME_BYTES).expect("decoder");
        let batch = decoder.push(&bytes);
        assert_eq!(batch.frames().len(), 1);
        assert_eq!(batch.terminal_error(), None);

        let oversized = Value::String("a".repeat(MAXIMUM_JSONL_FRAME_BYTES - 1));
        assert_eq!(
            encode_json_line(&oversized, MAXIMUM_JSONL_FRAME_BYTES),
            Err(JsonLineError::FrameTooLarge {
                limit: MAXIMUM_JSONL_FRAME_BYTES
            })
        );
    }

    #[test]
    fn configured_limit_cannot_exceed_the_global_ceiling() {
        assert!(matches!(
            BoundedJsonLineDecoder::new(0),
            Err(JsonLineError::InvalidFrameLimit { requested: 0, .. })
        ));
        assert!(matches!(
            BoundedJsonLineDecoder::new(MAXIMUM_JSONL_FRAME_BYTES + 1),
            Err(JsonLineError::InvalidFrameLimit { .. })
        ));
    }

    #[test]
    fn encoded_frame_bytes_remain_zeroizing_and_debug_is_redacted() {
        let frame =
            encode_json_line(&json!({ "secret": "fixture-token" }), 128).expect("encoded frame");
        assert!(frame.as_bytes().ends_with(b"\n"));
        assert!(!format!("{frame:?}").contains("fixture-token"));
        let bytes = frame.into_bytes();
        assert!(bytes.ends_with(b"\n"));
        assert_eq!(
            encode_json_line(&json!({ "value": "too long" }), 4),
            Err(JsonLineError::FrameTooLarge { limit: 4 })
        );
    }
}
