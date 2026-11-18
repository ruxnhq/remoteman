//! What travels beside the picture: sound and microphone packets (one datagram each), the
//! device's camera (cut into fragments like the picture), and blobs (clipboard contents and
//! files, each on a stream of its own).

use serde::{Deserialize, Serialize};

use crate::Error;

/// Host to client: the desktop's sound.
pub const KIND_SOUND: u8 = 3;
/// Client to host: the device's microphone.
pub const KIND_MIC: u8 = 4;
/// Client to host: the device's camera, in the picture's fragment layout (`video::Fragment`),
/// its epoch counting the times the camera was switched on.
pub const KIND_CAMERA: u8 = 5;

/// One packet of sound: a datagram of kind [`KIND_SOUND`] or [`KIND_MIC`].
///
/// ```text
/// u8   kind
/// u8   flags      bit 0: the first packet after a pause (the sender stops while there is
///                 nothing but silence): start the playout buffer afresh
/// u16  0
/// u32  seq        counts up by one per packet
/// u64  time_us    when it was captured, on the sender's session clock
/// ...  payload    one encoded packet (Opus)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    pub kind: u8,
    pub start: bool,
    pub seq: u32,
    pub time_us: u64,
    pub data: Vec<u8>,
}

impl Packet {
    pub const HEADER: usize = 16;

    pub fn write(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(Self::HEADER + self.data.len());
        d.push(self.kind);
        d.push(self.start as u8);
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&self.seq.to_le_bytes());
        d.extend_from_slice(&self.time_us.to_le_bytes());
        d.extend_from_slice(&self.data);
        d
    }

    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        if d.len() < Self::HEADER {
            return Err(Error::Truncated("sound datagram"));
        }
        Ok(Self {
            kind: d[0],
            start: d[1] & 1 != 0,
            seq: u32::from_le_bytes(d[4..8].try_into().unwrap()),
            time_us: u64::from_le_bytes(d[8..16].try_into().unwrap()),
            data: d[Self::HEADER..].to_vec(),
        })
    }
}

/// A blob is a one-way stream, opened by whichever side has something to send, that starts
/// with a kind byte, then a `u32` length and that many bytes of JSON (the header), then the
/// content to the end of the stream. A stream that is reset was given up by its sender.
///
/// The first byte also tells a blob from the client's input stream, which begins with an input
/// event's type (below 0x40).
pub const BLOB_CLIPBOARD: u8 = 0x40;
pub const BLOB_FILE: u8 = 0x41;
/// The first byte of every blob is at least this.
pub const BLOB_FIRST: u8 = 0x40;
/// The longest header taken.
pub const BLOB_HEADER_LIMIT: usize = 64 * 1024;
/// The largest clipboard content sent or taken.
pub const CLIPBOARD_LIMIT: u64 = 8 << 20;

/// The header of a clipboard blob: what was copied on the sending side, in one representation
/// (text if there is any, else an image).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardHeader {
    /// Counts up on the sending side; a blob with a lower number than the last taken is stale.
    pub seq: u32,
    /// `text/plain;charset=utf-8` or `image/png`.
    #[serde(rename = "type")]
    pub mime: String,
    pub size: u64,
}

/// The header of a file blob. The receiver decides where the file goes, never the sender, and
/// answers with `file_result` when it is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHeader {
    /// The sender's number for this transfer.
    pub id: u32,
    /// The file's name, with no folder. The receiver cleans it up all the same.
    pub name: String,
    pub size: u64,
}

/// A file name that is safe to create in a folder of the receiver's choosing: no path, no
/// leading dots, no control characters, not empty, and not absurdly long.
pub fn safe_file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last.chars().filter(|c| !c.is_control() && *c != ':').collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = cleaned.chars().take(200).collect();
    if out.is_empty() {
        out = "file".into();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_round_trip() {
        let p = Packet { kind: KIND_SOUND, start: true, seq: 7, time_us: 123_456_789, data: vec![1, 2, 3] };
        let d = p.write();
        assert_eq!(d.len(), Packet::HEADER + 3);
        assert_eq!(Packet::parse(&d).unwrap(), p);
        assert!(Packet::parse(&d[..10]).is_err());
    }

    #[test]
    fn headers_are_plain_json() {
        let h = ClipboardHeader { seq: 3, mime: "text/plain;charset=utf-8".into(), size: 5 };
        assert_eq!(serde_json::to_string(&h).unwrap(), r#"{"seq":3,"type":"text/plain;charset=utf-8","size":5}"#);
        let f: FileHeader = serde_json::from_str(r#"{"id":1,"name":"a.txt","size":9,"later":true}"#).unwrap();
        assert_eq!(f, FileHeader { id: 1, name: "a.txt".into(), size: 9 });
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\x\\report.pdf"), "report.pdf");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("a\nb\u{7}.txt"), "ab.txt");
        assert_eq!(safe_file_name("///"), "file");
        assert_eq!(safe_file_name(""), "file");
        assert_eq!(safe_file_name(&"x".repeat(500)).len(), 200);
    }
}
