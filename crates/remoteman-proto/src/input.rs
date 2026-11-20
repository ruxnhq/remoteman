//! Input events: a `u8` type and then fixed fields, one after another on the input stream.

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchPhase {
    Down,
    Move,
    Up,
}

/// Where a pen is: on the surface (down, move, up) or above it (hover, leave).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PenPhase {
    Down,
    Move,
    Up,
    Hover,
    Leave,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The pointer at this spot, as fractions (0 to 1) of the display from its top left.
    PointerTo { x: f32, y: f32 },
    /// The pointer moved by this many display pixels.
    PointerBy { dx: f32, dy: f32 },
    Button { button: Button, down: bool },
    /// In display pixels; positive `dy` moves towards the end of a document.
    Scroll { dx: f32, dy: f32 },
    /// A USB HID usage on page 7, by physical position.
    Key { usage: u16, down: bool },
    Touch { phase: TouchPhase, slot: u8, x: f32, y: f32 },
    /// The pen: `buttons` bit 0 the barrel button, bit 1 the eraser end; `x`, `y` as for
    /// `PointerTo`; `pressure` 0 to 1; tilt in degrees (-90 to 90); `twist` 0 to 359 degrees.
    Pen { phase: PenPhase, buttons: u8, x: f32, y: f32, pressure: f32, tilt_x: f32, tilt_y: f32, twist: f32 },
    /// A game controller (docs/PROTOCOL.md, "Game controllers"): its whole state. `pad` 0 to 3;
    /// `flags` bit 0 connected (clear: that pad was unplugged); `buttons` as XInput's mask;
    /// triggers 0 to 1; sticks -1 to 1, up and right positive.
    Gamepad { pad: u8, flags: u8, buttons: u16, left_trigger: f32, right_trigger: f32, left_x: f32, left_y: f32, right_x: f32, right_y: f32 },
    /// Text from an on-screen keyboard or input method.
    Text(String),
    /// The pointer and touch events that follow are on this display of the session (0, the
    /// first, until said otherwise; 1, the second: docs/PROTOCOL.md, "A second display").
    Display(u8),
}

impl Event {
    pub fn write(&self, out: &mut Vec<u8>) {
        let f = |out: &mut Vec<u8>, v: f32| out.extend_from_slice(&v.to_le_bytes());
        match self {
            Event::PointerTo { x, y } => {
                out.push(1);
                f(out, *x);
                f(out, *y);
            }
            Event::PointerBy { dx, dy } => {
                out.push(2);
                f(out, *dx);
                f(out, *dy);
            }
            Event::Button { button, down } => out.extend_from_slice(&[3, *button as u8, *down as u8]),
            Event::Scroll { dx, dy } => {
                out.push(4);
                f(out, *dx);
                f(out, *dy);
            }
            Event::Key { usage, down } => {
                out.push(5);
                out.extend_from_slice(&usage.to_le_bytes());
                out.push(*down as u8);
            }
            Event::Touch { phase, slot, x, y } => {
                out.extend_from_slice(&[6, *phase as u8, *slot]);
                f(out, *x);
                f(out, *y);
            }
            Event::Pen { phase, buttons, x, y, pressure, tilt_x, tilt_y, twist } => {
                out.extend_from_slice(&[9, *phase as u8, *buttons]);
                for v in [x, y, pressure, tilt_x, tilt_y, twist] {
                    f(out, *v);
                }
            }
            Event::Gamepad { pad, flags, buttons, left_trigger, right_trigger, left_x, left_y, right_x, right_y } => {
                out.extend_from_slice(&[10, *pad, *flags]);
                out.extend_from_slice(&buttons.to_le_bytes());
                for v in [left_trigger, right_trigger, left_x, left_y, right_x, right_y] {
                    f(out, *v);
                }
            }
            Event::Text(s) => {
                // Cut where a character ends: half a character is text no host can read,
                // and a host that can't read an event stops taking input.
                let mut end = s.len().min(u16::MAX as usize);
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                let s = &s.as_bytes()[..end];
                out.push(7);
                out.extend_from_slice(&(s.len() as u16).to_le_bytes());
                out.extend_from_slice(s);
            }
            Event::Display(index) => out.extend_from_slice(&[8, *index]),
        }
    }

