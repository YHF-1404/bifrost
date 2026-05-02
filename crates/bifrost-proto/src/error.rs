use thiserror::Error;

/// Failure modes of the Bifrost protocol codec.
#[derive(Debug, Error)]
pub enum ProtoError {
    /// The declared payload length exceeds the codec's configured maximum.
    ///
    /// `(declared, max)`.
    #[error("frame too large: declared {0} bytes, max allowed {1}")]
    FrameTooLarge(usize, usize),

    /// Postcard failed to (de)serialize the payload.
    #[error("postcard codec: {0}")]
    Postcard(#[from] postcard::Error),

    /// Underlying transport I/O error.
    ///
    /// `From<io::Error>` is required by `tokio_util::codec::Encoder`/`Decoder`.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
