//! The client core as a C interface, for Swift and anything else that isn't Rust.
//! `include/remoteman_core.h` is the header; keep the two in step by hand.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;

use crate::{Button, Chroma, Client, Codec, CursorShape, Display, Event, Frame, Handler, Mode, Options, StreamInfo, TouchPhase};

/// The app's callbacks. They are called from the core's threads, one at a time, with `ctx`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RmCallbacks {
    pub ctx: *mut c_void,
    /// codec: 0 HEVC, 1 AV1. chroma: 420 or 444. hdr: 0 BT.709, 1 BT.2020 PQ.
    pub on_stream: Option<extern "C" fn(ctx: *mut c_void, epoch: u16, codec: u8, chroma: u16, depth: u8, width: u32, height: u32, refresh: u32, hdr: u8)>,
    /// `data` is valid only during the call. `age_us` is -1 until the clocks have been compared.
    pub on_frame: Option<extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, key: u8, number: u32, capture_us: u64, age_us: i64)>,
    /// `reason` is valid only during the call. Nothing is called after this.
    pub on_closed: Option<extern "C" fn(ctx: *mut c_void, reason: *const c_char)>,
    /// The host's cursor image: `width * height * 4` bytes of premultiplied RGBA, rows top to
    /// bottom, valid only during the call. `width` 0: no cursor. Setting this callback is what
    /// asks the host to leave the cursor out of the picture.
    pub on_cursor_shape: Option<extern "C" fn(ctx: *mut c_void, rgba: *const u8, width: u32, height: u32, hot_x: i32, hot_y: i32)>,
    /// Where the host's cursor is, as fractions of the display; `visible` 0 when it has left it.
    pub on_cursor_at: Option<extern "C" fn(ctx: *mut c_void, x: f32, y: f32, visible: u8)>,
}

// SAFETY: the app promises its context may be used from another thread, one call at a time.
unsafe impl Send for RmCallbacks {}

impl Handler for RmCallbacks {
    fn stream(&mut self, s: &StreamInfo) {
        if let Some(f) = self.on_stream {
            let chroma = if s.mode.chroma == Chroma::C444 { 444 } else { 420 };
            f(self.ctx, s.epoch, (s.mode.codec == Codec::Av1) as u8, chroma, s.mode.depth, s.width, s.height, s.refresh, (s.colour == "bt2020-pq") as u8);
        }
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if let Some(f) = self.on_frame {
            f(self.ctx, frame.data.as_ptr(), frame.data.len(), frame.key as u8, frame.number, frame.capture_us, age_us.unwrap_or(-1));
        }
    }

    fn cursor_shape(&mut self, s: &CursorShape) {
        if let Some(f) = self.on_cursor_shape {
            f(self.ctx, s.rgba.as_ptr(), s.width, s.height, s.hot_x, s.hot_y);
        }
    }

    fn cursor_at(&mut self, x: f32, y: f32, visible: bool) {
        if let Some(f) = self.on_cursor_at {
            f(self.ctx, x, y, visible as u8);
        }
    }

    fn closed(&mut self, reason: &str) {
        if let Some(f) = self.on_closed {
            let reason = CString::new(reason.replace('\0', " ")).unwrap_or_default();
            f(self.ctx, reason.as_ptr());
        }
    }
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

fn report(err: *mut c_char, len: usize, message: &str) {
    if err.is_null() || len == 0 {
        return;
    }
    let bytes = message.as_bytes();
    let n = bytes.len().min(len - 1);
    // SAFETY: the caller gave a writable buffer of `len` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), err as *mut u8, n);
        *err.add(n) = 0;
    }
}