    /// How long the event at the front of `buf` is, if its type is known and enough of it is
    /// there to say: what a reader steps over when the event is whole but can't be read (a
    /// button or touch phase it doesn't know, text that isn't UTF-8).
    pub fn length(buf: &[u8]) -> Option<usize> {
        match *buf.first()? {
            1 | 2 | 4 => Some(9),
            3 => Some(3),
            5 => Some(4),
            6 => Some(11),
            9 => Some(27),
            10 => Some(29),
            8 => Some(2),
            7 if buf.len() >= 3 => Some(3 + u16::from_le_bytes([buf[1], buf[2]]) as usize),
            _ => None,
        }
    }

    /// Parse one event from the front of `buf`. Returns the event and how many bytes it took,
    /// or `None` if `buf` doesn't hold a whole event yet.
    pub fn parse(buf: &[u8]) -> Result<Option<(Self, usize)>, Error> {
        let Some(&ty) = buf.first() else { return Ok(None) };
        let need = match ty {
            1 | 2 | 4 => 9,
            3 => 3,
            5 => 4,
            6 => 11,
            9 => 27,
            10 => 29,
            8 => 2,
            7 => {
                if buf.len() < 3 {
                    return Ok(None);
                }
                3 + u16::from_le_bytes([buf[1], buf[2]]) as usize
            }
            other => return Err(Error::Unknown("input event", other as u32)),
        };
        if buf.len() < need {
            return Ok(None);
        }
        let f = |i: usize| f32::from_le_bytes(buf[i..i + 4].try_into().unwrap());
        let ev = match ty {
            1 => Event::PointerTo { x: f(1), y: f(5) },
            2 => Event::PointerBy { dx: f(1), dy: f(5) },
            3 => Event::Button {
                button: match buf[1] {
                    0 => Button::Left,
                    1 => Button::Right,
                    2 => Button::Middle,
                    b => return Err(Error::Unknown("button", b as u32)),
                },
                down: buf[2] != 0,
            },
            4 => Event::Scroll { dx: f(1), dy: f(5) },
            5 => Event::Key { usage: u16::from_le_bytes([buf[1], buf[2]]), down: buf[3] != 0 },
            8 => Event::Display(buf[1]),
            9 => Event::Pen {
                phase: match buf[1] {
                    0 => PenPhase::Down,
                    1 => PenPhase::Move,
                    2 => PenPhase::Up,
                    3 => PenPhase::Hover,
                    4 => PenPhase::Leave,
                    p => return Err(Error::Unknown("pen phase", p as u32)),
                },
                buttons: buf[2],
                x: f(3),
                y: f(7),
                pressure: f(11),
                tilt_x: f(15),
                tilt_y: f(19),
                twist: f(23),
            },
            10 => Event::Gamepad {
                pad: buf[1],
                flags: buf[2],
                buttons: u16::from_le_bytes([buf[3], buf[4]]),
                left_trigger: f(5),
                right_trigger: f(9),
                left_x: f(13),
                left_y: f(17),
                right_x: f(21),
                right_y: f(25),
            },
            6 => Event::Touch {
                phase: match buf[1] {
                    0 => TouchPhase::Down,
                    1 => TouchPhase::Move,
                    2 => TouchPhase::Up,
                    p => return Err(Error::Unknown("touch phase", p as u32)),
                },
                slot: buf[2],
                x: f(3),
                y: f(7),
            },
            _ => Event::Text(std::str::from_utf8(&buf[3..need]).map_err(|_| Error::Text)?.to_owned()),
        };
        Ok(Some((ev, need)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_round_trips_back_to_back() {
        let events = [
            Event::PointerTo { x: 0.25, y: 0.75 },
            Event::PointerBy { dx: -3.5, dy: 12.0 },
            Event::Button { button: Button::Right, down: true },
            Event::Scroll { dx: 0.0, dy: 40.0 },
            Event::Key { usage: 0xE3, down: false },
            Event::Touch { phase: TouchPhase::Move, slot: 2, x: 0.5, y: 0.5 },
            Event::Pen { phase: PenPhase::Hover, buttons: 3, x: 0.1, y: 0.9, pressure: 0.0, tilt_x: -45.5, tilt_y: 90.0, twist: 359.0 },
            Event::Gamepad { pad: 3, flags: 1, buttons: 0x9001, left_trigger: 0.5, right_trigger: 1.0, left_x: -1.0, left_y: 0.25, right_x: 0.0, right_y: -0.75 },
            Event::Text("ა é 🙂".into()),
        ];
        let mut buf = Vec::new();
        for e in &events {
            e.write(&mut buf);
        }
        let mut at = 0;
        for e in &events {
            let (got, used) = Event::parse(&buf[at..]).unwrap().unwrap();
            assert_eq!(&got, e);
            at += used;
        }
        assert_eq!(at, buf.len());
    }

    #[test]
    fn long_text_is_cut_where_a_character_ends() {
        // 65535 bytes would end in the middle of the last "é" (two bytes each).
        let mut b = Vec::new();
        Event::Text("é".repeat(40000)).write(&mut b);
        let (got, used) = Event::parse(&b).expect("the text written can't be read back").unwrap();
        assert_eq!(used, b.len());
        assert_eq!(got, Event::Text("é".repeat(32767)));
    }

    #[test]
    fn a_whole_event_that_cannot_be_read_has_a_length() {
        assert!(Event::parse(&[3, 9, 1]).is_err());
        assert_eq!(Event::length(&[3, 9, 1]), Some(3));
        assert_eq!(Event::length(&[7, 2, 0, 0xff, 0xfe]), Some(5));
        assert_eq!(Event::length(&[7, 2]), None);
        assert_eq!(Event::length(&[99, 0, 0]), None);
    }

    #[test]
    fn layout_and_partial_input() {
        let mut b = Vec::new();
        Event::Key { usage: 4, down: true }.write(&mut b);
        assert_eq!(b, [5, 4, 0, 1]);
        assert_eq!(Event::parse(&b[..3]).unwrap(), None);
        assert_eq!(Event::parse(&[]).unwrap(), None);
        assert!(Event::parse(&[99]).is_err());
    }
}

#[cfg(test)]
mod pen_tests {
    use super::*;

    #[test]
    fn a_pen_event_round_trips_in_every_phase_and_with_every_button() {
        for (n, phase) in [PenPhase::Down, PenPhase::Move, PenPhase::Up, PenPhase::Hover, PenPhase::Leave].into_iter().enumerate() {
            for buttons in 0..4u8 {
                let e = Event::Pen { phase, buttons, x: 0.25, y: 0.75, pressure: 0.5, tilt_x: -30.0, tilt_y: 45.0, twist: 270.0 };
                let mut b = Vec::new();
                e.write(&mut b);
                assert_eq!(b.len(), 27);
                assert_eq!(&b[..3], [9, n as u8, buttons]);
                assert_eq!(Event::length(&b), Some(27));
                assert_eq!(Event::parse(&b[..26]).unwrap(), None);
                assert_eq!(Event::parse(&b).unwrap(), Some((e, 27)));
            }
        }
    }

    #[test]
    fn the_pen_layout_is_little_endian_after_two_bytes() {
        let mut b = Vec::new();
        Event::Pen { phase: PenPhase::Move, buttons: 1, x: 1.0, y: 0.0, pressure: 0.0, tilt_x: 0.0, tilt_y: 0.0, twist: 0.0 }.write(&mut b);
        assert_eq!(&b[..7], [9, 1, 1, 0, 0, 0x80, 0x3f]);
        // A phase nobody knows is a whole event that can't be read: it has a length to skip.
        b[1] = 7;
        assert!(Event::parse(&b).is_err());
        assert_eq!(Event::length(&b), Some(27));
    }
}

#[cfg(test)]
mod gamepad_tests {
    use super::*;

    #[test]
    fn a_gamepad_event_is_29_bytes_little_endian_and_round_trips() {
        let e = Event::Gamepad { pad: 2, flags: 1, buttons: 0x8001, left_trigger: 1.0, right_trigger: 0.0, left_x: 0.0, left_y: 0.0, right_x: 0.0, right_y: -1.0 };
        let mut b = Vec::new();
        e.write(&mut b);
        assert_eq!(b.len(), 29);
        assert_eq!(Event::length(&b), Some(29));
        assert_eq!(&b[..5], &[10, 2, 1, 0x01, 0x80]);
        assert_eq!(&b[5..9], &1.0f32.to_le_bytes());
        assert_eq!(&b[25..29], &(-1.0f32).to_le_bytes());
        assert_eq!(Event::parse(&b[..28]).unwrap(), None);
        assert_eq!(Event::parse(&b).unwrap(), Some((e, 29)));
        // An unplugged pad is a state with the flag clear.
        let gone = Event::Gamepad { pad: 0, flags: 0, buttons: 0, left_trigger: 0.0, right_trigger: 0.0, left_x: 0.0, left_y: 0.0, right_x: 0.0, right_y: 0.0 };
        let mut b = Vec::new();
        gone.write(&mut b);
        assert_eq!(Event::parse(&b).unwrap().unwrap().0, gone);
    }
}
