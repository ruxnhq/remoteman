//! The remoteman wire format, version 0 (docs/PROTOCOL.md): control messages, video fragments
//! and input events. Types and framing only; no I/O, so every client and host shares it.

pub mod control;
pub mod input;
pub mod keys;
pub mod media;
pub mod video;

/// The protocol version this crate speaks.
pub const PROTO: u32 = 0;
/// The QUIC ALPN for that version.
pub const ALPN: &[u8] = b"remoteman/0";
/// The default UDP port.
pub const PORT: u16 = 7791;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("truncated {0}")]
    Truncated(&'static str),
    #[error("unknown {0} {1}")]
    Unknown(&'static str, u32),
    #[error("message of {0} bytes is over the limit")]
    TooLong(usize),
    #[error("bad JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("bad text in an input event")]
    Text,
}
