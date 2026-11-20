//! Game controllers held at this PC, sent to the computer: XInput (`XInputGetState`, which needs
//! no window or focus of its own) polled 250 times a second on a thread of its own, so that the
//! state goes out within a few milliseconds of a change whatever the interface is doing.
//!
//! The engine says (`Pads::update`, from its timer) whether the controllers are the computer's
//! now: the session window is in front, the picture is the computer's, the setting is on and the
//! computer lists `gamepad`. A state goes whenever it changes and every 100 ms while anything is
//! off neutral (docs/PROTOCOL.md, "Game controllers"); when the controllers stop being the
//! computer's, each one that was in use sends a neutral state, so nothing stays pressed there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remoteman_core::{Client, Event};
use windows::Win32::UI::Input::XboxController::{XINPUT_GAMEPAD, XINPUT_STATE, XInputGetState};

const POLL: Duration = Duration::from_millis(4);
const REPEAT: Duration = Duration::from_millis(100);
/// A slot with nothing in it is looked at this often: asking is slow on some systems.
const EMPTY_SLOT: Duration = Duration::from_secs(1);
/// Sticks this close to the middle are the middle: a worn stick never quite rests there.
const DEAD: f32 = 0.05;

/// One controller's state as the wire has it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct State {
    pub buttons: u16,
    pub triggers: (f32, f32),
    pub left: (f32, f32),
    pub right: (f32, f32),
}

impl State {
    pub fn from_xinput(p: &XINPUT_GAMEPAD) -> Self {
        let stick = |x: i16, y: i16| {
            let f = |v: i16| (v as f32 / 32767.0).clamp(-1.0, 1.0);
            let (x, y) = (f(x), f(y));
            if x.hypot(y) < DEAD { (0.0, 0.0) } else { (x, y) }
        };
        Self { buttons: p.wButtons.0, triggers: (p.bLeftTrigger as f32 / 255.0, p.bRightTrigger as f32 / 255.0), left: stick(p.sThumbLX, p.sThumbLY), right: stick(p.sThumbRX, p.sThumbRY) }
    }

    pub fn neutral(&self) -> bool {
        *self == Self { buttons: 0, triggers: (0.0, 0.0), left: (0.0, 0.0), right: (0.0, 0.0) }
    }

    pub fn event(&self, pad: u8, connected: bool) -> Event {
        Event::Gamepad { pad, flags: connected as u8, buttons: self.buttons, left_trigger: self.triggers.0, right_trigger: self.triggers.1, left_x: self.left.0, left_y: self.left.1, right_x: self.right.0, right_y: self.right.1 }
    }
}

struct Shared {
    /// Whether the controllers are the computer's now, and where they go.
    want: AtomicBool,
    to: Mutex<Option<Arc<Client>>>,
    quit: AtomicBool,
}

pub struct Pads(Arc<Shared>);

impl Pads {
    pub fn start() -> Self {
        let shared = Arc::new(Shared { want: AtomicBool::new(false), to: Mutex::new(None), quit: AtomicBool::new(false) });
        let s = shared.clone();
        let _ = std::thread::Builder::new().name("controllers".into()).spawn(move || run(&s));
        Self(shared)
    }

    /// Called often: where the controllers go now (`None`: nowhere).
    pub fn update(&self, to: Option<Arc<Client>>) {
        self.0.want.store(to.is_some(), Ordering::Relaxed);
        *self.0.to.lock().unwrap() = to;
    }
}

impl Drop for Pads {
    fn drop(&mut self) {
        self.0.quit.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Copy)]
struct Slot {
    /// What was last sent, and when; `None` while nothing is plugged in (or it was sent as gone).
    sent: Option<(State, Instant)>,
    /// When an empty slot was last looked at.
    looked: Option<Instant>,
    /// Whether the last look found a controller.
    present: bool,
}

fn run(shared: &Shared) {
    let mut slots = [Slot { sent: None, looked: None, present: false }; 4];
    // Where the controllers went last, to let go of what is held there when they stop going.
    let mut last: Option<Arc<Client>> = None;
    while !shared.quit.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        let to = shared.to.lock().unwrap().clone();
        let Some(client) = to else {
            if let Some(old) = last.take() {
                for (i, slot) in slots.iter_mut().enumerate() {
                    if let Some((state, _)) = slot.sent.take() {
                        if !state.neutral() {
                            old.send(State { buttons: 0, triggers: (0.0, 0.0), left: (0.0, 0.0), right: (0.0, 0.0) }.event(i as u8, true));
                        }
                    }
                    slot.present = false;
                    slot.looked = None;
                }
            }
            continue;
        };
        last = Some(client.clone());
        let now = Instant::now();
        for (i, slot) in slots.iter_mut().enumerate() {
            if !slot.present && slot.looked.is_some_and(|t| now.duration_since(t) < EMPTY_SLOT) {
                continue;
            }
            let mut s = XINPUT_STATE::default();
            // SAFETY: a slot number and a state to fill.
            let ok = unsafe { XInputGetState(i as u32, &mut s) } == 0;
            slot.looked = Some(now);
            slot.present = ok;
            if !ok {
                if let Some((gone, _)) = slot.sent.take() {
                    client.send(gone.event(i as u8, false));
                }
                continue;
            }
            let state = State::from_xinput(&s.Gamepad);
            let due = match slot.sent {
                None => true,
                Some((was, at)) => was != state || (!state.neutral() && now.duration_since(at) >= REPEAT),
            };
            if due {
                client.send(state.event(i as u8, true));
                slot.sent = Some((state, now));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xinput_states_become_wire_states() {
        let mut g = XINPUT_GAMEPAD::default();
        assert!(State::from_xinput(&g).neutral());
        g.wButtons.0 = 0x1000;
        g.bLeftTrigger = 255;
        g.sThumbLX = -32768;
        g.sThumbRY = 1000;
        let s = State::from_xinput(&g);
        assert_eq!((s.buttons, s.triggers.0, s.left.0, s.right), (0x1000, 1.0, -1.0, (0.0, 0.0)));
        assert!(!s.neutral());
        assert!(matches!(s.event(1, true), Event::Gamepad { pad: 1, flags: 1, buttons: 0x1000, .. }));
    }
}
