//! Video on the wire: each frame is cut into fragments, one per datagram, and put together again
//! on the other side.

use std::collections::BTreeMap;

use crate::Error;

pub const KIND_VIDEO: u8 = 1;
pub const KIND_CURSOR: u8 = 2;

/// Where the host's cursor is: a datagram of its own, sent when it moves (only to clients that
/// draw the cursor themselves).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorAt {
    pub epoch: u16,
    /// False: the cursor isn't on this display.
    pub visible: bool,
    /// Fractions (0 to 1) of the display from its top left.
    pub x: f32,
    pub y: f32,
}

impl CursorAt {
    pub const LEN: usize = 12;

    pub fn write(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(Self::LEN);
        d.push(KIND_CURSOR);
        d.push(self.visible as u8);
        d.extend_from_slice(&self.epoch.to_le_bytes());
        d.extend_from_slice(&self.x.to_le_bytes());
        d.extend_from_slice(&self.y.to_le_bytes());
        d
    }

    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        if d.len() < Self::LEN || d[0] != KIND_CURSOR {
            return Err(Error::Truncated("cursor datagram"));
        }
        Ok(Self {
            visible: d[1] & 1 != 0,
            epoch: u16::from_le_bytes([d[2], d[3]]),
            x: f32::from_le_bytes(d[4..8].try_into().unwrap()),
            y: f32::from_le_bytes(d[8..12].try_into().unwrap()),
        })
    }
}
pub const HEADER: usize = 20;
const FLAG_KEY: u8 = 1;
const FLAG_RECOVERY: u8 = 2;

/// The header of one video datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    pub key: bool,
    /// The first frame after a reported loss that leans on nothing the client missed.
    pub recovery: bool,
    pub epoch: u16,
    pub frame: u32,
    pub index: u16,
    pub count: u16,
    pub capture_us: u64,
}

impl Fragment {
    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(KIND_VIDEO);
        out.push(if self.key { FLAG_KEY } else { 0 } | if self.recovery { FLAG_RECOVERY } else { 0 });
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.capture_us.to_le_bytes());
    }

    /// Split a datagram into its header and payload.
    pub fn parse(d: &[u8]) -> Result<(Self, &[u8]), Error> {
        if d.len() < HEADER {
            return Err(Error::Truncated("video datagram"));
        }
        if d[0] != KIND_VIDEO {
            return Err(Error::Unknown("datagram kind", d[0] as u32));
        }
        let u16_at = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]);
        let f = Self {
            key: d[1] & FLAG_KEY != 0,
            recovery: d[1] & FLAG_RECOVERY != 0,
            epoch: u16_at(2),
            frame: u32::from_le_bytes(d[4..8].try_into().unwrap()),
            index: u16_at(8),
            count: u16_at(10),
            capture_us: u64::from_le_bytes(d[12..20].try_into().unwrap()),
        };
        if f.count == 0 || f.index >= f.count {
            return Err(Error::Truncated("fragment numbering"));
        }
        Ok((f, &d[HEADER..]))
    }
}

/// Cut `frame` into datagrams of at most `max` bytes each (header included).
pub fn fragment(frame: &[u8], key: bool, recovery: bool, epoch: u16, number: u32, capture_us: u64, max: usize) -> Vec<Vec<u8>> {
    let room = max.saturating_sub(HEADER).max(1);
    let count = frame.len().div_ceil(room).max(1);
    assert!(count <= u16::MAX as usize, "frame too large for the fragment counter");
    (0..count)
        .map(|i| {
            let part = &frame[(i * room).min(frame.len())..((i + 1) * room).min(frame.len())];
            let mut d = Vec::with_capacity(HEADER + part.len());
            Fragment { key, recovery, epoch, frame: number, index: i as u16, count: count as u16, capture_us }.write(&mut d);
            d.extend_from_slice(part);
            d
        })
        .collect()
}

/// A whole frame, put back together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub key: bool,
    /// See [`Fragment::recovery`].
    pub recovery: bool,
    pub epoch: u16,
    pub number: u32,
    pub capture_us: u64,
    pub data: Vec<u8>,
}

struct Partial {
    key: bool,
    recovery: bool,
    capture_us: u64,
    parts: Vec<Option<Vec<u8>>>,
    have: usize,
}

/// Puts fragments back into frames. Frames come out in order; a frame that is still incomplete
/// when a later one completes is given up and counted as lost.
#[derive(Default)]
pub struct Reassembler {
    epoch: u16,
    partial: BTreeMap<u32, Partial>,
    /// The number after the last frame delivered or given up.
    next: u32,
    /// Frames given up since the last call to [`Self::take_lost`]: the first and last.
    lost: Option<(u32, u32)>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new epoch: everything pending is dropped.
    pub fn reset(&mut self, epoch: u16) {
        self.epoch = epoch;
        self.partial.clear();
        self.next = 0;
    }

