//! The jitter buffer: packets go in as the network delivers them, samples come out as the sound
//! card asks for them.

use std::collections::VecDeque;

use anyhow::Result;

use crate::codec::Decoder;
use crate::{Channels, FRAME_SAMPLES, Packet};

/// How much sound is held before playing starts, in frames of 10 ms. It is what absorbs packets
/// arriving unevenly; the plan asks for 20 to 40 ms.
pub const JITTER_FRAMES: usize = 3;

/// The most frames the buffer holds back before playing (80 ms): where it starts if asked for
/// more, and as far as it grows by itself when packets keep arriving after their turn.
const MAX_DEPTH: usize = 8;

/// A packet no more than this many frames after its turn just missed it.
const JUST_MISSED: i32 = 5;

/// A packet this far from the one expected, on either side, isn't late or early: the sender
/// started over, and so does the buffer.
const RESYNC_FRAMES: u32 = 100;

/// How many frames are made up when the buffer runs dry before it gives up and plays silence.
/// It covers packets that are a little late without a gap in the sound. When the sender went
/// quiet instead, these two frames are a fading continuation of its last sound: of silence
/// after a [`crate::SpeakerSink`]'s silent run, of up to 20 ms of sound when its stream just
/// stopped.
const DRY_CONCEAL_FRAMES: u32 = 2;

/// What a [`JitterBuffer`] did so far, in frames.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    /// Decoded from their packet.
    pub played: u64,
    /// Made up: the packet was lost, or not here in time.
    pub concealed: u64,
    /// Packets dropped because their turn had passed, or that were already here.
    pub late: u64,
    /// Packets dropped to catch up, when more sound was held than the buffer allows.
    pub skipped: u64,
}

/// Orders packets by `seq`, decodes them when their turn comes and conceals the missing ones.
///
/// It holds [`JITTER_FRAMES`] (or the depth given) of sound before it starts and starts over the
/// same way whenever it runs dry, which is also how a sender's pause is handled (see
/// [`crate::SpeakerSink`]): `seq` has no hole across a pause, so nothing is concealed for it
/// beyond the two frames after the buffer empties.
///
/// After a stall the delay is back at the depth too: the packets that were held up arrive
/// together, and all but the newest are dropped.
///
/// The two ends run on different clocks. When the sender's is the faster one the buffer slowly
/// fills, and at twice its depth plus two frames it drops down to its depth again; when it is
/// the slower one the buffer runs dry now and then and one frame is concealed. Either happens
/// about once in a few minutes for ordinary clocks.
pub struct JitterBuffer {
    decoder: Decoder,
    channels: usize,
    depth: usize,
    /// The packets waiting, the one for `next_seq` first. `None`: not here (yet).
    slots: VecDeque<Option<Vec<u8>>>,
    next_seq: u32,
    /// False until the first packet says where `seq` starts.
    anchored: bool,
    /// Holding back until `depth` frames are here.
    filling: bool,
    /// Samples of silence played while filling with something already held: a few packets that
    /// never reach `depth` still get played, after as long as `depth` would have taken.
    waited: usize,
    /// Frames concealed in a row because nothing was held.
    dry: u32,
    /// Decoded samples not yet taken: `pull` is asked for any number, a frame is always 10 ms.
    pcm: VecDeque<f32>,
    frame: Vec<f32>,
    stats: JitterStats,
}

