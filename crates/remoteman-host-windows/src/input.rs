//! The client's input, put into the desktop with `SendInput` (and touches with
//! `InjectTouchInput`).
//!
//! Where this works and where it doesn't. `SendInput` reaches the desktop of the session the
//! calling process is in, and only the input desktop of that session. So this host must run in
//! the console's session, not in session 0. The sign-in screen, the lock screen and Windows'
//! own permission prompts are on the Winlogon desktop, which only a SYSTEM process attached to
//! that desktop may put input into: the service (`service.rs`) starts this host as SYSTEM in
//! the console's session, and each injection first follows the input desktop
//! (`crate::desk::follow`). A host started any other way reaches the user's desktop only.
//! Windows also drops input meant for a window of a more privileged process, which doesn't
//! arise here because the display driver already makes this host run elevated.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use remoteman_core::{Button, Event, PenPhase, TouchPhase};
use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY, VK_PAUSE,
};
use windows::Win32::UI::Input::Pointer::{
    InitializeTouchInjection, InjectSyntheticPointerInput, InjectTouchInput, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_NEW, POINTER_FLAG_SECONDBUTTON, POINTER_PEN_INFO, POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS, POINTER_INFO, POINTER_TOUCH_INFO,
    TOUCH_FEEDBACK_NONE,
};
use windows::Win32::UI::Controls::{CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT, POINTER_TYPE_INFO};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_ROTATION, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, PT_PEN, PT_TOUCH, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, TOUCH_MASK_CONTACTAREA};

use crate::display::Area;

/// A synthetic pen device (`CreateSyntheticPointerDevice`), destroyed when dropped.
struct PenDevice(HSYNTHETICPOINTERDEVICE);

// SAFETY: the handle is only used through `&mut self` of the injector that owns it, and
// Windows lets any thread of the process use it.
unsafe impl Send for PenDevice {}

impl PenDevice {
    fn create() -> windows::core::Result<Self> {
        // SAFETY: plain call: one pen, with Windows' own feedback (the pen cursor).
        unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT) }.map(Self)
    }
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        // SAFETY: a device `create` made, destroyed once.
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}

/// Where the pen is, as far as Windows has been told.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PenState {
    /// Not in range of the screen.
    Away,
    Hovering,
    Down,
}

/// How a key is given to Windows.
#[derive(Clone, Copy)]
enum Key {
    /// A scancode of the PC keyboard (set 1), and whether it is one of the "extended" keys
    /// (sent with an E0 prefix: the arrows, the right-hand modifiers, the keypad's Enter...).
    Scan(u16, bool),
    /// By its virtual key: Pause, whose scancode is a sequence `SendInput` can't express.
    Virtual(VIRTUAL_KEY),
}

