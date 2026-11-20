//! Video on the wire: each frame is cut into fragments, one per datagram, and put together again
//! on the other side.

use std::collections::BTreeMap;

use crate::Error;

pub const KIND_VIDEO: u8 = 1;
pub const KIND_CURSOR: u8 = 2;
/// A second display's picture: laid out as `KIND_VIDEO`, with epochs and frame numbers of its own.
pub const KIND_VIDEO2: u8 = 6;
/// Parity for a group of the first picture's fragments (docs/PROTOCOL.md, "Parity"), sent only to
/// a client that asked for it with `"parity":true` in its hello.
pub const KIND_PARITY: u8 = 7;
/// The fragments one parity datagram covers, as the core's host sends them.
pub const PARITY_GROUP: usize = 10;

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
        self.write_as(KIND_VIDEO, out);
    }

    /// As [`Self::write`], for another stream in the same layout (the device's camera).
    pub fn write_as(&self, kind: u8, out: &mut Vec<u8>) {
        out.push(kind);
        out.push(if self.key { FLAG_KEY } else { 0 } | if self.recovery { FLAG_RECOVERY } else { 0 });
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.capture_us.to_le_bytes());
    }

    /// Split a datagram into its header and payload.
    pub fn parse(d: &[u8]) -> Result<(Self, &[u8]), Error> {
        Self::parse_as(KIND_VIDEO, d)
    }

    /// As [`Self::parse`], for another stream in the same layout.
    pub fn parse_as(kind: u8, d: &[u8]) -> Result<(Self, &[u8]), Error> {
        if d.len() < HEADER {
            return Err(Error::Truncated("video datagram"));
        }
        if d[0] != kind {
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
    fragment_as(KIND_VIDEO, frame, key, recovery, epoch, number, capture_us, max)
}

/// As [`fragment`], for another stream in the same layout (the device's camera).
#[allow(clippy::too_many_arguments)]
pub fn fragment_as(kind: u8, frame: &[u8], key: bool, recovery: bool, epoch: u16, number: u32, capture_us: u64, max: usize) -> Vec<Vec<u8>> {
    let room = max.saturating_sub(HEADER).max(1);
    let count = frame.len().div_ceil(room).max(1);
    assert!(count <= u16::MAX as usize, "frame too large for the fragment counter");
    (0..count)
        .map(|i| {
            let part = &frame[(i * room).min(frame.len())..((i + 1) * room).min(frame.len())];
            let mut d = Vec::with_capacity(HEADER + part.len());
            Fragment { key, recovery, epoch, frame: number, index: i as u16, count: count as u16, capture_us }.write_as(kind, &mut d);
            d.extend_from_slice(part);
            d
        })
        .collect()
}

/// The header of one parity datagram: the XOR of the payloads of `n` fragments of a frame, the
/// ones from `first` on (shorter ones padded with zeros to the longest, which is the parity's own
/// length). One missing fragment of such a group is the XOR of the parity and the others; its
/// length is the parity's, unless it is the frame's last, whose length is `last_len`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parity {
    pub key: bool,
    pub recovery: bool,
    pub epoch: u16,
    pub frame: u32,
    pub first: u16,
    /// The frame's fragment count.
    pub count: u16,
    pub n: u8,
    /// The payload length of the frame's last fragment.
    pub last_len: u16,
    pub capture_us: u64,
}

pub const PARITY_HEADER: usize = 24;