impl JitterBuffer {
    /// `depth` in frames; it is kept between 1 and 8 (10 to 80 ms).
    pub fn new(channels: Channels, depth: usize) -> Result<Self> {
        let depth = depth.clamp(1, MAX_DEPTH);
        Ok(Self {
            decoder: Decoder::new(channels)?,
            channels: channels.count(),
            depth,
            slots: VecDeque::with_capacity(RESYNC_FRAMES as usize),
            next_seq: 0,
            anchored: false,
            filling: true,
            waited: 0,
            dry: 0,
            pcm: VecDeque::with_capacity(FRAME_SAMPLES * channels.count()),
            frame: vec![0.0; FRAME_SAMPLES * channels.count()],
            stats: JitterStats::default(),
        })
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// The most frames held: beyond it the oldest are dropped.
    fn limit(&self) -> usize {
        self.depth * 2 + 2
    }

    /// Start over with `seq` as the next packet to play.
    fn restart(&mut self, seq: u32) {
        self.slots.clear();
        self.pcm.clear();
        self.next_seq = seq;
        self.anchored = true;
        self.filling = true;
        self.waited = 0;
        self.dry = 0;
        let _ = self.decoder.reset();
    }

    /// Take a packet in. One whose turn has passed is dropped, as is one that is already here.
    pub fn push(&mut self, packet: Packet) {
        if !self.anchored {
            self.restart(packet.seq);
        }
        // How far ahead of the next one to play; `seq` wraps around, so the distance does too.
        let mut ahead = packet.seq.wrapping_sub(self.next_seq) as i32;
        if ahead.unsigned_abs() >= RESYNC_FRAMES {
            self.restart(packet.seq);
            ahead = 0;
        }
        if ahead < 0 {
            self.stats.late += 1;
            // Its turn came before it did, and a made-up frame was played in its place: the
            // sender delivers less evenly than this depth allows for (one that sends two or
            // three packets at a time, say). Hold one frame more from here on, starting with
            // a refill now, when the sound has just had its gap anyway. It never goes back
            // down while this sender lasts.
            // (One that comes long after its turn is a straggler, and says nothing of that.)
            if ahead >= -JUST_MISSED && self.depth < MAX_DEPTH && !self.filling {
                self.depth += 1;
                self.filling = true;
                self.waited = 0;
            }
            return;
        }
        let at = ahead as usize;
        if self.slots.len() <= at {
            self.slots.resize(at + 1, None);
        }
        if self.slots[at].is_some() {
            self.stats.late += 1;
            return;
        }
        self.slots[at] = Some(packet.data);
        // Too much held: nobody is taking samples, or the sender's clock runs ahead.
        if self.slots.len() > self.limit() {
            self.catch_up();
        }
    }

    /// Drop the oldest packets until only `depth` frames are held, so that the delay stays
    /// what it is meant to be.
    fn catch_up(&mut self) {
        while self.slots.len() > self.depth {
            if self.slots.pop_front().flatten().is_some() {
                self.stats.skipped += 1;
            }
            self.next_seq = self.next_seq.wrapping_add(1);
        }
        self.pcm.clear();
    }

    /// Fill `out` (interleaved) with the next samples: sound when there is some, silence when
    /// not.
    pub fn pull(&mut self, out: &mut [f32]) {
        let mut filled = 0;
        while filled < out.len() {
            if self.pcm.is_empty() && !self.next_frame() {
                break;
            }
            let n = self.pcm.len().min(out.len() - filled);
            for (o, s) in out[filled..filled + n].iter_mut().zip(self.pcm.drain(..n)) {
                *o = s;
            }
            filled += n;
        }
        out[filled..].fill(0.0);
        if self.filling && !self.slots.is_empty() {
            self.waited += (out.len() - filled) / self.channels;
        }
    }

    /// Decode or make up the next frame into `pcm`. False: there is nothing to play now.
    fn next_frame(&mut self) -> bool {
        if self.filling {
            let ready = self.slots.len() >= self.depth || (!self.slots.is_empty() && self.waited >= self.depth * FRAME_SAMPLES);
            if !ready {
                return false;
            }
            self.filling = false;
            self.waited = 0;
            // After a stall the packets held up arrive all at once. Played in full they would
            // keep the sound that much later from here on. (One over the depth is let through:
            // it is what arrives between two calls when all is well.)
            if self.slots.len() > self.depth + 1 {
                self.catch_up();
            }
            // Start at the first packet that is here, not at a hole before it.
            while let Some(None) = self.slots.front() {
                self.slots.pop_front();
                self.next_seq = self.next_seq.wrapping_add(1);
            }
        }
        let decoded = match self.slots.pop_front() {
            Some(Some(data)) => {
                self.dry = 0;
                match self.decoder.decode(&data, &mut self.frame) {
                    Ok(()) => {
                        self.stats.played += 1;
                        Ok(())
                    }
                    // Not a packet of ours: treat it as lost.
                    Err(_) => {
                        self.stats.concealed += 1;
                        self.decoder.conceal(None, &mut self.frame)
                    }
                }
            }
            // A hole with later packets behind it: lost.
            Some(None) => {
                self.stats.concealed += 1;
                let next = self.slots.front().and_then(|p| p.as_deref());
                self.decoder.conceal(next, &mut self.frame)
            }
            // Nothing held at all: late, lost, or the sender went quiet.
            None => {
                if self.dry >= DRY_CONCEAL_FRAMES {
                    // The frames made up stood in for packets that were taken for late. They
                    // weren't: nothing came at all. Their turn comes again, so that a sender
                    // going on after a pause is heard from its first packet.
                    self.next_seq = self.next_seq.wrapping_sub(self.dry);
                    self.filling = true;
                    self.dry = 0;
                    return false;
                }
                self.dry += 1;
                self.stats.concealed += 1;
                self.decoder.conceal(None, &mut self.frame)
            }
        };
        if decoded.is_err() {
            self.frame.fill(0.0);
        }
        self.next_seq = self.next_seq.wrapping_add(1);
        self.pcm.extend(self.frame.iter().copied());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Encoder;
    use crate::testing::{frequency, peak, sine};

    /// `count` packets of a 1 kHz tone, numbered from `first`.
    fn tone_packets(first: u32, count: usize) -> Vec<Packet> {
        let mut encoder = Encoder::new(Channels::Mono).unwrap();
        sine(1000.0, 0.5, count * FRAME_SAMPLES, 1)
            .chunks(FRAME_SAMPLES)
            .enumerate()
            .map(|(i, f)| Packet { seq: first.wrapping_add(i as u32), capture_us: i as u64 * 10_000, data: encoder.encode(f).unwrap() })
            .collect()
    }

    fn pull_frames(buffer: &mut JitterBuffer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * FRAME_SAMPLES];
        // In pieces of an odd size, as a sound card asks.
        for piece in out.chunks_mut(331) {
            buffer.pull(piece);
        }
        out
    }

    /// Hand over `packets` at the pace they are played: one frame out for each packet in.
    fn stream(buffer: &mut JitterBuffer, packets: &[Packet]) -> Vec<f32> {
        let mut out = Vec::new();
        for p in packets {
            buffer.push(p.clone());
            out.extend(pull_frames(buffer, 1));
        }
        out
    }

    #[test]
    fn waits_for_its_depth_then_plays() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        let packets = tone_packets(0, 40);
        let mut out = stream(&mut buffer, &packets);
        // Silence until the third packet is in, then every packet in turn.
        assert_eq!(peak(&out[..2 * FRAME_SAMPLES]), 0.0, "plays before it is full");
        assert!(peak(&out[2 * FRAME_SAMPLES..3 * FRAME_SAMPLES]) > 0.0);
        out.extend(pull_frames(&mut buffer, 2));
        assert!((frequency(&out[7 * FRAME_SAMPLES..]) - 1000.0).abs() < 5.0);
        assert_eq!(buffer.stats(), JitterStats { played: 40, ..Default::default() });
    }