/// A USB HID usage (page 7, the key's physical position) as Windows wants the same key. By
/// scancode, so that the desktop's own keyboard layout decides what the key types, as it does
/// for a keyboard plugged into the PC.
fn key(usage: u16) -> Option<Key> {
    const fn s(code: u16) -> Option<Key> {
        Some(Key::Scan(code, false))
    }
    const fn e(code: u16) -> Option<Key> {
        Some(Key::Scan(code, true))
    }
    // Letters, in the usages' alphabetical order.
    const LETTERS: [u16; 26] = [0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C];
    match usage {
        0x04..=0x1D => s(LETTERS[(usage - 0x04) as usize]),
        // 1 to 9, then 0.
        0x1E..=0x27 => s(usage - 0x1E + 0x02),
        0x28 => s(0x1C), // Enter
        0x29 => s(0x01), // Escape
        0x2A => s(0x0E), // Backspace
        0x2B => s(0x0F), // Tab
        0x2C => s(0x39), // Space
        0x2D => s(0x0C), // -
        0x2E => s(0x0D), // =
        0x2F => s(0x1A), // [
        0x30 => s(0x1B), // ]
        0x31 | 0x32 => s(0x2B), // \ and the ISO key beside Enter
        0x33 => s(0x27), // ;
        0x34 => s(0x28), // '
        0x35 => s(0x29), // `
        0x36 => s(0x33), // ,
        0x37 => s(0x34), // .
        0x38 => s(0x35), // /
        0x39 => s(0x3A), // Caps Lock
        0x3A..=0x43 => s(usage - 0x3A + 0x3B), // F1 to F10
        0x44 => s(0x57), // F11
        0x45 => s(0x58), // F12
        0x46 => e(0x37), // Print Screen
        0x47 => s(0x46), // Scroll Lock
        0x48 => Some(Key::Virtual(VK_PAUSE)),
        0x49 => e(0x52), // Insert
        0x4A => e(0x47), // Home
        0x4B => e(0x49), // Page Up
        0x4C => e(0x53), // Delete
        0x4D => e(0x4F), // End
        0x4E => e(0x51), // Page Down
        0x4F => e(0x4D), // Right
        0x50 => e(0x4B), // Left
        0x51 => e(0x50), // Down
        0x52 => e(0x48), // Up
        0x53 => s(0x45), // Num Lock
        0x54 => e(0x35), // Keypad /
        0x55 => s(0x37), // Keypad *
        0x56 => s(0x4A), // Keypad -
        0x57 => s(0x4E), // Keypad +
        0x58 => e(0x1C), // Keypad Enter
        0x59 => s(0x4F), // Keypad 1 to 9, 0, .
        0x5A => s(0x50),
        0x5B => s(0x51),
        0x5C => s(0x4B),
        0x5D => s(0x4C),
        0x5E => s(0x4D),
        0x5F => s(0x47),
        0x60 => s(0x48),
        0x61 => s(0x49),
        0x62 => s(0x52),
        0x63 => s(0x53),
        0x64 => s(0x56), // the ISO key beside the left Shift
        0x65 => e(0x5D), // Menu
        0x68..=0x72 => s(usage - 0x68 + 0x64), // F13 to F23
        0x73 => s(0x76), // F24
        0x7F => e(0x20), // Mute
        0x80 => e(0x30), // Volume up
        0x81 => e(0x2E), // Volume down
        0x87 => s(0x73), // International 1 to 5 (Japanese keyboards)
        0x88 => s(0x70),
        0x89 => s(0x7D),
        0x8A => s(0x79),
        0x8B => s(0x7B),
        0xE0 => s(0x1D), // Left Control
        0xE1 => s(0x2A), // Left Shift
        0xE2 => s(0x38), // Left Alt
        0xE3 => e(0x5B), // Left Windows
        0xE4 => e(0x1D), // Right Control
        0xE5 => s(0x36), // Right Shift
        0xE6 => e(0x38), // Right Alt
        0xE7 => e(0x5C), // Right Windows
        _ => None,
    }
}

fn key_input(key: Key, down: bool) -> INPUT {
    let up = if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP };
    let ki = match key {
        Key::Scan(code, extended) => KEYBDINPUT { wScan: code, dwFlags: KEYEVENTF_SCANCODE | up | if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() },
        Key::Virtual(vk) => KEYBDINPUT { wVk: vk, dwFlags: up, ..Default::default() },
    };
    INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki } }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, ..Default::default() } } }
}

/// One notch of a mouse wheel is 120 units to Windows, and scrolls about three lines. The
/// client sends pixels; this many of them are taken for a notch. Programs that scroll smoothly
/// take the units as they come, the others add them up to whole notches.
const PIXELS_PER_NOTCH: f32 = 50.0;

pub struct Injector {
    /// Where the client's display is on the Windows desktop.
    area: Arc<Mutex<Area>>,
    /// What the client holds down, to let go of when it leaves.
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    /// Parts of a pixel and of a wheel unit not yet passed on.
    moved: (f32, f32),
    scrolled: (f32, f32),
    /// Fingers on the screen, by the client's slot: where each is, in desktop pixels.
    touches: BTreeMap<u8, POINT>,
    /// Whether touch injection could be set up; not tried until the first touch.
    touch: Option<bool>,
    /// The synthetic pen device, made at the first pen event; and whether making it failed.
    pen_device: Option<PenDevice>,
    pen_failed: bool,
    pen_state: PenState,
    /// The virtual game controller (gamepad.rs).
    gamepad: crate::gamepad::Gamepad,
    /// Events Windows didn't take, and whether that has been said.
    refused: u32,
}