impl Parity {
    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(KIND_PARITY);
        out.push(if self.key { FLAG_KEY } else { 0 } | if self.recovery { FLAG_RECOVERY } else { 0 });
        out.extend_from_slice(&self.epoch.to_le_bytes());
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&self.first.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.push(self.n);
        out.push(0);
        out.extend_from_slice(&self.last_len.to_le_bytes());
        out.extend_from_slice(&self.capture_us.to_le_bytes());
    }

    pub fn parse(d: &[u8]) -> Result<(Self, &[u8]), Error> {
        if d.len() < PARITY_HEADER {
            return Err(Error::Truncated("parity datagram"));
        }
        if d[0] != KIND_PARITY {
            return Err(Error::Unknown("datagram kind", d[0] as u32));
        }
        let u16_at = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]);
        let p = Self {
            key: d[1] & FLAG_KEY != 0,
            recovery: d[1] & FLAG_RECOVERY != 0,
            epoch: u16_at(2),
            frame: u32::from_le_bytes(d[4..8].try_into().unwrap()),
            first: u16_at(8),
            count: u16_at(10),
            n: d[12],
            last_len: u16_at(14),
            capture_us: u64::from_le_bytes(d[16..24].try_into().unwrap()),
        };
        let len = d.len() - PARITY_HEADER;
        if p.count == 0 || p.n == 0 || p.first as usize + p.n as usize > p.count as usize || p.last_len as usize > len {
            return Err(Error::Truncated("parity numbering"));
        }
        Ok((p, &d[PARITY_HEADER..]))
    }
}

/// The size the data datagrams of a frame are cut to when parity goes with them, so that the
/// parity datagram (a longer header) fits the same limit `max`. A copy of the frame sent again
/// with [`fragment_as`] must be cut to this size too.
pub fn parity_cut(max: usize) -> usize {
    max.saturating_sub(PARITY_HEADER - HEADER).max(HEADER + 1)
}