    #[test]
    fn holds_more_when_packets_just_miss_their_turn() {
        // A sender that hands over two packets at a time, every fourth pair a frame after it
        // is needed: with a depth of 2 their turn comes before they do.
        let mut buffer = JitterBuffer::new(Channels::Mono, 2).unwrap();
        let packets = tone_packets(3, 240);
        let mut late_at_half = 0;
        for (i, pair) in packets.chunks(2).enumerate() {
            // As much is taken as comes; a late pair is one that comes after the first of its
            // two frames was asked for.
            let late = i % 4 == 3;
            if late {
                pull_frames(&mut buffer, 1);
            }
            for p in pair {
                buffer.push(p.clone());
            }
            pull_frames(&mut buffer, if late { 1 } else { 2 });
            if i == 59 {
                late_at_half = buffer.stats().late;
            }
        }
        let stats = buffer.stats();
        assert!(late_at_half > 0, "the test never made a packet late: {stats:?}");
        // It grew until nothing was late any more, in a few steps, and stayed there.
        assert_eq!(stats.late, late_at_half, "still late in the second half: {stats:?}");
        assert!(stats.late <= 6, "{stats:?}");
    }

    #[test]
    fn conceals_lost_drops_late_and_duplicate() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        let packets = tone_packets(7, 30);
        let mut out = Vec::new();
        for (i, p) in packets.iter().enumerate() {
            match i {
                // Lost.
                10 => {}
                // Swapped with the one after it: out of order, but both in time.
                15 => buffer.push(packets[16].clone()),
                16 => buffer.push(packets[15].clone()),
                // Twice.
                20 => {
                    buffer.push(p.clone());
                    buffer.push(p.clone());
                }
                _ => buffer.push(p.clone()),
            }
            out.extend(pull_frames(&mut buffer, 1));
        }
        // The lost one, long after its turn.
        buffer.push(packets[10].clone());
        out.extend(pull_frames(&mut buffer, 2));
        assert_eq!(buffer.stats(), JitterStats { played: 29, concealed: 1, late: 2, skipped: 0 });
        let hz = frequency(&out[7 * FRAME_SAMPLES..]);
        assert!((hz - 1000.0).abs() < 5.0, "{hz} Hz");
        // The concealed frame (two frames of silence come first) is sound, not a hole.
        let concealed = &out[12 * FRAME_SAMPLES..13 * FRAME_SAMPLES];
        assert!((0.1..0.7).contains(&peak(concealed)), "concealed frame peaks at {}", peak(concealed));
    }

    #[test]
    fn seq_wraps_around() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        let mut out = stream(&mut buffer, &tone_packets(u32::MAX - 9, 20));
        out.extend(pull_frames(&mut buffer, 2));
        assert_eq!(buffer.stats(), JitterStats { played: 20, ..Default::default() });
        assert!((frequency(&out[7 * FRAME_SAMPLES..]) - 1000.0).abs() < 5.0);
    }

    #[test]
    fn a_pause_is_silence_and_sound_starts_again() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        let packets = tone_packets(0, 20);
        let before = stream(&mut buffer, &packets[..10]);
        assert!(peak(&before) > 0.4);
        // The sender is quiet. What is held is played, two frames are made up, then nothing
        // but silence.
        let quiet = pull_frames(&mut buffer, 30);
        assert_eq!(peak(&quiet[5 * FRAME_SAMPLES..]), 0.0);
        assert_eq!(buffer.stats(), JitterStats { played: 10, concealed: 2, late: 0, skipped: 0 });
        // It goes on where it stopped. There is no hole in seq, so nothing more is concealed,
        // and the packets the made-up frames stood in for are not late.
        let mut after = stream(&mut buffer, &packets[10..]);
        after.extend(pull_frames(&mut buffer, 2));
        assert_eq!(peak(&after[..2 * FRAME_SAMPLES]), 0.0, "plays before it is full again");
        assert!(peak(&after) > 0.4);
        assert_eq!(buffer.stats(), JitterStats { played: 20, concealed: 2, late: 0, skipped: 0 });
    }

    #[test]
    fn a_burst_after_a_stall_does_not_add_delay() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        let packets = tone_packets(0, 30);
        stream(&mut buffer, &packets[..10]);
        // The network stalls for 60 ms: the buffer runs dry and goes silent.
        pull_frames(&mut buffer, 6);
        assert_eq!(buffer.stats(), JitterStats { played: 10, concealed: 2, late: 0, skipped: 0 });
        // What was held up arrives at once, and the stream goes on at its pace.
        for p in &packets[10..16] {
            buffer.push(p.clone());
        }
        let out = stream(&mut buffer, &packets[16..]);
        // No more is held than before the stall; the oldest of the burst made way for that.
        assert!(buffer.slots.len() <= 3, "{} frames held", buffer.slots.len());
        assert_eq!(buffer.stats().skipped, 4);
        assert_eq!(buffer.stats().late, 0);
        assert!(peak(&out) > 0.4);
    }

    #[test]
    fn a_few_packets_short_of_the_depth_still_play() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        for p in tone_packets(0, 2) {
            buffer.push(p);
        }
        let out = pull_frames(&mut buffer, 8);
        assert_eq!(peak(&out[..3 * FRAME_SAMPLES]), 0.0);
        assert!(peak(&out[3 * FRAME_SAMPLES..6 * FRAME_SAMPLES]) > 0.0);
        assert_eq!(buffer.stats().played, 2);
    }

    #[test]
    fn stays_bounded_when_nobody_listens() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        for p in tone_packets(0, 500) {
            buffer.push(p);
        }
        assert!(buffer.slots.len() <= buffer.limit());
        // What is left is the newest sound.
        assert_eq!(buffer.next_seq.wrapping_add(buffer.slots.len() as u32), 500);
        assert!(buffer.stats().skipped > 480);
    }

    #[test]
    fn a_sender_that_starts_over_is_followed() {
        let mut buffer = JitterBuffer::new(Channels::Mono, 3).unwrap();
        stream(&mut buffer, &tone_packets(5000, 10));
        pull_frames(&mut buffer, 2);
        // A new sender, counting from zero: far behind, but not late.
        let mut out = stream(&mut buffer, &tone_packets(0, 10));
        out.extend(pull_frames(&mut buffer, 2));
        assert!(peak(&out) > 0.4);
        assert_eq!(buffer.stats(), JitterStats { played: 20, ..Default::default() });
    }
}
