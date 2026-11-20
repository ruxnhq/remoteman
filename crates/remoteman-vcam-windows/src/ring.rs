//! The shared-memory ring that carries finished pictures from the host to the media source in
//! the Frame Server's process. Pure memory layout and atomics: where the bytes come from (a named
//! section, a file mapping, a test's `Vec`) is [`crate::shm`]'s business.
//!
//! One writer, any number of readers; the readers only read. A reader wants the newest picture,
//! never a backlog, so this is a small set of slots and a counter, not a queue:
//!
//! ```text
//! header (64 bytes)   magic, version, width, height, fps, slots, slot stride,
//!                     active (u32), latest (u64: number of the newest complete picture),
//!                     alive (u64: the writer's last sign of life, in ms)
//! slot 0 .. slots-1   seq (u64: the picture's number once complete, 0 while being written),
//!                     pts (u64: capture time in µs, the sender's clock), then the picture
//! ```
//!
//! The picture is NV12 (Y plane, then interleaved CbCr), video range, `width * height * 3 / 2`
//! bytes. The writer fills slot `n % slots` for picture `n` (counting from 1): it zeroes the slot's
//! `seq`, copies, sets `seq = n`, then publishes `latest = n`. A reader takes `latest`, copies that
//! slot and keeps the copy only if `seq` was `n` before and after (a seqlock): a slot is only
//! rewritten `slots` pictures later, so the copy is almost always good, and a torn one is
//! never used.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

pub const MAGIC: u32 = 0x434D_4D52; // "RMMC"
pub const VERSION: u32 = 1;
const HEADER: usize = 64;
const SLOT_HEADER: usize = 16;
/// Slots a ring is made with.
pub const SLOTS: u32 = 3;
/// The writer shows it is alive at least this often (ms); a reader that hasn't seen it for
/// [`STALE_MS`] shows black.
pub const BEAT_MS: u64 = 500;
pub const STALE_MS: u64 = 3000;

/// Bytes of one NV12 picture.
pub const fn picture_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

fn stride(width: u32, height: u32) -> usize {
    (SLOT_HEADER + picture_bytes(width, height)).next_multiple_of(64)
}

/// Bytes of memory a ring for pictures of this size needs.
pub const fn ring_bytes(width: u32, height: u32) -> usize {
    HEADER + SLOTS as usize * (SLOT_HEADER + picture_bytes(width, height)).next_multiple_of(64)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Too small, or not a ring: no magic, another version, sizes that don't add up.
    Invalid,
}

/// A ring in memory somebody else keeps alive.
pub struct Ring {
    base: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    fps: u32,
    slots: u32,
    stride: usize,
}

// SAFETY: all shared access goes through atomics and the seqlock described above.
unsafe impl Send for Ring {}
unsafe impl Sync for Ring {}

impl Ring {
    /// Lay out a fresh ring in `len` bytes at `base` (8-byte aligned, writable, zeroed or not).
    ///
    /// # Safety
    /// `base..base+len` is valid memory for as long as the ring is used.
    pub unsafe fn create(base: *mut u8, len: usize, width: u32, height: u32, fps: u32) -> Result<Ring, Error> {
        if width < 16 || height < 16 || width > 8192 || height > 8192 || width % 2 != 0 || height % 2 != 0 || base.align_offset(8) != 0 || len < ring_bytes(width, height) {
            return Err(Error::Invalid);
        }
        let stride = stride(width, height);
        // SAFETY: in range, as checked.
        unsafe {
            std::ptr::write_bytes(base, 0, HEADER + SLOTS as usize * stride);
            let h = base as *mut u32;
            h.add(1).write(VERSION);
            h.add(2).write(width);
            h.add(3).write(height);
            h.add(4).write(fps);
            h.add(5).write(SLOTS);
            h.add(6).write(stride as u32);
        }
        let ring = Ring { base, len, width, height, fps, slots: SLOTS, stride };
        // The magic last: a reader that sees it sees the rest.
        ring.atomic32(0).store(MAGIC, Ordering::Release);
        Ok(ring)
    }