impl Injector {
    pub fn new(area: Arc<Mutex<Area>>) -> Self {
        Self { area, keys: BTreeSet::new(), buttons: BTreeSet::new(), moved: (0.0, 0.0), scrolled: (0.0, 0.0), touches: BTreeMap::new(), touch: None, pen_device: None, pen_failed: false, pen_state: PenState::Away, gamepad: Default::default(), refused: 0 }
    }

    fn send(&mut self, inputs: &[INPUT]) {
        // Started by the service: onto the desktop that takes input now (the sign-in or lock
        // screen's, or the user's).
        crate::desk::follow();
        // SAFETY: a slice of whole INPUTs and their size.
        let taken = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
        if taken as usize != inputs.len() {
            self.refused += 1;
            if self.refused == 1 {
                tracing::warn!("Windows didn't take an input event ({}); is this process in the desktop's session, and is the desktop the user's (not the sign-in or a permission prompt)? Said once.", windows::core::Error::from_thread());
            }
        }
    }

    /// A spot on the client's display, as fractions, in desktop pixels.
    fn pixel(&self, x: f32, y: f32) -> POINT {
        let a = *self.area.lock().unwrap();
        let on = |f: f32, size: u32| ((f.clamp(0.0, 1.0) * size as f32) as i32).min(size as i32 - 1);
        POINT { x: a.left + on(x, a.width), y: a.top + on(y, a.height) }
    }

