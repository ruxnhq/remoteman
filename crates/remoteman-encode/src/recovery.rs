//! What a host does about a client's loss reports, apart from any one encoder: which frames to
//! keep as long-term ones, and how to answer a loss without a keyframe (and so without a burst)
//! whenever the encoder still holds a frame the client has.
//!
//! This is the loop of the Linux agent's pipeline, taken out so that the Windows host follows
//! the same rules. The Linux agent still carries its own copy and is the one to move here next.
//!
//! For each frame: [`Recovery::plan`] before encoding, then [`Recovery::made`] with what the
//! encoder did.

use anyhow::Result;

use crate::How;

/// How to encode the next frame, and what it is to the client.
#[derive(Debug, Clone, Copy)]
pub struct Plan {
    pub how: How,
    /// It answers a loss report without being a keyframe: it leans on nothing the client missed.
    pub recovery: bool,
}

/// What an encoded frame is sent as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    Key,
    /// Not a keyframe, but the client can pick the picture up again at it.
    Recovery,
    Delta,
}

pub struct Recovery {
    /// Recent frames the encoder can fall back on.
    short: u64,
    /// Long-term frames in use: frames the encoder keeps when told, so that a loss reported
    /// too late for the recent ones still doesn't cost a keyframe.
    slots: usize,
    /// The long-term frames the client holds too, by frame number.
    kept: [Option<u64>; 2],
    kept_last: u64,
    /// A new long-term frame this often, in frames.
    every: u64,
    /// The number of the next frame.
    n: u64,
    /// The last frame sent that a client can restart from (a keyframe or recovery point).
    answer: Option<u64>,
    pub tally: Tally,
}

impl Recovery {
    /// For an encoder that keeps this many recent and long-term frames, at `fps` frames a second.
    pub fn new(short_term: u32, long_term: u32, fps: u32) -> Self {
        let slots = (long_term as usize).min(2);
        // A new long-term frame four times a second: with two, one is always older than any
        // loss reported within a quarter of a second. With room for only one, twice a second:
        // a late report then finds it too new now and then, and gets a keyframe.
        let every = (fps as u64 / if slots > 1 { 4 } else { 2 }).max(8);
        Self { short: short_term as u64, slots, kept: [None; 2], kept_last: 0, every, n: 0, answer: None, tally: Tally::default() }
    }

    /// Another encoder takes over the same stream (opened again, as at another rate where the
    /// encoder can't change it live): its own recent and long-term frames, none kept yet, and the
    /// stream's frame numbers, last answer and tally carried on, since the client's are. Its
    /// first frame has to be a keyframe.
    pub fn renewed(&mut self, short_term: u32, long_term: u32, fps: u32) {
        let (n, answer, tally) = (self.n, self.answer, std::mem::take(&mut self.tally));
        *self = Self { n, answer, tally, ..Self::new(short_term, long_term, fps) };
    }

    /// The number of the next frame: its timestamp for the encoder.
    pub fn number(&self) -> u64 {
        self.n
    }

    /// Decide how the next frame is encoded. `idr`: a keyframe was asked for. `lost`: a loss
    /// report not yet acted on, as the first frame the client missed and the newest it had seen
    /// when it said so. `invalidate` tells the encoder to stop referring to one frame.
    pub fn plan(&mut self, idr: bool, lost: Option<(u32, u32)>, invalidate: &mut dyn FnMut(u64) -> Result<()>) -> Plan {
        let n = self.n;
        let mut how = How { idr, ..How::default() };
        let mut recovery = false;
        if let (Some((first, last)), false) = (lost, how.idr) {
            // The client missed frames from `first` on, and nothing sent since is any use to
            // it either.
            let (first, last) = (first as u64, last as u64);
            let behind = n.saturating_sub(first);
            self.tally.reports += 1;
            self.tally.behind[(behind as usize).min(self.tally.behind.len() - 1)] += 1;
            if self.answer.is_some_and(|a| last < a) {
                // Said before the answer already sent could have reached the client.
                self.tally.early += 1;
            } else {
                // Stop referring to anything from the loss on, long-term frames included. Then
                // carry on from the newest frame the client does have: a recent one if the
                // encoder still holds it, else a long-term one. No keyframe, so no burst.
                // Failing both, start afresh.
                let dropped = first < n && (first..n).try_for_each(&mut *invalidate).is_ok();
                for k in &mut self.kept {
                    if k.is_some_and(|f| f >= first) {
                        *k = None;
                    }
                }
                let old = (0..self.slots).filter(|&i| self.kept[i].is_some()).max_by_key(|&i| self.kept[i]);
                if !dropped {
                    how.idr = true;
                    self.tally.refused += 1;
                } else if behind < self.short {
                    recovery = true;
                    self.tally.recovered += 1;
                } else if let Some(slot) = old {
                    how.refer = 1 << slot;
                    recovery = true;
                    self.tally.long_term += 1;
                } else {
                    how.idr = true;
                    self.tally.too_old += 1;
                }
            }
        }
        if how.idr {
            // A keyframe empties the encoder's store.
            self.kept = [None; 2];
        }
        if self.slots > 0 && how.refer == 0 && (self.kept[..self.slots].contains(&None) || n - self.kept_last >= self.every) {
            // Replace the older one (or fill an empty place).
            how.mark = (0..self.slots).min_by_key(|&i| self.kept[i]).map(|i| i as u32);
        }
        Plan { how, recovery }
    }

