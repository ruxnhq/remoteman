//! Control messages: JSON objects with a `"t"` field, each framed by a `u32` length.

use serde::{Deserialize, Serialize};

use crate::Error;

/// The longest control message accepted.
pub const MAX_MESSAGE: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Chroma {
    #[serde(rename = "420")]
    C420,
    #[serde(rename = "444")]
    C444,
}

/// One way of coding the picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub codec: Codec,
    pub chroma: Chroma,
    pub depth: u8,
}

/// A client's screen.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub width: u32,
    pub height: u32,
    /// How many pixels per point the desktop should be drawn at.
    pub scale: f32,
    /// In Hz.
    pub refresh: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Message {
    ClientHello {
        proto: u32,
        name: String,
        display: Display,
        /// What the client decodes in hardware, most preferred first.
        decoders: Vec<Mode>,
    },
    HostHello {
        proto: u32,
        name: String,
        os: String,
        features: Vec<String>,
    },
    /// What the video datagrams of `epoch` carry.
    Stream {
        epoch: u16,
        #[serde(flatten)]
        mode: Mode,
        width: u32,
        height: u32,
        refresh: u32,
        colour: String,
    },
    /// The client needs a keyframe.
    Keyframe,
    Ping {
        id: u32,
        client_us: u64,
    },
    Pong {
        id: u32,
        client_us: u64,
        host_us: u64,
    },
    Error {
        code: String,
        message: String,
    },
    /// Anything this version doesn't know. Never sent.
    #[serde(other)]
    Unknown,
}

impl Message {
    /// The message with its length in front, ready to write to the control stream.
    pub fn frame(&self) -> Vec<u8> {
        let body = serde_json::to_vec(self).expect("control messages always serialize");
        let mut out = Vec::with_capacity(body.len() + 4);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Parse one message body (what follows the length).
    pub fn parse(body: &[u8]) -> Result<Self, Error> {
        Ok(serde_json::from_slice(body)?)
    }
}

/// Check a length prefix before reading the body.
pub fn body_len(prefix: [u8; 4]) -> Result<usize, Error> {
    let n = u32::from_le_bytes(prefix) as usize;
    if n > MAX_MESSAGE { Err(Error::TooLong(n)) } else { Ok(n) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_matches_the_spec() {
        let json = r#"{"t":"client_hello","proto":0,"name":"Anna's iPad","display":{"width":2752,"height":2064,"scale":2.0,"refresh":120},"decoders":[{"codec":"av1","chroma":"420","depth":10},{"codec":"hevc","chroma":"444","depth":10}]}"#;
        let m = Message::parse(json.as_bytes()).unwrap();
        let Message::ClientHello { display, decoders, .. } = &m else { panic!("{m:?}") };
        assert_eq!((display.width, display.refresh), (2752, 120));
        assert_eq!(decoders[1], Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 });
        assert_eq!(serde_json::to_string(&m).unwrap(), json);
    }

    #[test]
    fn stream_is_flat_and_unknowns_are_ignored() {
        let m = Message::Stream { epoch: 1, mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: 2752, height: 2064, refresh: 120, colour: "bt709".into() };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"t":"stream","epoch":1,"codec":"av1","chroma":"420","depth":10,"width":2752,"height":2064,"refresh":120,"colour":"bt709"}"#
        );
        assert_eq!(Message::parse(br#"{"t":"something_new","x":1}"#).unwrap(), Message::Unknown);
        assert_eq!(Message::parse(br#"{"t":"keyframe","later_field":true}"#).unwrap(), Message::Keyframe);
    }

    #[test]
    fn framing() {
        let f = Message::Keyframe.frame();
        assert_eq!(body_len(f[..4].try_into().unwrap()).unwrap(), f.len() - 4);
        assert_eq!(Message::parse(&f[4..]).unwrap(), Message::Keyframe);
        assert!(body_len((u32::MAX).to_le_bytes()).is_err());
    }
}