/// As [`fragment`], with a parity datagram after each group of `group` data fragments (and after
/// the last, partial group). The datagrams are the same size at most: the data fragments give up
/// the difference between the two headers.
#[allow(clippy::too_many_arguments)]
pub fn fragment_with_parity(frame: &[u8], key: bool, recovery: bool, epoch: u16, number: u32, capture_us: u64, max: usize, group: usize) -> Vec<Vec<u8>> {
    let group = group.clamp(1, 255);
    let data = fragment(frame, key, recovery, epoch, number, capture_us, parity_cut(max));
    let count = data.len();
    let last_len = data[count - 1].len() - HEADER;
    let mut out = Vec::with_capacity(count + count.div_ceil(group));
    for (g, chunk) in data.chunks(group).enumerate() {
        let len = chunk.iter().map(|d| d.len() - HEADER).max().unwrap_or(0);
        let mut x = vec![0u8; len];
        for d in chunk {
            for (a, b) in x.iter_mut().zip(&d[HEADER..]) {
                *a ^= b;
            }
        }
        let mut p = Vec::with_capacity(PARITY_HEADER + len);
        Parity { key, recovery, epoch, frame: number, first: (g * group) as u16, count: count as u16, n: chunk.len() as u8, last_len: last_len as u16, capture_us }.write(&mut p);
        p.extend_from_slice(&x);
        out.extend(chunk.iter().cloned());
        out.push(p);
    }
    out
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

struct Group {
    first: usize,
    n: usize,
    last_len: usize,
    data: Vec<u8>,
}

struct Partial {
    key: bool,
    recovery: bool,
    capture_us: u64,
    parts: Vec<Option<Vec<u8>>>,
    have: usize,
    /// The parity that has come for this frame, one per group.
    parity: Vec<Group>,
}

impl Partial {
    fn new(key: bool, recovery: bool, capture_us: u64, count: usize) -> Self {
        Self { key, recovery, capture_us, parts: vec![None; count], have: 0, parity: Vec::new() }
    }

    /// Rebuild every fragment that is the only one missing from a group whose parity is in.
    /// Returns how many were rebuilt.
    fn repair(&mut self) -> u64 {
        let count = self.parts.len();
        let mut rebuilt = 0;
        let mut i = 0;
        while i < self.parity.len() {
            let g = &self.parity[i];
            let missing: Vec<usize> = (g.first..g.first + g.n).filter(|&k| self.parts[k].is_none()).collect();
            if missing.is_empty() {
                // Nothing more to be had from it.
                self.parity.swap_remove(i);
                continue;
            }
            if missing.len() == 1 {
                let m = missing[0];
                let len = if m == count - 1 { g.last_len } else { g.data.len() };
                let mut x = g.data.clone();
                // A group that doesn't add up (a longer fragment than its parity) is not trusted.
                let fits = len <= x.len() && (g.first..g.first + g.n).filter(|&k| k != m).all(|k| self.parts[k].as_ref().is_some_and(|p| p.len() <= x.len()));
                if fits {
                    for k in (g.first..g.first + g.n).filter(|&k| k != m) {
                        for (a, b) in x.iter_mut().zip(self.parts[k].as_ref().unwrap()) {
                            *a ^= b;
                        }
                    }
                    x.truncate(len);
                    self.parts[m] = Some(x);
                    self.have += 1;
                    rebuilt += 1;
                }
                self.parity.swap_remove(i);
                continue;
            }
            i += 1;
        }
        rebuilt
    }
}

/// Puts fragments back into frames. Frames come out in order; a frame that is still incomplete
/// when a later one completes is given up and counted as lost.
pub struct Reassembler {
    kind: u8,
    epoch: u16,
    partial: BTreeMap<u32, Partial>,
    /// The number after the last frame delivered or given up.
    next: u32,
    /// Frames given up since the last call to [`Self::take_lost`]: the first and last.
    lost: Option<(u32, u32)>,
    /// Something of this epoch has arrived: losses count from the first frame seen, not from 0.
    /// (A device that joins another's session comes in at the lead's frame numbers: the frames
    /// before it came weren't lost to it, and 464 of them were counted so on 2026-11-20.)
    begun: bool,
    /// Fragments rebuilt from parity so far.
    repaired: u64,
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::of(KIND_VIDEO)
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// For another stream in the picture's layout (the device's camera).
    pub fn of(kind: u8) -> Self {
        Self { kind, epoch: 0, partial: BTreeMap::new(), next: 0, lost: None, begun: false, repaired: 0 }
    }

    /// The epoch being put together.
    pub fn epoch(&self) -> u16 {
        self.epoch
    }

    /// Start a new epoch: everything pending is dropped.
    pub fn reset(&mut self, epoch: u16) {
        self.epoch = epoch;
        self.partial.clear();
        self.next = 0;
        self.begun = false;
    }

    /// The frames given up since this was last asked, as the first and last of them.
    pub fn take_lost(&mut self) -> Option<(u32, u32)> {
        self.lost.take()
    }

    /// How many fragments have been rebuilt from parity (they would each have cost a frame).
    pub fn repaired(&self) -> u64 {
        self.repaired
    }

    /// Take in one datagram; returns a frame if this completed one. A parity datagram (kind
    /// [`KIND_PARITY`], for the picture's reassembler only) that is malformed or doesn't fit
    /// the frame it names is ignored.
    pub fn push(&mut self, datagram: &[u8]) -> Result<Option<Frame>, Error> {
        if self.kind == KIND_VIDEO && datagram.first() == Some(&KIND_PARITY) {
            return Ok(Parity::parse(datagram).ok().and_then(|(p, data)| self.parity(&p, data)));
        }
        let (f, payload) = Fragment::parse_as(self.kind, datagram)?;
        if f.epoch == self.epoch && !self.begun {
            self.begun = true;
            self.next = f.frame;
        }
        if f.epoch != self.epoch || f.frame < self.next {
            return Ok(None);
        }
        let p = self.partial.entry(f.frame).or_insert_with(|| Partial::new(f.key, f.recovery, f.capture_us, f.count as usize));
        if p.parts.len() != f.count as usize {
            return Err(Error::Truncated("fragment count changed within a frame"));
        }
        let slot = &mut p.parts[f.index as usize];
        if slot.is_none() {
            *slot = Some(payload.to_vec());
            p.have += 1;
        }
        Ok(self.complete(f.frame))
    }

    fn parity(&mut self, p: &Parity, data: &[u8]) -> Option<Frame> {
        if p.epoch == self.epoch && !self.begun {
            self.begun = true;
            self.next = p.frame;
        }
        if p.epoch != self.epoch || p.frame < self.next {
            return None;
        }
        let part = self.partial.entry(p.frame).or_insert_with(|| Partial::new(p.key, p.recovery, p.capture_us, p.count as usize));
        if part.parts.len() != p.count as usize || part.parity.iter().any(|g| g.first == p.first as usize) {
            return None;
        }
        part.parity.push(Group { first: p.first as usize, n: p.n as usize, last_len: p.last_len as usize, data: data.to_vec() });
        self.complete(p.frame)
    }

    /// Mend what parity allows of `frame`, and hand it on if it is whole.
    fn complete(&mut self, frame: u32) -> Option<Frame> {
        let p = self.partial.get_mut(&frame)?;
        self.repaired += p.repair();
        if p.have < p.parts.len() {
            return None;
        }
        let p = self.partial.remove(&frame).unwrap();
        // Anything older that is still incomplete is given up: later frames don't wait.
        self.partial.retain(|&n, _| n > frame);
        if frame > self.next {
            self.lost = Some((self.lost.map_or(self.next, |(first, _)| first), frame - 1));
        }
        self.next = frame + 1;
        let data = p.parts.into_iter().flat_map(|part| part.unwrap()).collect();
        Some(Frame { key: p.key, recovery: p.recovery, epoch: self.epoch, number: frame, capture_us: p.capture_us, data })
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

    /// A small deterministic generator, for loss patterns.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    #[test]
    fn parity_layout_and_sizes() {
        let frame = bytes(5000, 3);
        let ds = fragment_with_parity(&frame, true, false, 7, 9, 42, 1200, 3);
        // 5000 bytes in fragments of 1176: 5 of them, groups of 3 and 2, a parity after each.
        assert_eq!(ds.len(), 7);
        assert!(ds.iter().all(|d| d.len() <= 1200));
        assert_eq!(ds[3][0], KIND_PARITY);
        assert_eq!(ds[6][0], KIND_PARITY);
        let (p, x) = Parity::parse(&ds[3]).unwrap();
        assert_eq!((p.first, p.n, p.count, p.epoch, p.frame, p.capture_us, p.key), (0, 3, 5, 7, 9, 42, true));
        assert_eq!(x.len(), 1176);
        let (p, x) = Parity::parse(&ds[6]).unwrap();
        assert_eq!((p.first, p.n, p.last_len as usize), (3, 2, 5000 - 4 * 1176));
        assert_eq!(x.len(), 1176);
        // Fixed bytes of a header.
        let mut d = Vec::new();
        Parity { key: true, recovery: false, epoch: 0x0102, frame: 0x03040506, first: 7, count: 9, n: 2, last_len: 0x0a0b, capture_us: 0x1112131415161718 }.write(&mut d);
        assert_eq!(d, [7, 1, 2, 1, 6, 5, 4, 3, 7, 0, 9, 0, 2, 0, 0x0b, 0x0a, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11]);
        assert_eq!(d.len(), PARITY_HEADER);
    }

    /// Every datagram of each frame in random order, with chosen ones thrown away.
    fn run(frames: &[Vec<u8>], group: usize, shuffle: bool, keep: impl Fn(usize, &[u8]) -> bool, rng: &mut Rng) -> (Reassembler, Vec<Frame>) {
        let mut r = Reassembler::new();
        let mut out = Vec::new();
        for (n, f) in frames.iter().enumerate() {
            let mut ds = fragment_with_parity(f, n == 0, false, 0, n as u32, n as u64, 1200, group);
            // Shuffle (parity may come before or after its data).
            for i in (1..ds.len()).rev().filter(|_| shuffle) {
                ds.swap(i, rng.next() as usize % (i + 1));
            }
            for d in &ds {
                if keep(n, d) {
                    if let Some(f) = r.push(d).unwrap() {
                        out.push(f);
                    }
                }
            }
        }
        (r, out)
    }

    fn index_of(d: &[u8]) -> usize {
        u16::from_le_bytes([d[8], d[9]]) as usize
    }

    #[test]
    fn one_missing_fragment_per_group_is_rebuilt_byte_exact() {
        let mut rng = Rng(1);
        for round in 0..200 {
            let size = 1 + rng.next() as usize % 40_000;
            let group = 1 + rng.next() as usize % 12;
            let frames = vec![bytes(size, round as u8), bytes(1 + rng.next() as usize % 9000, 5)];
            // At most one data fragment gone from each group of the first frame.
            let count = fragment(&frames[0], true, false, 0, 0, 0, 1196).len();
            let mut dead = std::collections::HashSet::new();
            for g in (0..count).step_by(group) {
                let n = group.min(count - g);
                let pick = rng.next() as usize % (n + 1);
                if pick < n {
                    dead.insert(g + pick);
                }
            }
            for shuffle in [false, true] {
                let (r, got) = run(&frames, group, shuffle, |n, d| n != 0 || d[0] != KIND_VIDEO || !dead.contains(&index_of(d)), &mut rng);
                assert_eq!(got.len(), 2, "round {round}: size {size} group {group}");
                assert_eq!(got[0].data, frames[0], "round {round}");
                assert_eq!(got[1].data, frames[1]);
                // (Parity that beats its data to it makes a late fragment look lost, and rebuilds it.)
                assert!(if shuffle { r.repaired() >= dead.len() as u64 } else { r.repaired() == dead.len() as u64 }, "round {round}");
            }
        }
    }

    #[test]
    fn two_missing_in_a_group_are_never_made_up() {
        let mut rng = Rng(7);
        for round in 0..200 {
            let frames = vec![bytes(30_000, round as u8), bytes(500, 1)];
            let group = 4 + rng.next() as usize % 6;
            let (a, b) = (rng.next() as usize % group, rng.next() as usize % group);
            for shuffle in [false, true] {
                let (r, got) = run(&frames, group, shuffle, |n, d| n != 0 || d[0] != KIND_VIDEO || (index_of(d) != a && index_of(d) != b), &mut rng);
                if a == b {
                    assert_eq!(got[0].data, frames[0]);
                    assert!(r.repaired() >= 1);
                } else {
                    // Given up, not guessed at: the next frame comes, the first never does.
                    assert_eq!(got.len(), 1);
                    assert_eq!(got[0].data, frames[1]);
                }
            }
        }
    }

    #[test]
    fn parity_by_itself_rebuilds_a_whole_frame_of_one_fragment() {
        let f = bytes(300, 9);
        let ds = fragment_with_parity(&f, true, false, 0, 0, 77, 1200, 10);
        assert_eq!(ds.len(), 2);
        let mut r = Reassembler::new();
        let got = r.push(&ds[1]).unwrap().unwrap();
        assert_eq!((got.data, got.key, got.capture_us), (f, true, 77));
        assert_eq!(r.repaired(), 1);
    }

    #[test]
    fn parity_for_other_epochs_old_frames_and_duplicates_does_nothing() {
        let f = bytes(3000, 2);
        let ds = fragment_with_parity(&f, false, false, 3, 5, 0, 1200, 10);
        let mut r = Reassembler::new();
        r.reset(3);
        r.next = 6;
        // An old frame.
        for d in &ds {
            assert!(r.push(d).unwrap().is_none());
        }
        assert!(r.partial.is_empty());
        r.reset(4);
        // Another epoch's.
        for d in &ds {
            assert!(r.push(d).unwrap().is_none());
        }
        assert!(r.partial.is_empty());
        // The same parity twice, and garbage in its place, leave the frame alone.
        r.reset(3);
        let (data, parity) = ds.split_at(ds.len() - 1);
        r.push(&parity[0]).unwrap();
        r.push(&parity[0]).unwrap();
        r.push(&parity[0][..10]).unwrap();
        let mut bad = parity[0].clone();
        bad[12] = 0; // a group of none
        assert!(r.push(&bad).unwrap().is_none());
        let mut out = None;
        for d in &data[1..] {
            out = r.push(d).unwrap();
        }
        assert_eq!(out.unwrap().data, f);
        assert_eq!(r.repaired(), 1);
    }

    #[test]
    fn a_given_up_frame_is_not_mended_late() {
        let a = fragment_with_parity(&bytes(3000, 1), true, false, 0, 0, 0, 1200, 10);
        let b = fragment_with_parity(&bytes(100, 2), false, false, 0, 1, 0, 1200, 10);
        let mut r = Reassembler::new();
        // Frame 0 loses most of it, frame 1 arrives: 0 is given up.
        r.push(&a[0]).unwrap();
        for d in &b {
            r.push(d).unwrap();
        }
        assert_eq!(r.take_lost(), Some((0, 0)));
        // Its parity turns up late: ignored, nothing comes out.
        assert!(r.push(a.last().unwrap()).unwrap().is_none());
        assert!(r.partial.is_empty());
    }
}