    pub fn inject(&mut self, event: &Event) {
        match event {
            Event::PointerTo { x, y } => {
                // Absolute positions are in 65536ths of the whole desktop (all monitors).
                let p = self.pixel(*x, *y);
                // SAFETY: no arguments to get wrong.
                let (left, top, width, height) = unsafe { (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN), GetSystemMetrics(SM_CXVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN)) };
                let unit = |at: i32, from: i32, size: i32| (((at - from) as i64 * 65535 + (size as i64 - 1) / 2) / (size as i64 - 1).max(1)) as i32;
                self.send(&[mouse_input(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, unit(p.x, left, width), unit(p.y, top, height), 0)]);
            }
            Event::PointerBy { dx, dy } => {
                // Whole pixels now, the rest with the next move. (Windows applies the user's
                // pointer speed and acceleration to relative moves, as it does to a mouse.)
                self.moved = (self.moved.0 + dx, self.moved.1 + dy);
                let (x, y) = (self.moved.0.trunc(), self.moved.1.trunc());
                if x != 0.0 || y != 0.0 {
                    self.moved = (self.moved.0 - x, self.moved.1 - y);
                    self.send(&[mouse_input(MOUSEEVENTF_MOVE, x as i32, y as i32, 0)]);
                }
            }
            Event::Button { button, down } => {
                let flags = match (button, down) {
                    (Button::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (Button::Left, false) => MOUSEEVENTF_LEFTUP,
                    (Button::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (Button::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (Button::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (Button::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                if *down {
                    self.buttons.insert(*button as u8);
                } else {
                    self.buttons.remove(&(*button as u8));
                }
                self.send(&[mouse_input(flags, 0, 0, 0)]);
            }
            Event::Scroll { dx, dy } => {
                // Towards the end of a document is a negative wheel turn; to the right is a
                // positive one on the other wheel.
                self.scrolled = (self.scrolled.0 + dx * 120.0 / PIXELS_PER_NOTCH, self.scrolled.1 - dy * 120.0 / PIXELS_PER_NOTCH);
                let (x, y) = (self.scrolled.0.trunc(), self.scrolled.1.trunc());
                self.scrolled = (self.scrolled.0 - x, self.scrolled.1 - y);
                if y != 0.0 {
                    self.send(&[mouse_input(MOUSEEVENTF_WHEEL, 0, 0, y as i32)]);
                }
                if x != 0.0 {
                    self.send(&[mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, x as i32)]);
                }
            }
            Event::Key { usage, down } => {
                let Some(k) = key(*usage) else {
                    tracing::debug!(usage, "a key this host has no scancode for");
                    return;
                };
                // Ctrl+Alt+Del (HID Delete with a Ctrl and an Alt held): no program can type it,
                // only the service can make Windows see it.
                let held = |a: u16, b: u16| self.keys.contains(&a) || self.keys.contains(&b);
                if *down && *usage == 0x4C && held(0xE0, 0xE4) && held(0xE2, 0xE6) {
                    let asked = crate::desk::secure_attention();
                    tracing::info!(asked, "Ctrl+Alt+Del from the device");
                    return;
                }
                if *down {
                    self.keys.insert(*usage);
                } else {
                    self.keys.remove(usage);
                }
                self.send(&[key_input(k, *down)]);
            }
            Event::Text(text) => {
                // Each UTF-16 unit as a key that types it, whatever the keyboard layout.
                let unit = |c: u16, up: bool| INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: c, dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
                };
                // One character (down and up) at a time, as the other hosts do: sent as one batch,
                // Windows' newer text controls (Notepad's, Windows Terminal's) took the first
                // word and then the last character over and over ("remoteman rrrr…" for
                // "remoteman from the Mac viewer", seen 2026-11-19).
                let units: Vec<u16> = text.encode_utf16().collect();
                for (i, &c) in units.iter().enumerate() {
                    // (A surrogate pair goes as one: the two halves together.)
                    if (0xDC00..0xE000).contains(&c) && i > 0 {
                        continue;
                    }
                    let pair = (0xD800..0xDC00).contains(&c) && units.get(i + 1).is_some_and(|n| (0xDC00..0xE000).contains(n));
                    let inputs: Vec<INPUT> = if pair { vec![unit(c, false), unit(c, true), unit(units[i + 1], false), unit(units[i + 1], true)] } else { vec![unit(c, false), unit(c, true)] };
                    self.send(&inputs);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
            }
            Event::Touch { phase, slot, x, y } => self.touch(*phase, *slot, *x, *y),
            Event::Pen { phase, buttons, x, y, pressure, tilt_x, tilt_y, twist } => self.pen(*phase, *buttons, *x, *y, *pressure, *tilt_x, *tilt_y, *twist),
            Event::Gamepad { .. } => self.gamepad.inject(event),
            // This host gives a device one display, so there is nothing to choose between.
            Event::Display(_) => {}
        }
    }

    /// One finger went down, moved or lifted. Windows wants every finger that is down in each
    /// report, so the others are sent along as they were.
    ///
    /// Not seen working yet, and one thing is known to be missing: Windows drops an injected
    /// finger that isn't reported again for a while, so one held still for long is lifted by
    /// Windows unless the client keeps sending moves for it.
    fn touch(&mut self, phase: TouchPhase, slot: u8, x: f32, y: f32) {
        if self.touch.is_none() {
            // SAFETY: plain call. Ten fingers, and no touch marks drawn by Windows.
            let ready = unsafe { InitializeTouchInjection(10, TOUCH_FEEDBACK_NONE) };
            if let Err(e) = &ready {
                tracing::warn!("touches from the client can't be passed on (InitializeTouchInjection: {e})");
            }
            self.touch = Some(ready.is_ok());
        }
        if self.touch != Some(true) {
            return;
        }
        let at = self.pixel(x, y);
        let at = match phase {
            TouchPhase::Up => self.touches.get(&slot).copied().unwrap_or(at),
            _ => at,
        };
        // A move for a finger that never went down, or a second down: make it what it must be.
        let known = self.touches.contains_key(&slot);
        let changed = match (phase, known) {
            (TouchPhase::Up, false) => return,
            (TouchPhase::Up, true) => POINTER_FLAG_UP,
            (_, false) => POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
            (_, true) => POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT,
        };
        let contact = |id: u8, p: POINT, flags: POINTER_FLAGS| POINTER_TOUCH_INFO {
            pointerInfo: POINTER_INFO { pointerType: PT_TOUCH, pointerId: id as u32, pointerFlags: flags, ptPixelLocation: p, ..Default::default() },
            touchMask: TOUCH_MASK_CONTACTAREA,
            // A fingertip of a few pixels around the spot.
            rcContact: RECT { left: p.x - 2, top: p.y - 2, right: p.x + 2, bottom: p.y + 2 },
            ..Default::default()
        };
        let mut contacts = vec![contact(slot, at, changed)];
        contacts.extend(self.touches.iter().filter(|(id, _)| **id != slot).map(|(id, p)| contact(*id, *p, POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT)));
        if phase == TouchPhase::Up {
            self.touches.remove(&slot);
        } else {
            self.touches.insert(slot, at);
        }
        crate::desk::follow();
        // SAFETY: a slice of whole contacts.
        if let Err(e) = unsafe { InjectTouchInput(&contacts) } {
            self.refused += 1;
            if self.refused == 1 {
                tracing::warn!("Windows didn't take a touch ({e}). Said once.");
            }
        }
    }


    /// The pen, through a synthetic pen device made at the first event. Hovering is "in range"
    /// without "in contact"; leaving the range is an update without "in range". A lift is
    /// made up when the client goes from down to hovering or away without saying so, and a
    /// contact when a move comes for a pen that isn't down.
    #[allow(clippy::too_many_arguments)]
    fn pen(&mut self, phase: PenPhase, buttons: u8, x: f32, y: f32, pressure: f32, tilt_x: f32, tilt_y: f32, twist: f32) {
        if self.pen_device.is_none() {
            if self.pen_failed {
                return;
            }
            // The desktop the device is made on is the one that takes input now.
            crate::desk::follow();
            match PenDevice::create() {
                Ok(d) => self.pen_device = Some(d),
                Err(e) => {
                    self.pen_failed = true;
                    tracing::warn!("pen events from the client can't be passed on (CreateSyntheticPointerDevice: {e}). Said once.");
                    return;
                }
            }
        }
        let at = self.pixel(x, y);
        // The steps to get the pen from where it is to where the client says.
        let mut steps: Vec<(PenState, POINTER_FLAGS)> = Vec::new();
        let was = self.pen_state;
        let in_range = POINTER_FLAG_INRANGE;
        let contact = POINTER_FLAG_INCONTACT | POINTER_FLAG_FIRSTBUTTON | POINTER_FLAG_INRANGE;
        let new = if was == PenState::Away { POINTER_FLAG_NEW } else { POINTER_FLAGS(0) };
        match phase {
            PenPhase::Down | PenPhase::Move if was == PenState::Down => steps.push((PenState::Down, POINTER_FLAG_UPDATE | contact)),
            PenPhase::Down | PenPhase::Move => steps.push((PenState::Down, POINTER_FLAG_DOWN | new | contact)),
            PenPhase::Up if was == PenState::Down => steps.push((PenState::Hovering, POINTER_FLAG_UP | in_range)),
            PenPhase::Up => return,
            PenPhase::Hover => {
                if was == PenState::Down {
                    steps.push((PenState::Hovering, POINTER_FLAG_UP | in_range));
                }
                steps.push((PenState::Hovering, POINTER_FLAG_UPDATE | new | in_range));
            }
            PenPhase::Leave => {
                if was == PenState::Away {
                    return;
                }
                if was == PenState::Down {
                    steps.push((PenState::Hovering, POINTER_FLAG_UP | in_range));
                }
                steps.push((PenState::Away, POINTER_FLAG_UPDATE));
            }
        }
        let mut pen_flags = 0;
        if buttons & 1 != 0 {
            pen_flags |= PEN_FLAG_BARREL;
        }
        if buttons & 2 != 0 {
            pen_flags |= PEN_FLAG_ERASER | PEN_FLAG_INVERTED;
        }
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        for (state, flags) in steps {
            let mut flags = flags;
            let touching = state == PenState::Down;
            if buttons & 1 != 0 && touching {
                flags |= POINTER_FLAG_SECONDBUTTON;
            }
            let info = POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO { pointerType: PT_PEN, pointerId: 0, pointerFlags: flags, ptPixelLocation: at, ..Default::default() },
                penFlags: pen_flags,
                penMask: PEN_MASK_PRESSURE | PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                pressure: if touching { (finite(pressure).clamp(0.0, 1.0) * 1024.0).round() as u32 } else { 0 },
                rotation: (finite(twist).round() as i32).rem_euclid(360) as u32,
                tiltX: finite(tilt_x).clamp(-90.0, 90.0).round() as i32,
                tiltY: finite(tilt_y).clamp(-90.0, 90.0).round() as i32,
            };
            let mut ti = POINTER_TYPE_INFO { r#type: PT_PEN, ..Default::default() };
            ti.Anonymous.penInfo = info;
            crate::desk::follow();
            // SAFETY: one whole POINTER_TYPE_INFO, for the device this injector made.
            let result = unsafe { InjectSyntheticPointerInput(self.pen_device.as_ref().unwrap().0, &[ti]) };
            if let Err(e) = result {
                self.refused += 1;
                if self.refused == 1 {
                    tracing::warn!("Windows didn't take a pen event ({e}); it may be on another desktop than the device was made on. Said once.");
                }
                // A new device (on the desktop that takes input now) at the next event.
                self.pen_device = None;
                self.pen_state = PenState::Away;
                return;
            }
            self.pen_state = state;
        }
    }

    /// Let go of everything the client held down: nothing may stay pressed when it leaves.
    pub fn release_all(&mut self) {
        for usage in std::mem::take(&mut self.keys) {
            self.inject(&Event::Key { usage, down: false });
        }
        for b in std::mem::take(&mut self.buttons) {
            let button = [Button::Left, Button::Right, Button::Middle][b as usize % 3];
            self.inject(&Event::Button { button, down: false });
        }
        for slot in self.touches.keys().copied().collect::<Vec<_>>() {
            self.touch(TouchPhase::Up, slot, 0.0, 0.0);
        }
        if self.pen_state != PenState::Away {
            self.pen(PenPhase::Leave, 0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        }
        // The device goes with the session.
        self.pen_device = None;
        self.gamepad.release();
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device's fractions land in the rectangle of the screen it is shown (a mirrored screen
    /// that isn't at the desktop's origin included), and never outside it.
    #[test]
    fn fractions_map_into_the_screens_rectangle() {
        let area = Arc::new(Mutex::new(Area { left: -2560, top: 300, width: 2560, height: 1440 }));
        let injector = Injector::new(area.clone());
        let p = injector.pixel(0.0, 0.0);
        assert_eq!((p.x, p.y), (-2560, 300));
        let p = injector.pixel(0.5, 0.5);
        assert_eq!((p.x, p.y), (-1280, 1020));
        let p = injector.pixel(1.0, 1.0);
        assert_eq!((p.x, p.y), (-1, 1739));
        let p = injector.pixel(7.0, -3.0);
        assert_eq!((p.x, p.y), (-1, 300));
        // The screen changing size under the stream: the next event follows it.
        *area.lock().unwrap() = Area { left: 0, top: 0, width: 1920, height: 1080 };
        let p = injector.pixel(0.5, 0.5);
        assert_eq!((p.x, p.y), (960, 540));
    }

    /// Makes and destroys the synthetic pen device, and injects nothing.
    #[test]
    fn a_synthetic_pen_device_can_be_made_and_destroyed() {
        let device = PenDevice::create().expect("CreateSyntheticPointerDevice(PT_PEN)");
        assert!(!device.0.0.is_null());
        drop(device);
    }
}