/// Connect to `host` ("name:port") and say hello. Blocks until the host answers or the attempt
/// fails. `decoders` is a comma-separated list in order of preference, from `av1`, `hevc444`,
/// `hevc`. With `pair` non-zero, a host not seen before is accepted and remembered. Returns
/// null on failure, with the reason in `err`.
///
/// # Safety
/// The strings must be valid C strings and `err` a writable buffer of `err_len` bytes (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_connect(
    host: *const c_char,
    state_dir: *const c_char,
    name: *const c_char,
    pair: i32,
    width: u32,
    height: u32,
    scale: f32,
    refresh: u32,
    decoders: *const c_char,
    callbacks: RmCallbacks,
    err: *mut c_char,
    err_len: usize,
) -> *mut Client {
    let (host, state, name, decoders) = unsafe { (text(host), text(state_dir), text(name), text(decoders)) };
    let mut modes = Vec::new();
    for d in decoders.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        modes.push(match d {
            "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
            "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
            "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
            other => {
                report(err, err_len, &format!("unknown decoder {other}"));
                return std::ptr::null_mut();
            }
        });
    }
    let options = Options { host, state: PathBuf::from(state), name, pair: pair != 0, display: Display { width, height, scale, refresh }, decoders: modes, cursor: callbacks.on_cursor_shape.is_some() };
    match Client::connect(options, callbacks) {
        Ok(c) => Box::into_raw(Box::new(c)),
        Err(e) => {
            report(err, err_len, &format!("{e:#}"));
            std::ptr::null_mut()
        }
    }
}

/// The host's name, operating system and certificate fingerprint, as one line
/// "name\tos\tfingerprint", copied into `out`.
///
/// # Safety
/// `client` must come from `rm_client_connect`; `out` must be writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_host(client: *const Client, out: *mut c_char, len: usize) {
    let c = unsafe { &*client };
    report(out, len, &format!("{}\t{}\t{}", c.host_name, c.host_os, c.host_fingerprint));
}

/// Frames lost, keyframes asked for, and the shortest round trip in microseconds.
///
/// # Safety
/// `client` must come from `rm_client_connect`; the out pointers must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_stats(client: *const Client, lost: *mut u32, keyframe_requests: *mut u32, round_trip_us: *mut u32) {
    let s = unsafe { &*client }.stats();
    unsafe {
        *lost = s.lost;
        *keyframe_requests = s.keyframe_requests;
        *round_trip_us = s.round_trip_us;
    }
}

macro_rules! input {
    ($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) => $event:expr) => {
        $(#[$doc])*
        ///
        /// # Safety
        /// `client` must come from `rm_client_connect`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(client: *const Client, $($arg: $ty),*) {
            unsafe { &*client }.send($event);
        }
    };
}

input!(/// The pointer at (x, y), as fractions of the display from its top left.
    rm_client_pointer_to(x: f32, y: f32) => Event::PointerTo { x, y });
input!(/// The pointer moved by (dx, dy) display pixels.
    rm_client_pointer_by(dx: f32, dy: f32) => Event::PointerBy { dx, dy });
input!(/// A mouse button: 0 left, 1 right, 2 middle.
    rm_client_button(button: u8, down: u8) => Event::Button { button: match button { 1 => Button::Right, 2 => Button::Middle, _ => Button::Left }, down: down != 0 });
input!(/// Scrolling by (dx, dy) display pixels; positive dy moves towards the end of a document.
    rm_client_scroll(dx: f32, dy: f32) => Event::Scroll { dx, dy });
input!(/// A key by USB HID usage (page 7), modifiers included.
    rm_client_key(usage: u16, down: u8) => Event::Key { usage, down: down != 0 });
input!(/// A finger: phase 0 down, 1 move, 2 up; `slot` tells fingers apart.
    rm_client_touch(phase: u8, slot: u8, x: f32, y: f32) => Event::Touch { phase: match phase { 0 => TouchPhase::Down, 2 => TouchPhase::Up, _ => TouchPhase::Move }, slot, x, y });

/// Text typed through an on-screen keyboard or input method (UTF-8).
///
/// # Safety
/// `client` must come from `rm_client_connect`; `utf8` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_text(client: *const Client, utf8: *const c_char) {
    unsafe { &*client }.send(Event::Text(unsafe { text(utf8) }));
}

/// Leave and free the client. `on_closed` is called before this returns or shortly after; no
/// callback comes once it has been.
///
/// # Safety
/// `client` must come from `rm_client_connect` and not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_close(client: *mut Client) {
    if !client.is_null() {
        unsafe { Box::from_raw(client) }.close();
    }
}