    /// Take a ring somebody else made; everything it says is checked against `len`.
    ///
    /// # Safety
    /// `base..base+len` is valid readable memory for as long as the ring is used.
    pub unsafe fn open(base: *mut u8, len: usize) -> Result<Ring, Error> {
        if len < HEADER || base.align_offset(8) != 0 {
            return Err(Error::Invalid);
        }
        // SAFETY: at least a header, as checked.
        let word = |i: usize| unsafe { (*(base as *const AtomicU32).add(i)).load(Ordering::Acquire) };
        if word(0) != MAGIC || word(1) != VERSION {
            return Err(Error::Invalid);
        }
        let (width, height, fps, slots, stride_in_file) = (word(2), word(3), word(4), word(5), word(6) as usize);
        let ok = (16..=8192).contains(&width) && (16..=8192).contains(&height) && width % 2 == 0 && height % 2 == 0 && slots >= 2 && slots <= 16;
        if !ok || stride_in_file != stride(width, height) || HEADER + slots as usize * stride_in_file > len {
            return Err(Error::Invalid);
        }
        Ok(Ring { base, len, width, height, fps, slots, stride: stride_in_file })
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn fps(&self) -> u32 {
        self.fps
    }
    pub fn picture_bytes(&self) -> usize {
        picture_bytes(self.width, self.height)
    }
    pub fn len(&self) -> usize {
        self.len
    }

    fn atomic32(&self, word: usize) -> &AtomicU32 {
        // SAFETY: inside the header, aligned.
        unsafe { &*(self.base as *const AtomicU32).add(word) }
    }
    fn atomic64(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: offset is 8-aligned and inside the ring.
        unsafe { &*(self.base.add(offset) as *const AtomicU64) }
    }
    fn latest(&self) -> &AtomicU64 {
        self.atomic64(32)
    }
    fn alive(&self) -> &AtomicU64 {
        self.atomic64(40)
    }
    fn slot(&self, n: u64) -> usize {
        HEADER + (n % self.slots as u64) as usize * self.stride
    }

    // --- the writer ---

    /// Put a picture in (NV12, `picture_bytes()` long; anything else is refused) and make it the
    /// newest. `pts_us` is its capture time. Returns its number.
    pub fn write(&self, nv12: &[u8], pts_us: u64, now_ms: u64) -> Option<u64> {
        if nv12.len() != self.picture_bytes() {
            return None;
        }
        let n = self.latest().load(Ordering::Relaxed) + 1;
        let at = self.slot(n);
        let seq = self.atomic64(at);
        seq.store(0, Ordering::Release);
        fence(Ordering::SeqCst);
        // SAFETY: the slot is inside the ring (checked at creation); readers copy it only under
        // the seqlock.
        unsafe { std::ptr::copy_nonoverlapping(nv12.as_ptr(), self.base.add(at + SLOT_HEADER), nv12.len()) };
        self.atomic64(at + 8).store(pts_us, Ordering::Relaxed);
        seq.store(n, Ordering::Release);
        self.latest().store(n, Ordering::Release);
        self.alive().store(now_ms, Ordering::Release);
        Some(n)
    }

    /// The writer is here (or, with `false`, gone: readers show black at once).
    pub fn set_active(&self, active: bool, now_ms: u64) {
        self.alive().store(now_ms, Ordering::Release);
        self.atomic32(7).store(active as u32, Ordering::Release);
    }

    /// A sign of life with no new picture.
    pub fn beat(&self, now_ms: u64) {
        self.alive().store(now_ms, Ordering::Release);
    }

    // --- the readers ---

    /// Is a writer there that has shown itself within [`STALE_MS`] of `now_ms`?
    pub fn live(&self, now_ms: u64) -> bool {
        self.atomic32(7).load(Ordering::Acquire) != 0 && now_ms.saturating_sub(self.alive().load(Ordering::Acquire)) <= STALE_MS
    }

    /// The newest picture number (0: none yet).
    pub fn newest(&self) -> u64 {
        self.latest().load(Ordering::Acquire)
    }

    /// Copy the newest picture into `out` (`picture_bytes()` long). Returns its number and capture
    /// time, or `None` if there is none yet or `out` is the wrong size.
    pub fn read(&self, out: &mut [u8]) -> Option<(u64, u64)> {
        if out.len() != self.picture_bytes() {
            return None;
        }
        for _ in 0..4 {
            let n = self.latest().load(Ordering::Acquire);
            if n == 0 {
                return None;
            }
            let at = self.slot(n);
            if self.atomic64(at).load(Ordering::Acquire) != n {
                continue;
            }
            // SAFETY: inside the ring; a torn copy is thrown away below.
            unsafe { std::ptr::copy_nonoverlapping(self.base.add(at + SLOT_HEADER), out.as_mut_ptr(), out.len()) };
            fence(Ordering::SeqCst);
            if self.atomic64(at).load(Ordering::Acquire) == n {
                return Some((n, self.atomic64(at + 8).load(Ordering::Relaxed)));
            }
        }
        None
    }
}

/// A buffer for tests and for rings in process memory.
pub struct Heap(Vec<u64>);

impl Heap {
    pub fn new(len: usize) -> Heap {
        Heap(vec![0; len.div_ceil(8)])
    }
    pub fn ptr(&mut self) -> (*mut u8, usize) {
        (self.0.as_mut_ptr() as *mut u8, self.0.len() * 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![v; picture_bytes(w, h)]
    }

    #[test]
    fn write_then_read() {
        let mut heap = Heap::new(ring_bytes(64, 32));
        let (p, l) = heap.ptr();
        let w = unsafe { Ring::create(p, l, 64, 32, 30) }.unwrap();
        let r = unsafe { Ring::open(p, l) }.unwrap();
        assert_eq!((r.width(), r.height(), r.fps()), (64, 32, 30));
        let mut out = pic(64, 32, 0);
        assert_eq!(r.read(&mut out), None);
        assert!(!r.live(0));
        w.set_active(true, 100);
        assert!(r.live(100));
        assert_eq!(w.write(&pic(64, 32, 7), 1234, 110), Some(1));
        assert_eq!(r.read(&mut out), Some((1, 1234)));
        assert!(out.iter().all(|&b| b == 7));
        // Many pictures: the reader always gets the newest, whole.
        for i in 2..=20u64 {
            w.write(&pic(64, 32, i as u8), i, 110).unwrap();
            assert_eq!(r.read(&mut out), Some((i, i)));
            assert!(out.iter().all(|&b| b == i as u8));
        }
        assert_eq!(r.newest(), 20);
    }

    #[test]
    fn wrong_sizes_are_refused() {
        let mut heap = Heap::new(ring_bytes(64, 32));
        let (p, l) = heap.ptr();
        let w = unsafe { Ring::create(p, l, 64, 32, 30) }.unwrap();
        assert_eq!(w.write(&[0; 10], 0, 0), None);
        let mut small = vec![0; 10];
        assert_eq!(w.read(&mut small), None);
        // Too little memory, odd sizes.
        assert!(unsafe { Ring::create(p, 100, 64, 32, 30) }.is_err());
        assert!(unsafe { Ring::create(p, l, 63, 32, 30) }.is_err());
    }

    #[test]
    fn liveness_follows_the_writer() {
        let mut heap = Heap::new(ring_bytes(64, 32));
        let (p, l) = heap.ptr();
        let w = unsafe { Ring::create(p, l, 64, 32, 30) }.unwrap();
        let r = unsafe { Ring::open(p, l) }.unwrap();
        w.set_active(true, 1000);
        assert!(r.live(1000 + STALE_MS));
        // A writer that stopped showing itself (the host died): black.
        assert!(!r.live(1000 + STALE_MS + 1));
        w.beat(5000);
        assert!(r.live(5100));
        // A writer that left on purpose.
        w.set_active(false, 5200);
        assert!(!r.live(5200));
    }

    #[test]
    fn garbage_is_not_a_ring() {
        let mut heap = Heap::new(ring_bytes(64, 32));
        let (p, l) = heap.ptr();
        assert!(unsafe { Ring::open(p, l) }.is_err()); // zeroed
        unsafe { Ring::create(p, l, 64, 32, 30) }.unwrap();
        assert!(unsafe { Ring::open(p, 70) }.is_err()); // cut short
        assert!(unsafe { Ring::open(p, 10) }.is_err());
        // Header fields that lie.
        unsafe { (p as *mut u32).add(2).write(8000) };
        assert!(unsafe { Ring::open(p, l) }.is_err());
        unsafe { (p as *mut u32).add(2).write(64) };
        unsafe { (p as *mut u32).add(5).write(1000) };
        assert!(unsafe { Ring::open(p, l) }.is_err());
    }

    #[test]
    fn a_reader_never_keeps_a_torn_picture() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let (w, h) = (64u32, 32u32);
        let mut heap = Heap::new(ring_bytes(w, h));
        let (p, l) = heap.ptr();
        let writer = Arc::new(unsafe { Ring::create(p, l, w, h, 30) }.unwrap());
        let reader = unsafe { Ring::open(p, l) }.unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let t = {
            let (writer, stop) = (writer.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut i = 0u8;
                while !stop.load(Ordering::Relaxed) {
                    i = i.wrapping_add(1);
                    writer.write(&pic(w, h, i), 0, 0);
                }
            })
        };
        let mut out = pic(w, h, 0);
        let mut got = 0;
        for _ in 0..200_000 {
            if reader.read(&mut out).is_some() {
                got += 1;
                assert!(out.iter().all(|&b| b == out[0]), "torn picture");
            }
        }
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
        assert!(got > 0);
        drop(heap);
    }
}
