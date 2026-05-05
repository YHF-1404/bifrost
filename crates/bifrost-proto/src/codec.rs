use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::ProtoError;
use crate::frame::Frame;
use crate::MAX_FRAME_LEN;

/// Length of the on-wire size header in bytes.
const HEADER_LEN: usize = 4;

/// `tokio_util::codec`-style codec for [`Frame`] values.
///
/// Encodes as `[u32 BE payload_len][postcard bytes…]`. The codec is
/// stateless apart from a configurable maximum payload length.
#[derive(Debug, Clone)]
pub struct FrameCodec {
    max_len: usize,
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameCodec {
    /// Build a codec that accepts payloads up to [`MAX_FRAME_LEN`].
    pub fn new() -> Self {
        Self {
            max_len: MAX_FRAME_LEN,
        }
    }

    /// Build a codec with a custom maximum payload size.
    ///
    /// Frames larger than this — on either encode or decode — return
    /// [`ProtoError::FrameTooLarge`].
    pub fn with_max_len(max: usize) -> Self {
        Self { max_len: max }
    }

    /// Maximum payload length this codec accepts.
    pub fn max_len(&self) -> usize {
        self.max_len
    }
}

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = ProtoError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, ProtoError> {
        // Need at least the length header.
        if src.len() < HEADER_LEN {
            return Ok(None);
        }
        // Peek at the length without consuming, so a partial body keeps
        // the header for the next attempt.
        let len = u32::from_be_bytes(src[..HEADER_LEN].try_into().unwrap()) as usize;
        if len > self.max_len {
            return Err(ProtoError::FrameTooLarge(len, self.max_len));
        }
        if src.len() < HEADER_LEN + len {
            // Reserve so the next read fills the buffer in one syscall.
            src.reserve(HEADER_LEN + len - src.len());
            return Ok(None);
        }
        // Header is fully validated — consume it now.
        src.advance(HEADER_LEN);
        let payload = src.split_to(len);
        let frame = postcard::from_bytes(&payload)?;
        Ok(Some(frame))
    }
}

/// Postcard `Flavor` that serializes directly into a `BytesMut`,
/// avoiding the intermediate `Vec` allocation that `to_allocvec` does.
///
/// On the bifrost upload hot path `perf record` showed
/// `FrameCodec::encode` taking ~10 % of all cycles — almost entirely
/// the per-frame allocation + copy from `to_allocvec`'s `Vec` into the
/// `BytesMut`. Writing straight into the destination eliminates both.
struct BytesMutFlavor<'a>(&'a mut BytesMut);

impl<'a> postcard::ser_flavors::Flavor for BytesMutFlavor<'a> {
    type Output = ();

    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.0.put_u8(byte);
        Ok(())
    }

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.0.put_slice(data);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<Self::Output> {
        Ok(())
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = ProtoError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), ProtoError> {
        // Reserve a placeholder length header, serialize the body
        // straight into `dst` via the no-alloc flavor, then back-patch
        // the header with the actual length. Truncating on overflow
        // restores `dst` exactly to its pre-encode state so a too-big
        // frame doesn't leave half-written bytes on the wire.
        let len_pos = dst.len();
        dst.reserve(HEADER_LEN);
        dst.put_u32(0); // placeholder; patched below
        let payload_start = dst.len();
        postcard::serialize_with_flavor::<_, _, ()>(&item, BytesMutFlavor(dst))?;
        let payload_len = dst.len() - payload_start;
        if payload_len > self.max_len {
            dst.truncate(len_pos);
            return Err(ProtoError::FrameTooLarge(payload_len, self.max_len));
        }
        let len_be = (payload_len as u32).to_be_bytes();
        dst[len_pos..len_pos + HEADER_LEN].copy_from_slice(&len_be);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;

    #[test]
    fn empty_buffer_returns_none() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn header_only_returns_none_and_preserves_buffer() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        buf.put_u32(100);
        assert!(codec.decode(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), HEADER_LEN);
    }

    #[test]
    fn declared_length_over_max_is_rejected() {
        let mut codec = FrameCodec::with_max_len(64);
        let mut buf = BytesMut::new();
        buf.put_u32(200);
        match codec.decode(&mut buf) {
            Err(ProtoError::FrameTooLarge(200, 64)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn ping_pong_minimal_roundtrip() {
        let mut codec = FrameCodec::new();
        let mut buf = BytesMut::new();
        codec.encode(Frame::Ping(7), &mut buf).unwrap();
        codec.encode(Frame::Pong(7), &mut buf).unwrap();
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), Frame::Ping(7));
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), Frame::Pong(7));
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }
}
