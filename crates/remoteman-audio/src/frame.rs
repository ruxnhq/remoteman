//! From samples as PipeWire (or WASAPI, in the Windows viewer) delivers them, in pieces of any
//! size, to packets of one frame each, leaving out the frames of a silence.

use anyhow::Result;

use crate::codec::Encoder;
use crate::{Channels, FRAME_SAMPLES, FRAME_US, Packet, SAMPLE_RATE};

/// Below one step of 16-bit sound a sample counts as silence.
const SILENCE_LEVEL: f32 = 1.0 / 32768.0;

/// How many silent frames in a row are still sent before sending stops: 100 ms. They let the
/// decoder end on silence, and keep a short rest inside a sound from stopping the stream.
pub(crate) const SILENCE_FRAMES: u32 = 10;

/// A piece of sound that starts this much off where the one before ended isn't its
/// continuation: the stream stopped in between.
const BREAK_US: i64 = 20_000;

pub struct Framer {
    encoder: Encoder,
    channels: usize,
    /// Samples that don't fill a frame yet.
    pending: Vec<f32>,
    /// When the sample after the last one fed is due.
    expected_us: i64,
    seq: u32,
    /// Silent frames in a row. Sending starts stopped: silence at the start is no sound either.
    quiet: u32,
    on_packet: Box<dyn FnMut(Packet) + Send>,
}

impl Framer {
    pub fn new(channels: Channels, on_packet: Box<dyn FnMut(Packet) + Send>) -> Result<Self> {
        Ok(Self {
            encoder: Encoder::new(channels)?,
            channels: channels.count(),
            pending: Vec::with_capacity(FRAME_SAMPLES * channels.count() * 4),
            expected_us: 0,
            seq: 0,
            quiet: SILENCE_FRAMES,
            on_packet,
        })
    }

    /// Take in interleaved samples, the first of them captured at `first_us`, and hand over a
    /// packet for every frame they complete.
    pub fn feed(&mut self, samples: &[f32], first_us: u64) {
        let first_us = first_us as i64;
        // Left over from before the stream stopped: by now it is old sound, and playing it in
        // front of the new one would be worse than losing those last milliseconds.
        if !self.pending.is_empty() && (first_us - self.expected_us).abs() > BREAK_US {
            self.pending.clear();
        }
        let per_sample = |frames: usize| (frames as i64 * 1_000_000) / SAMPLE_RATE as i64;
        // When the first sample still waiting was captured.
        let base_us = first_us - per_sample(self.pending.len() / self.channels);
        self.expected_us = first_us + per_sample(samples.len() / self.channels);

        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(samples);
        let size = FRAME_SAMPLES * self.channels;
        let mut done = 0;
        for (i, frame) in pending.chunks_exact(size).enumerate() {
            self.frame(frame, (base_us + i as i64 * FRAME_US as i64).max(0) as u64);
            done += size;
        }
        pending.drain(..done);
        self.pending = pending;
    }

