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
            Event::Text(s) => {
                let s = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
                out.push(7);
                out.extend_from_slice(&(s.len() as u16).to_le_bytes());
                out.extend_from_slice(s);
            }
            Event::Display(index) => out.extend_from_slice(&[8, *index]),
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
    fn layout_and_partial_input() {
        let mut b = Vec::new();
        Event::Key { usage: 4, down: true }.write(&mut b);
        assert_eq!(b, [5, 4, 0, 1]);
        assert_eq!(Event::parse(&b[..3]).unwrap(), None);
        assert_eq!(Event::parse(&[]).unwrap(), None);
        assert!(Event::parse(&[99]).is_err());
    }
}
