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

/// Where the client's display sits among the host's own screens. The numbers are the C
/// interface's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// The main display: where the desktop's panel and new windows go.
    Main = 1,
    /// The only display: the host's own screens are off while the client is connected.
    Only = 2,
    /// No display of the client's own: it is shown the host's main monitor as it is, at that
    /// monitor's size, and its input lands there. A host that doesn't list `mirror` among
    /// its features reads this as an extra display.
    Mirror = 3,
    /// An extra display beside the host's own. Also what a placement this version doesn't know
    /// is read as.
    #[default]
    #[serde(other)]
    Extend = 0,
}

impl Placement {
    fn is_default(&self) -> bool {
        *self == Placement::Extend
    }
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
    #[serde(default, skip_serializing_if = "Placement::is_default")]
    pub placement: Placement,
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
        /// The client draws the cursor itself: leave it out of the picture and send its shape
        /// (`cursor`) and position (cursor datagrams) instead.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cursor: bool,
        /// The password of the host's user, for a host that is locked and answered an
        /// earlier hello with the error `locked` (docs/PROTOCOL.md, "A locked host").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unlock: Option<String>,
        /// How this device may be let in where there is a choice: left out, any way the host
        /// has; `"attach"`, only into a session that is already running (never a login
        /// screen). A host with one way in takes no notice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        way_in: Option<String>,
    },
    HostHello {
        proto: u32,
        name: String,
        os: String,
        features: Vec<String>,
    },
    /// What the video datagrams of `epoch` carry. `index` 1: the second display's picture
    /// (docs/PROTOCOL.md, "A second display"), whose epochs count by themselves.
    Stream {
        epoch: u16,
        #[serde(flatten)]
        mode: Mode,
        width: u32,
        height: u32,
        refresh: u32,
        colour: String,
        #[serde(default, skip_serializing_if = "is_zero8")]
        index: u8,
    },
    /// Client to host: add a second display of this size to the session (`index` 1, `on`
    /// true), or take it away. Host to client, with `on` false: the second display is gone
    /// (it could not be made, or the host took it away).
    Display {
        index: u8,
        on: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<Display>,
        /// Host to client: why it is gone.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    /// Client to host: the next frame of this display (not the first) must be a keyframe.
    DisplayKeyframe {
        index: u8,
    },
    /// The cursor's shape changed. `rgba` is base64 of `width * height * 4` bytes, premultiplied
    /// RGBA, rows top to bottom; the hotspot is in the image's pixels. `width` 0: no cursor
    /// (hidden, or it left this display).
    Cursor {
        width: u32,
        height: u32,
        hot_x: i32,
        hot_y: i32,
        #[serde(default)]
        rgba: String,
    },
    /// The client needs a keyframe.
    Keyframe,
    /// Frames `first` to `last` never arrived whole. The host answers with a frame flagged as a
    /// recovery point (one that leans on nothing from `first` on) or, failing that, a keyframe.
    Lost {
        first: u32,
        last: u32,
        /// The stream epoch the numbers belong to (older clients leave it out).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        epoch: Option<u16>,
    },
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

    // What follows is the features beyond the picture and input. A host lists the ones it has
    // in its hello; nothing of a feature is sent to a side that hasn't asked for it, so a
    // client or host that knows nothing of it is never handed something it can't read.
    /// Client to host: send the desktop's sound (sound datagrams), or stop.
    Sound {
        on: bool,
    },
    /// Host to client, in place of the hello for a moment: this device is new here, and the
    /// host's owner has this many seconds to confirm it. Both sides can show the same code
    /// meanwhile (docs/PROTOCOL.md, "Pairing"); the hello, or an error, follows.
    Pairing {
        seconds: u32,
    },
    /// Host to client: what the sound datagrams carry, before the first one and on any change.
    SoundFormat {
        /// `opus`.
        codec: String,
        rate: u32,
        channels: u8,
        /// How much sound one packet holds, in microseconds.
        frame_us: u32,
    },
    /// Client to host: this device's microphone is being sent (microphone datagrams), as
    /// described, or has stopped.
    Mic {
        on: bool,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        codec: String,
        #[serde(default, skip_serializing_if = "is_zero")]
        rate: u32,
        #[serde(default, skip_serializing_if = "is_zero8")]
        channels: u8,
        #[serde(default, skip_serializing_if = "is_zero")]
        frame_us: u32,
    },
    /// Client to host: this device's camera is being sent (camera datagrams), as described, or
    /// has stopped.
    Camera {
        on: bool,
        /// `h264` or `hevc`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        codec: String,
        #[serde(default, skip_serializing_if = "is_zero")]
        width: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        height: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        fps: u32,
    },
    /// Host to client: the camera's next frame must be a keyframe.
    CameraKeyframe,
    /// Client to host: change what was asked for in the hello. Whatever is left out stays as it
    /// is. A new display or other decoders start a new stream epoch.
    Configure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<Display>,
        /// The most the picture may use, in bits a second. 0: the host's own choice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bitrate: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decoders: Option<Vec<Mode>>,
    },
    /// Host to client: what the host can do changed while connected (its owner switched
    /// something on or off). It replaces the list in the hello.
    Features { features: Vec<String> },
    /// Host to client, now and then: how the stream stands, for the device to show.
    Status {
        /// What the picture is being encoded at, in bits a second.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bitrate: Option<u32>,
    },
    /// Client to host: share the clipboard, both ways, or stop. While on, each side sends what
    /// is copied on it as a clipboard blob.
    Clipboard {
        on: bool,
    },
    /// The receiver of a file blob, once it has it or has given up: `id` is the blob header's.
    FileResult {
        id: u32,
        ok: bool,
        /// Why not; or where it was put, for showing.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        message: String,
    },
    /// Anything this version doesn't know. Never sent.
    #[serde(other)]
    Unknown,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero8(n: &u8) -> bool {
    *n == 0
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

/// Base64, as `cursor` carries its image.
pub fn encode_bytes(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_bytes(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(text).ok()
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
        let Message::ClientHello { display, decoders, cursor, .. } = &m else { panic!("{m:?}") };
        assert!(!cursor);
        assert_eq!((display.width, display.refresh, display.placement), (2752, 120, Placement::Extend));
        let only = Message::parse(br#"{"t":"client_hello","proto":0,"name":"x","display":{"width":2,"height":2,"scale":1.0,"refresh":60,"placement":"only"},"decoders":[]}"#).unwrap();
        assert!(matches!(only, Message::ClientHello { display: Display { placement: Placement::Only, .. }, .. }));
        let mirror = Message::parse(br#"{"t":"client_hello","proto":0,"name":"x","display":{"width":2,"height":2,"scale":1.0,"refresh":60,"placement":"mirror"},"decoders":[]}"#).unwrap();
        assert!(matches!(mirror, Message::ClientHello { display: Display { placement: Placement::Mirror, .. }, .. }));
        // A placement this version doesn't know is an extra display.
        let newer = Message::parse(br#"{"t":"client_hello","proto":0,"name":"x","display":{"width":2,"height":2,"scale":1.0,"refresh":60,"placement":"sideways"},"decoders":[]}"#).unwrap();
        assert!(matches!(newer, Message::ClientHello { display: Display { placement: Placement::Extend, .. }, .. }));
        assert_eq!(decoders[1], Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 });
        assert_eq!(serde_json::to_string(&m).unwrap(), json);
    }

    #[test]
    fn stream_is_flat_and_unknowns_are_ignored() {
        let m = Message::Stream { epoch: 1, mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: 2752, height: 2064, refresh: 120, colour: "bt709".into(), index: 0 };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"t":"stream","epoch":1,"codec":"av1","chroma":"420","depth":10,"width":2752,"height":2064,"refresh":120,"colour":"bt709"}"#
        );
        assert_eq!(Message::parse(br#"{"t":"something_new","x":1}"#).unwrap(), Message::Unknown);
        assert_eq!(Message::parse(br#"{"t":"keyframe","later_field":true}"#).unwrap(), Message::Keyframe);
    }

    #[test]
    fn feature_messages_leave_out_what_is_not_said() {
        assert_eq!(serde_json::to_string(&Message::Sound { on: true }).unwrap(), r#"{"t":"sound","on":true}"#);
        let off = Message::Mic { on: false, codec: String::new(), rate: 0, channels: 0, frame_us: 0 };
        assert_eq!(serde_json::to_string(&off).unwrap(), r#"{"t":"mic","on":false}"#);
        assert_eq!(Message::parse(br#"{"t":"mic","on":false}"#).unwrap(), off);
        let c = Message::Configure { display: None, bitrate: Some(25_000_000), decoders: None };
        assert_eq!(serde_json::to_string(&c).unwrap(), r#"{"t":"configure","bitrate":25000000}"#);
        assert_eq!(Message::parse(br#"{"t":"configure","bitrate":25000000}"#).unwrap(), c);
        assert_eq!(Message::parse(br#"{"t":"camera_keyframe"}"#).unwrap(), Message::CameraKeyframe);
        assert_eq!(serde_json::to_string(&Message::FileResult { id: 4, ok: true, message: String::new() }).unwrap(), r#"{"t":"file_result","id":4,"ok":true}"#);
    }

    #[test]
    fn framing() {
        let f = Message::Keyframe.frame();
        assert_eq!(body_len(f[..4].try_into().unwrap()).unwrap(), f.len() - 4);
        assert_eq!(Message::parse(&f[4..]).unwrap(), Message::Keyframe);
        assert!(body_len((u32::MAX).to_le_bytes()).is_err());
    }
}