    fn frame(&mut self, frame: &[f32], capture_us: u64) {
        if frame.iter().all(|s| s.abs() < SILENCE_LEVEL) {
            self.quiet = self.quiet.saturating_add(1);
            if self.quiet > SILENCE_FRAMES {
                return;
            }
        } else {
            self.quiet = 0;
        }
        match self.encoder.encode(frame) {
            Ok(data) => {
                (self.on_packet)(Packet { seq: self.seq, capture_us, data });
                self.seq = self.seq.wrapping_add(1);
            }
            Err(e) => eprintln!("remoteman-audio: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::codec::Decoder;
    use crate::testing::{frequency, sine_from};

    fn framer() -> (Framer, Arc<Mutex<Vec<Packet>>>) {
        let packets = Arc::new(Mutex::new(Vec::new()));
        let sink = packets.clone();
        (Framer::new(Channels::Stereo, Box::new(move |p| sink.lock().unwrap().push(p))).unwrap(), packets)
    }

    /// Feed `frames` samples (per channel) of tone or silence in pieces of `piece`, as a graph
    /// with that quantum would, starting at `*at` samples on the clock.
    fn feed(framer: &mut Framer, at: &mut usize, frames: usize, piece: usize, tone: bool) {
        let end = *at + frames;
        while *at < end {
            let n = piece.min(end - *at);
            let samples = if tone { sine_from(*at, 1000.0, 0.5, n, 2) } else { vec![0.0; n * 2] };
            framer.feed(&samples, (*at as u64 * 1_000_000) / SAMPLE_RATE as u64);
            *at += n;
        }
    }

    #[test]
    fn pieces_of_any_size_become_frames() {
        for piece in [128, 256, 480, 1024, 2048] {
            let (mut framer, packets) = framer();
            let mut at = 0;
            feed(&mut framer, &mut at, 48_000, piece, true);
            let packets = packets.lock().unwrap();
            assert_eq!(packets.len(), 100, "pieces of {piece}");
            let mut decoder = Decoder::new(Channels::Stereo).unwrap();
            let mut left = Vec::new();
            for (i, p) in packets.iter().enumerate() {
                assert_eq!(p.seq, i as u32);
                // Each frame is stamped with when its first sample came, whatever the pieces.
                assert!(p.capture_us.abs_diff(i as u64 * FRAME_US) <= 1, "pieces of {piece}: frame {i} at {}", p.capture_us);
                let mut out = [0.0; FRAME_SAMPLES * 2];
                decoder.decode(&p.data, &mut out).unwrap();
                left.extend(out.iter().step_by(2));
            }
            let hz = frequency(&left[5 * FRAME_SAMPLES..]);
            assert!((hz - 1000.0).abs() < 5.0, "pieces of {piece}: {hz} Hz");
        }
    }

    #[test]
    fn silence_is_not_sent() {
        let (mut framer, packets) = framer();
        let mut at = 0;
        // Silence first: nothing.
        feed(&mut framer, &mut at, 48_000, 256, false);
        assert_eq!(packets.lock().unwrap().len(), 0);
        // Half a second of tone, then two seconds of silence: the tone's frames, the run of
        // silent ones that ends it, and no more.
        feed(&mut framer, &mut at, 24_000, 256, true);
        feed(&mut framer, &mut at, 96_000, 256, false);
        assert_eq!(packets.lock().unwrap().len(), 50 + SILENCE_FRAMES as usize);
        // Sound again: it goes on with the next seq, and the time shows how long the pause was.
        feed(&mut framer, &mut at, 4800, 256, true);
        let packets = packets.lock().unwrap();
        assert_eq!(packets.len(), 50 + SILENCE_FRAMES as usize + 10);
        for (i, p) in packets.iter().enumerate() {
            assert_eq!(p.seq, i as u32);
        }
        let (last, first) = (&packets[59], &packets[60]);
        assert_eq!(first.capture_us - last.capture_us, 2_000_000 - (SILENCE_FRAMES as u64 - 1) * FRAME_US);
    }

    #[test]
    fn a_short_rest_does_not_stop_the_stream() {
        let (mut framer, packets) = framer();
        let mut at = 0;
        feed(&mut framer, &mut at, 4800, 256, true);
        feed(&mut framer, &mut at, 2400, 256, false);
        feed(&mut framer, &mut at, 4800, 256, true);
        assert_eq!(packets.lock().unwrap().len(), 25);
    }

    #[test]
    fn what_is_left_from_before_a_stop_is_dropped() {
        let (mut framer, packets) = framer();
        let mut at = 0;
        // 1000 samples: two frames and 40 samples over.
        feed(&mut framer, &mut at, 1000, 1000, true);
        assert_eq!(packets.lock().unwrap().len(), 2);
        // The stream stops for five seconds. The next frame starts with the new sound.
        at += 240_000;
        feed(&mut framer, &mut at, 480, 480, true);
        let packets = packets.lock().unwrap();
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[2].capture_us, (1000 + 240_000) * 1_000_000 / SAMPLE_RATE as u64);
    }
}
