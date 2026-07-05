//! Length-prefixed framing for the IPC channel.
//!
//! Each frame is a `u32` little-endian length followed by exactly that
//! many UTF-8 bytes of JSON. Frames are bounded at
//! [`vapor_shared::constants::ipc::MAX_PAYLOAD_BYTES`]; the reader fails
//! the connection rather than allocating an oversized buffer.

use std::error::Error;
use std::fmt::{self, Display};
use std::io::{self, Read, Write};

use vapor_shared::constants;

#[derive(Debug)]
pub enum FrameError {
    /// Underlying I/O failed mid-frame.
    Io(io::Error),
    /// The peer announced a frame larger than
    /// [`MAX_PAYLOAD_BYTES`](constants::ipc::MAX_PAYLOAD_BYTES). The
    /// reader must close the connection rather than allocate.
    OversizedFrame { declared: u32, max: u32 },
    /// The peer closed the connection before sending a complete frame.
    UnexpectedEof,
}

impl Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "frame I/O error: {error}"),
            Self::OversizedFrame { declared, max } => {
                write!(f, "frame declares {declared} bytes (max {max})")
            }
            Self::UnexpectedEof => write!(f, "unexpected EOF mid-frame"),
        }
    }
}

impl Error for FrameError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::UnexpectedEof => Self::UnexpectedEof,
            _ => Self::Io(error),
        }
    }
}

/// Reads a single length-prefixed frame from `reader`.
///
/// Returns `Ok(Some(payload))` for a complete frame and `Ok(None)` when
/// the peer closed the connection cleanly **at a frame boundary** (EOF
/// before any prefix byte). EOF in the middle of a prefix or payload is a
/// protocol error and surfaces as [`FrameError::UnexpectedEof`], so
/// truncation is distinguishable from a polite hangup in diagnostics.
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len_buf = [0u8; 4];
    let mut filled = 0usize;
    while filled < len_buf.len() {
        match reader.read(&mut len_buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::UnexpectedEof),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }

    let declared = u32::from_le_bytes(len_buf);
    if (declared as usize) > constants::ipc::MAX_PAYLOAD_BYTES {
        return Err(FrameError::OversizedFrame {
            declared,
            max: constants::ipc::MAX_PAYLOAD_BYTES as u32,
        });
    }
    let mut payload = vec![0u8; declared as usize];
    reader.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Writes a single length-prefixed frame to `writer`.
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError> {
    if payload.len() > constants::ipc::MAX_PAYLOAD_BYTES {
        return Err(FrameError::OversizedFrame {
            declared: payload.len() as u32,
            max: constants::ipc::MAX_PAYLOAD_BYTES as u32,
        });
    }
    let length = payload.len() as u32;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_frame_through_in_memory_buffer() {
        let payload = br#"{"hello":"world"}"#.to_vec();
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &payload).expect("write");
        let mut reader = Cursor::new(buffer);
        let decoded = read_frame(&mut reader).expect("read").expect("frame");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn eof_at_frame_boundary_is_a_clean_close() {
        let mut reader = Cursor::new(Vec::new());
        assert!(read_frame(&mut reader).expect("clean eof").is_none());
    }

    #[test]
    fn eof_inside_length_prefix_is_a_protocol_error() {
        // Two of the four prefix bytes, then EOF: truncation, not hangup.
        let mut reader = Cursor::new(vec![0x10, 0x00]);
        let error = read_frame(&mut reader).expect_err("mid-prefix eof");
        assert!(matches!(error, FrameError::UnexpectedEof));
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let mut buffer = Vec::new();
        // Manually craft a frame whose declared length exceeds the cap.
        let bogus_length = (constants::ipc::MAX_PAYLOAD_BYTES as u32) + 1;
        buffer.extend_from_slice(&bogus_length.to_le_bytes());
        // We don't even need to fill the payload — the reader must reject
        // before trying to allocate.
        let mut reader = Cursor::new(buffer);
        let error = read_frame(&mut reader).expect_err("oversized");
        assert!(matches!(error, FrameError::OversizedFrame { .. }));
    }

    #[test]
    fn unexpected_eof_is_classified_as_unexpected_eof_error() {
        // 4 bytes of length prefix saying 16 bytes follow, but only 4 bytes
        // are actually available.
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&16u32.to_le_bytes());
        buffer.extend_from_slice(&[0xAB; 4]);
        let mut reader = Cursor::new(buffer);
        let error = read_frame(&mut reader).expect_err("eof");
        assert!(matches!(error, FrameError::UnexpectedEof));
    }

    #[test]
    fn write_frame_rejects_oversized_payload_without_writing_anything() {
        let payload = vec![0u8; constants::ipc::MAX_PAYLOAD_BYTES + 1];
        let mut buffer = Vec::new();
        let error = write_frame(&mut buffer, &payload).expect_err("oversized write");
        assert!(matches!(error, FrameError::OversizedFrame { .. }));
        assert!(buffer.is_empty());
    }
}