    /// The frame planned was encoded: `key`, `marked` and `referred` are what the encoder says
    /// it made. Returns what to send it as, and counts it.
    pub fn made(&mut self, plan: &Plan, key: bool, marked: Option<u32>, referred: u32) -> Sent {
        let n = self.n;
        if key && !plan.how.idr {
            self.kept = [None; 2];
            if n > 0 {
                self.tally.unasked += 1;
            }
        }
        if let Some(slot) = marked.filter(|s| (*s as usize) < self.slots) {
            self.kept[slot as usize] = Some(n);
            self.kept_last = n;
        }
        if plan.how.refer != 0 && !key && referred != 0 && referred != plan.how.refer {
            // Built from another long-term frame than the one asked for (the HEVC encoder
            // doesn't say which it used, so nothing to compare there): it may lean on
            // something the client lacks.
            self.tally.unsure += 1;
        }
        if key || plan.recovery {
            self.answer = Some(n);
        }
        self.n += 1;
        if key {
            Sent::Key
        } else if plan.recovery {
            Sent::Recovery
        } else {
            Sent::Delta
        }
    }
}

/// What became of the client's loss reports, for the log.
#[derive(Debug, Default)]
pub struct Tally {
    pub reports: u32,
    /// Answered from a recent frame, and from a long-term frame.
    pub recovered: u32,
    pub long_term: u32,
    /// Came before the last answer could have arrived.
    pub early: u32,
    /// Too old for anything the encoder holds (keyframe), and refused by the encoder (keyframe).
    pub too_old: u32,
    pub refused: u32,
    /// Keyframes the encoder made unasked.
    pub unasked: u32,
    /// Long-term answers built from another frame than asked.
    pub unsure: u32,
    /// How many frames had been encoded since the lost one when each report came (the last
    /// entry: that many or more).
    pub behind: [u32; 12],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode `count` frames with nothing lost, as an encoder that does what it is told.
    fn run(r: &mut Recovery, count: u32) {
        for _ in 0..count {
            let idr = r.number() == 0;
            let plan = r.plan(idr, None, &mut |_| Ok(()));
            r.made(&plan, plan.how.idr, plan.how.mark, 0);
        }
    }

    #[test]
    fn a_recent_loss_is_answered_from_a_recent_frame() {
        let mut r = Recovery::new(4, 2, 120);
        run(&mut r, 100);
        let mut dropped = Vec::new();
        let plan = r.plan(false, Some((98, 99)), &mut |t| {
            dropped.push(t);
            Ok(())
        });
        assert_eq!(dropped, [98, 99]);
        assert!(plan.recovery && !plan.how.idr && plan.how.refer == 0);
        assert_eq!(r.made(&plan, false, None, 0), Sent::Recovery);
        // The same loss said again, by a client that hadn't seen the answer yet: nothing new.
        let again = r.plan(false, Some((98, 99)), &mut |_| panic!("already answered"));
        assert!(!again.recovery && !again.how.idr);
        assert_eq!(r.tally.early, 1);
    }

    #[test]
    fn an_old_loss_is_answered_from_a_long_term_frame() {
        let mut r = Recovery::new(4, 2, 120);
        run(&mut r, 100);
        // Long-term frames are marked every 30 frames from the first two on: 0, 1, 31, 61, 91.
        let plan = r.plan(false, Some((80, 99)), &mut |_| Ok(()));
        assert!(plan.recovery && !plan.how.idr);
        assert_ne!(plan.how.refer, 0, "built from the long-term frame kept before the loss");
        assert_eq!(plan.how.mark, None);
        assert_eq!(r.tally.long_term, 1);
    }

    #[test]
    fn with_nothing_left_it_is_a_keyframe() {
        // No long-term frames at all, and the loss is older than the recent ones.
        let mut r = Recovery::new(4, 0, 60);
        run(&mut r, 50);
        let plan = r.plan(false, Some((10, 49)), &mut |_| Ok(()));
        assert!(plan.how.idr && !plan.recovery);
        assert_eq!(r.made(&plan, true, None, 0), Sent::Key);
        // An encoder that refuses to drop its references: a keyframe as well.
        let mut r = Recovery::new(4, 2, 60);
        run(&mut r, 50);
        let plan = r.plan(false, Some((48, 49)), &mut |_| anyhow::bail!("no"));
        assert!(plan.how.idr);
        assert_eq!(r.tally.refused, 1);
    }

    #[test]
    fn another_encoder_carries_the_stream_on() {
        let mut r = Recovery::new(4, 2, 120);
        run(&mut r, 100);
        assert!(r.kept.iter().all(Option::is_some));
        r.renewed(1 << 20, 0, 120);
        // The frame numbers go on, nothing is kept from the old encoder, the tally stays.
        assert_eq!(r.number(), 100);
        assert_eq!((r.slots, r.kept, r.short), (0, [None; 2], 1 << 20));
        let plan = r.plan(true, None, &mut |_| Ok(()));
        assert_eq!(r.made(&plan, true, None, 0), Sent::Key);
        assert_eq!(r.number(), 101);
        // A loss reported from before the new encoder's keyframe could have arrived is early.
        let plan = r.plan(false, Some((95, 99)), &mut |_| Ok(()));
        assert!(!plan.how.idr && !plan.recovery);
        assert_eq!((r.tally.reports, r.tally.early), (1, 1));
    }
}