    /// The frames given up since this was last asked, as the first and last of them.
    pub fn take_lost(&mut self) -> Option<(u32, u32)> {
        self.lost.take()
    }

    /// Take in one datagram; returns a frame if this completed one.
    pub fn push(&mut self, datagram: &[u8]) -> Result<Option<Frame>, Error> {
        let (f, payload) = Fragment::parse(datagram)?;
        if f.epoch != self.epoch || f.frame < self.next {
            return Ok(None);
        }
        let p = self.partial.entry(f.frame).or_insert_with(|| Partial { key: f.key, recovery: f.recovery, capture_us: f.capture_us, parts: vec![None; f.count as usize], have: 0 });
        if p.parts.len() != f.count as usize {
            return Err(Error::Truncated("fragment count changed within a frame"));
        }
        let slot = &mut p.parts[f.index as usize];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            p.have += 1;
        }
        if p.have < p.parts.len() {
            return Ok(None);
        }
        let p = self.partial.remove(&f.frame).unwrap();
        // Anything older that is still incomplete is given up: later frames don't wait.
        self.partial.retain(|&n, _| n > f.frame);
        if f.frame > self.next {
            self.lost = Some((self.lost.map_or(self.next, |(first, _)| first), f.frame - 1));
        }
        self.next = f.frame + 1;
        let data = p.parts.into_iter().flat_map(|part| part.unwrap()).collect();
        Ok(Some(Frame { key: p.key, recovery: p.recovery, epoch: f.epoch, number: f.frame, capture_us: p.capture_us, data }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn cursor_datagram() {
        let c = CursorAt { epoch: 3, visible: true, x: 0.25, y: 0.5 };
        let d = c.write();
        assert_eq!(d, [2, 1, 3, 0, 0, 0, 0x80, 0x3e, 0, 0, 0, 0x3f]);
        assert_eq!(CursorAt::parse(&d).unwrap(), c);
        assert!(CursorAt::parse(&d[..5]).is_err());
    }

    #[test]
    fn header_layout() {
        let mut d = Vec::new();
        Fragment { key: true, recovery: false, epoch: 0x0102, frame: 0x03040506, index: 7, count: 9, capture_us: 0x1112131415161718 }.write(&mut d);
        assert_eq!(d, [1, 1, 2, 1, 6, 5, 4, 3, 7, 0, 9, 0, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11]);
        assert_eq!(d.len(), HEADER);
    }

    #[test]
    fn round_trip_in_any_order() {
        let frame = bytes(5000, 3);
        let mut parts = fragment(&frame, true, false, 0, 0, 42, 1200);
        assert_eq!(parts.len(), 5);
        assert!(parts.iter().all(|p| p.len() <= 1200));
        parts.reverse();
        let mut r = Reassembler::new();
        let mut out = None;
        for p in &parts {
            out = r.push(p).unwrap();
        }
        let f = out.unwrap();
        assert_eq!((f.key, f.number, f.capture_us), (true, 0, 42));
        assert_eq!(f.data, frame);
        assert_eq!(r.take_lost(), None);
    }

    #[test]
    fn an_incomplete_frame_is_given_up_when_a_later_one_completes() {
        let mut r = Reassembler::new();
        let a = fragment(&bytes(3000, 1), true, false, 0, 0, 0, 1200);
        let b = fragment(&bytes(3000, 2), false, false, 0, 1, 0, 1200);
        let c = fragment(&bytes(100, 3), false, true, 0, 2, 0, 1200);
        for p in &a {
            r.push(p).unwrap();
        }
        r.push(&b[0]).unwrap(); // the rest of b never arrives
        let got = r.push(&c[0]).unwrap().unwrap();
        assert_eq!((got.number, got.recovery), (2, true));
        assert_eq!(r.take_lost(), Some((1, 1)));
        // A late fragment of b is ignored.
        assert_eq!(r.push(&b[1]).unwrap(), None);
    }

    #[test]
    fn other_epochs_are_dropped() {
        let mut r = Reassembler::new();
        r.reset(2);
        let old = fragment(&bytes(10, 1), true, false, 1, 0, 0, 1200);
        assert_eq!(r.push(&old[0]).unwrap(), None);
        let new = fragment(&bytes(10, 1), true, false, 2, 0, 0, 1200);
        assert!(r.push(&new[0]).unwrap().is_some());
    }

    #[test]
    fn an_empty_frame_is_one_fragment() {
        assert_eq!(fragment(&[], false, false, 0, 0, 0, 1200).len(), 1);
    }
}
