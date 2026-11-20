//! Fingers on this PC's touchscreen, over the computer's picture, sent as the protocol's touches
//! (docs/PROTOCOL.md, "Input", type 6: phase, slot, x, y) when Settings says "Send touchscreen
//! touches as touch"; otherwise they stay the pointer, as Windows makes them. Every host takes
//! them (a host that has no touchscreen to put them into plays the first finger as the left
//! button), so no feature of the host's gates them.
//!
//! Windows numbers each contact with a pointer id that is never used again; the protocol wants
//! small slots that tell the fingers down now apart, as the Linux viewer's `finger_slot` gives
//! them.

use remoteman_core::TouchPhase;

/// The fingers the computer knows of: each one's pointer id, by slot.
#[derive(Default)]
pub struct Fingers {
    slots: Vec<Option<u32>>,
    /// Moves sent for each slot since it went down, for the log.
    moves: Vec<u32>,
    /// Where each slot's finger was last sent, so that a finger lifted for it is lifted there.
    places: Vec<(f32, f32)>,
}

impl Fingers {
    /// The slot of the finger `id` for an event of `phase`: a new slot (the lowest free) when it
    /// goes down, its slot while it moves and when it lifts (freed then). `None` for a finger
    /// that isn't known (it went down before touches were sent as touches).
    pub fn slot(&mut self, id: u32, phase: TouchPhase) -> Option<u8> {
        let at = self.slots.iter().position(|s| *s == Some(id));
        let slot = match (at, phase) {
            (Some(at), _) => at,
            (None, TouchPhase::Down) => match self.slots.iter().position(Option::is_none) {
                Some(free) => {
                    self.slots[free] = Some(id);
                    free
                }
                None if self.slots.len() < 32 => {
                    self.slots.push(Some(id));
                    self.moves.push(0);
                    self.slots.len() - 1
                }
                None => return None,
            },
            (None, _) => return None,
        };
        self.moves.resize(self.slots.len(), 0);
        match phase {
            TouchPhase::Down => self.moves[slot] = 0,
            TouchPhase::Move => self.moves[slot] += 1,
            TouchPhase::Up => self.slots[slot] = None,
        }
        Some(slot as u8)
    }

    /// Moves sent for the finger in `slot` since it went down.
    pub fn moves(&self, slot: u8) -> u32 {
        self.moves.get(slot as usize).copied().unwrap_or(0)
    }

    /// The finger in `slot` was sent at this place.
    pub fn place(&mut self, slot: u8, x: f32, y: f32) {
        if self.places.len() <= slot as usize {
            self.places.resize(slot as usize + 1, (0.0, 0.0));
        }
        self.places[slot as usize] = (x, y);
    }

    /// Where the finger in `slot` was last sent.
    pub fn last(&self, slot: u8) -> (f32, f32) {
        self.places.get(slot as usize).copied().unwrap_or((0.0, 0.0))
    }

    /// Whether the finger `id` is one the computer knows of.
    pub fn has(&self, id: u32) -> bool {
        self.slots.contains(&Some(id))
    }

    /// Every finger down, by slot with where it was last sent, forgotten: the caller lifts them
    /// on the computer.
    pub fn lift_all(&mut self) -> Vec<(u8, f32, f32)> {
        let down: Vec<(u8, f32, f32)> = self.slots.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| (i as u8, self.last(i as u8).0, self.last(i as u8).1)).collect();
        self.slots.iter_mut().for_each(|s| *s = None);
        down
    }
}

/// The phase as the app hands it over: 0 down, 1 move, anything else up (lifted, cancelled,
/// or the panel lost the pointer).
pub fn phase(n: i32) -> TouchPhase {
    match n {
        0 => TouchPhase::Down,
        1 => TouchPhase::Move,
        _ => TouchPhase::Up,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingers_get_slots_that_are_given_back() {
        let mut f = Fingers::default();
        assert_eq!(f.slot(500, TouchPhase::Down), Some(0));
        assert_eq!(f.slot(700, TouchPhase::Down), Some(1));
        assert_eq!(f.slot(500, TouchPhase::Move), Some(0));
        assert_eq!(f.slot(500, TouchPhase::Move), Some(0));
        assert_eq!(f.moves(0), 2);
        assert_eq!(f.slot(500, TouchPhase::Up), Some(0));
        // The lowest free slot is used again.
        assert_eq!(f.slot(900, TouchPhase::Down), Some(0));
        assert_eq!(f.slot(700, TouchPhase::Move), Some(1));
        // A finger nobody saw go down is nobody's.
        assert_eq!(f.slot(123, TouchPhase::Move), None);
        assert_eq!(f.slot(123, TouchPhase::Up), None);
        assert!(f.has(700) && !f.has(500));
        f.place(1, 0.25, 0.75);
        assert_eq!(f.lift_all(), [(0, 0.0, 0.0), (1, 0.25, 0.75)]);
        assert_eq!(f.slot(700, TouchPhase::Up), None);
        assert_eq!((phase(0), phase(1), phase(2), phase(3)), (TouchPhase::Down, TouchPhase::Move, TouchPhase::Up, TouchPhase::Up));
    }
}
