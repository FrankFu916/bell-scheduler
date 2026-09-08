//! Bounded framing for worker stdin/stdout.

use std::io::{self, Read, Write};

use bytes::{BufMut, Bytes, BytesMut};
use prost::Message;
use thiserror::Error;

const HEADER_LEN: usize = 4;

/// Conservative default limit that prevents an untrusted header from causing
/// an unbounded allocation. Callers may choose a lower per-project limit.
pub const DEFAULT_MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// Failures in the transport frame, distinct from protocol validation errors.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame payload length {declared} exceeds configured maximum {maximum}")]
    FrameTooLarge { declared: usize, maximum: usize },
    #[error("truncated frame header: expected 4 bytes, received {actual}")]
    TruncatedHeader { actual: usize },
    #[error("truncated frame payload: expected {expected} bytes, received {actual}")]
    TruncatedPayload { expected: usize, actual: usize },
    #[error("frame contains {extra} trailing bytes")]
    TrailingBytes { extra: usize },
    #[error("protobuf encode failed: {0}")]
    Encode(#[from] prost::EncodeError),
    #[error("protobuf decode failed: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("frame I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Encode one protobuf message with a four-byte big-endian length prefix.
///
/// # Errors
///
/// Returns [`FrameError::FrameTooLarge`] when the encoded message exceeds the
/// configured or wire-format limit, or [`FrameError::Encode`] when protobuf
/// serialization fails.
pub fn encode_frame<M: Message>(message: &M, maximum: usize) -> Result<Bytes, FrameError> {
    let payload_len = message.encoded_len();
    if payload_len > maximum || payload_len > u32::MAX as usize {
        return Err(FrameError::FrameTooLarge {
            declared: payload_len,
            maximum: maximum.min(u32::MAX as usize),
        });
    }

    let mut frame = BytesMut::with_capacity(HEADER_LEN + payload_len);
    let wire_len = u32::try_from(payload_len).map_err(|_| FrameError::FrameTooLarge {
        declared: payload_len,
        maximum: maximum.min(u32::MAX as usize),
    })?;
    frame.put_u32(wire_len);
    message.encode(&mut frame)?;
    Ok(frame.freeze())
}

/// Decode exactly one complete frame.
///
/// Unlike an incremental decoder, this rejects incomplete and concatenated
/// input explicitly so a worker cannot accidentally accept a partial request.
///
/// # Errors
///
/// Returns a framing error for truncated, oversized, or concatenated input and
/// a protobuf decode error when the bounded payload is malformed.
pub fn decode_frame<M: Message + Default>(frame: &[u8], maximum: usize) -> Result<M, FrameError> {
    if frame.len() < HEADER_LEN {
        return Err(FrameError::TruncatedHeader {
            actual: frame.len(),
        });
    }

    let declared = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if declared > maximum {
        return Err(FrameError::FrameTooLarge { declared, maximum });
    }

    let actual = frame.len() - HEADER_LEN;
    if actual < declared {
        return Err(FrameError::TruncatedPayload {
            expected: declared,
            actual,
        });
    }
    if actual > declared {
        return Err(FrameError::TrailingBytes {
            extra: actual - declared,
        });
    }

    Ok(M::decode(&frame[HEADER_LEN..])?)
}

/// Write one complete frame, retrying interrupted writes through `write_all`.
///
/// # Errors
///
/// Returns a framing or protobuf error from [`encode_frame`], or an I/O error
/// when the complete frame cannot be written and flushed.
pub fn write_frame<W: Write, M: Message>(
    writer: &mut W,
    message: &M,
    maximum: usize,
) -> Result<(), FrameError> {
    let frame = encode_frame(message, maximum)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read one complete frame without allocating until its length is bounded.
///
/// # Errors
///
/// Returns a truncation error on premature EOF, an oversize error before
/// payload allocation, an I/O error, or a protobuf decode error.
pub fn read_frame<R: Read, M: Message + Default>(
    reader: &mut R,
    maximum: usize,
) -> Result<M, FrameError> {
    let mut header = [0_u8; HEADER_LEN];
    let header_read = read_fully(reader, &mut header)?;
    if header_read != HEADER_LEN {
        return Err(FrameError::TruncatedHeader {
            actual: header_read,
        });
    }

    let declared = u32::from_be_bytes(header) as usize;
    if declared > maximum {
        return Err(FrameError::FrameTooLarge { declared, maximum });
    }

    let mut payload = vec![0_u8; declared];
    let payload_read = read_fully(reader, &mut payload)?;
    if payload_read != declared {
        return Err(FrameError::TruncatedPayload {
            expected: declared,
            actual: payload_read,
        });
    }
    Ok(M::decode(payload.as_slice())?)
}

fn read_fully<R: Read>(reader: &mut R, buffer: &mut [u8]) -> Result<usize, io::Error> {
    let mut read = 0;
    while read < buffer.len() {
        match reader.read(&mut buffer[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(read)
}
