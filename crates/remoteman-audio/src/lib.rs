//! Sound for the Linux host and the Linux viewer: Opus packets on one side, PipeWire nodes on the
//! other.
//!
//! The host's speaker is a sink node that desktop sound is sent to and that hands it on as
//! packets ([`SpeakerSink`]); its microphone is a source node fed from packets
//! ([`MicSource`]). A viewer plays packets on its own output ([`Player`]) and turns its own
//! microphone into packets ([`MicCapture`]). [`DefaultSink`] makes the host's sink the one the
//! desktop plays to while a client is connected.
//!
//! Everything is 48 kHz in 10 ms frames: stereo for the speaker, mono for the microphone. Each
//! node runs a PipeWire loop on a thread of its own, and is gone from the graph when it is
//! dropped.
//!
//! On Windows there is no PipeWire: only the Opus half builds ([`Encoder`], [`Decoder`],
//! [`Framer`], [`JitterBuffer`]), which the Windows viewer feeds from WASAPI.

mod codec;
#[cfg(not(windows))]
mod default;
mod frame;
mod jitter;
#[cfg(not(windows))]
mod node;
#[doc(hidden)]
pub mod testing;

pub use codec::{Decoder, Encoder};
#[cfg(not(windows))]
pub use default::{DefaultSink, default_sink};
pub use frame::Framer;
pub use jitter::{JITTER_FRAMES, JitterBuffer, JitterStats};
#[cfg(not(windows))]
pub use node::{MicCapture, MicSource, Node, Player, SpeakerSink};

/// Samples per second, the only rate used.
pub const SAMPLE_RATE: u32 = 48_000;

/// Samples of one channel in a frame: 10 ms. One packet is one frame.
pub const FRAME_SAMPLES: usize = 480;

/// How long a frame lasts, in microseconds.
pub const FRAME_US: u64 = 10_000;

/// One frame of sound as it crosses the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// Counts the packets of one sender, wrapping around. It has no hole where the sender
    /// paused, so a hole always means packets were lost.
    pub seq: u32,
    /// When the first sample of the frame was captured, in microseconds on the clock the
    /// capturing side was given.
    pub capture_us: u64,
    /// One Opus packet.
    pub data: Vec<u8>,
}

/// What is carried: the speaker in stereo, the microphone in mono.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    pub fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }

    /// The bitrate an [`Encoder`] starts at, in bits per second.
    pub fn default_bitrate(self) -> u32 {
        match self {
            Channels::Mono => 48_000,
            Channels::Stereo => 128_000,
        }
    }
}
