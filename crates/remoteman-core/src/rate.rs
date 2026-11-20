//! The bitrate following the network (docs/PROTOCOL.md, "Settings"): the host's picture is
//! encoded at a *target* that falls when the client reports lost frames and climbs back, in small
//! steps, after a stretch without. The ceiling is what the client asked for with `configure`
//! (or the host's own default); the target never goes above it.
//!
//! The policy is additive increase, multiplicative decrease, with every constant below.
//! A pure struct: time comes in as milliseconds from any clock that only goes forward.

/// What a loss event multiplies the target by.
pub const CUT_PERCENT: u64 = 80;
/// Loss events closer together than this count as one (a client repeats `lost` every 40 ms
/// until the host has answered, and that answer takes a round trip and an encode).
pub const CUT_GAP_MS: u64 = 500;
/// The target never goes below this, or below [`FLOOR_PERCENT`] of the ceiling, whichever is more
/// (and never above the ceiling).
pub const FLOOR_BPS: u32 = 2_000_000;
pub const FLOOR_PERCENT: u64 = 15;
/// No loss for this long, and the target starts to climb.
pub const CLEAN_MS: u64 = 2_000;
/// A climb is a step this often, each one this much: about 6% a second.
pub const RAISE_EVERY_MS: u64 = 500;
pub const RAISE_PERCENT: u64 = 103;

#[derive(Debug, Clone)]
pub struct RateController {
    ceiling: u32,
    target: u32,
    /// When the last loss event was seen (even one that cut nothing).
    last_loss: Option<u64>,
    /// When the target last changed.
    last_change: Option<u64>,
}

impl RateController {
    /// Start at the ceiling, which must be above 0.
    pub fn new(ceiling: u32) -> Self {
        Self { ceiling, target: ceiling, last_loss: None, last_change: None }
    }

    pub fn target(&self) -> u32 {
        self.target
    }

    pub fn ceiling(&self) -> u32 {
        self.ceiling
    }

    fn floor(&self) -> u32 {
        ((self.ceiling as u64 * FLOOR_PERCENT / 100) as u32).max(FLOOR_BPS).min(self.ceiling)
    }

    /// The client reported lost frames. Returns the new target if it changed.
    pub fn loss(&mut self, now_ms: u64) -> Option<u32> {
        self.last_loss = Some(now_ms);
        if self.last_change.is_some_and(|t| now_ms.saturating_sub(t) < CUT_GAP_MS) && self.target < self.ceiling {
            return None;
        }
        let target = ((self.target as u64 * CUT_PERCENT / 100) as u32).max(self.floor());
        if target >= self.target {
            return None;
        }
        self.target = target;
        self.last_change = Some(now_ms);
        Some(target)
    }

    /// Call now and then (a few times a second). Returns the new target if it climbed.
    pub fn tick(&mut self, now_ms: u64) -> Option<u32> {
        if self.target >= self.ceiling {
            return None;
        }
        if self.last_loss.is_some_and(|t| now_ms.saturating_sub(t) < CLEAN_MS) || self.last_change.is_some_and(|t| now_ms.saturating_sub(t) < RAISE_EVERY_MS) {
            return None;
        }
        self.target = ((self.target as u64 * RAISE_PERCENT / 100) as u32).max(self.target + 1).min(self.ceiling);
        self.last_change = Some(now_ms);
        Some(self.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loss_cuts_by_a_fifth_and_repeats_within_half_a_second_count_once() {
        let mut c = RateController::new(40_000_000);
        assert_eq!(c.loss(1000), Some(32_000_000));
        assert_eq!(c.loss(1040), None);
        assert_eq!(c.loss(1400), None);
        assert_eq!(c.target(), 32_000_000);
        assert_eq!(c.loss(1500), Some(25_600_000));
    }

    #[test]
    fn the_target_stops_at_the_floor() {
        let mut c = RateController::new(40_000_000);
        let mut t = 0;
        for _ in 0..50 {
            t += 600;
            c.loss(t);
        }
        // 15% of 40 Mbit/s.
        assert_eq!(c.target(), 6_000_000);
        let mut small = RateController::new(8_000_000);
        for i in 0..50 {
            small.loss(i * 600);
        }
        assert_eq!(small.target(), 2_000_000);
        // A ceiling below the floor is the floor.
        let mut tiny = RateController::new(1_000_000);
        assert_eq!(tiny.loss(0), None);
        assert_eq!(tiny.target(), 1_000_000);
    }

    #[test]
    fn it_climbs_back_only_after_a_clean_stretch_and_stops_at_the_ceiling() {
        let mut c = RateController::new(20_000_000);
        c.loss(0);
        assert_eq!(c.target(), 16_000_000);
        // Not for the first two seconds.
        for t in (0..2000).step_by(250) {
            assert_eq!(c.tick(t), None, "at {t}");
        }
        assert_eq!(c.tick(2000), Some(16_480_000));
        // No faster than a step each half second.
        assert_eq!(c.tick(2250), None);
        assert_eq!(c.tick(2500), Some(16_974_400));
        // A loss during the climb cuts again and starts the wait over.
        let cut = c.loss(3000).unwrap();
        assert!(cut < 16_974_400);
        assert_eq!(c.tick(4900), None);
        // Left alone it gets all the way back, and no further.
        let mut t = 5000;
        while c.target() < 20_000_000 {
            c.tick(t);
            t += 250;
            assert!(t < 120_000, "never got back");
        }
        assert_eq!(c.target(), 20_000_000);
        assert_eq!(c.tick(t + 10_000), None);
    }

    #[test]
    fn a_cut_after_a_long_clean_run_at_the_ceiling_is_not_held_back_by_the_gap() {
        let mut c = RateController::new(10_000_000);
        assert_eq!(c.loss(100), Some(8_000_000));
        // Climbed all the way back, then another loss right at a step.
        let mut t = 100;
        while c.target() < 10_000_000 {
            t += 250;
            c.tick(t);
        }
        assert_eq!(c.loss(t + 1), Some(8_000_000));
    }
}
