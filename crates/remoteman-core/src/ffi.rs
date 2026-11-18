//! The core as a C interface, for Swift and anything else that isn't Rust: the client role
//! (`rm_client_*`) and the host role (`rm_host_*`). `include/remoteman_core.h` is the header;
//! keep the two in step by hand.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;

use crate::{BlobEvent, BlobSender, Button, Chroma, Client, Codec, CursorShape, Display, Event, Frame, Handler, Message, Mode, Options, Packet, Placement, StreamInfo, TouchPhase};

/// The app's callbacks. They are called from the core's threads, one at a time, with `ctx`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RmCallbacks {
    pub ctx: *mut c_void,
    /// codec: 0 HEVC, 1 AV1. chroma: 420 or 444. hdr: 0 BT.709, 1 BT.2020 PQ.
    pub on_stream: Option<extern "C" fn(ctx: *mut c_void, epoch: u16, codec: u8, chroma: u16, depth: u8, width: u32, height: u32, refresh: u32, hdr: u8)>,
    /// `data` is valid only during the call. `key`: bit 0 a keyframe, bit 1 a recovery point.
    /// `age_us` is -1 until the clocks have been compared.
    pub on_frame: Option<extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, key: u8, number: u32, capture_us: u64, age_us: i64)>,
    /// `reason` is valid only during the call. Nothing is called after this.
    pub on_closed: Option<extern "C" fn(ctx: *mut c_void, reason: *const c_char)>,
    /// The host's cursor image: `width * height * 4` bytes of premultiplied RGBA, rows top to
    /// bottom, valid only during the call. `width` 0: no cursor. Setting this callback is what
    /// asks the host to leave the cursor out of the picture.
    pub on_cursor_shape: Option<extern "C" fn(ctx: *mut c_void, rgba: *const u8, width: u32, height: u32, hot_x: i32, hot_y: i32)>,
    /// Where the host's cursor is, as fractions of the display; `visible` 0 when it has left it.
    pub on_cursor_at: Option<extern "C" fn(ctx: *mut c_void, x: f32, y: f32, visible: u8)>,
    /// One of the host's feature messages, as its JSON (docs/PROTOCOL.md, "Features"), valid
    /// only during the call.
    pub on_message: Option<extern "C" fn(ctx: *mut c_void, json: *const c_char)>,
    /// One packet of the desktop's sound (Opus), valid only during the call. `start` 1: the
    /// first after a pause.
    pub on_sound: Option<extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, start: u8, seq: u32, time_us: u64)>,
    /// A blob from the host. `what` 0: opened (`kind`, `header` as JSON); 1: a piece of its
    /// content (`data`, `len`); 2: closed (`complete` 1 if it is all there). `id` tells blobs
    /// apart. Pointers are valid only during the call.
    pub on_blob: Option<extern "C" fn(ctx: *mut c_void, what: u8, id: u32, kind: u8, header: *const c_char, data: *const u8, len: usize, complete: u8)>,
}

// SAFETY: the app promises its context may be used from another thread, one call at a time.
unsafe impl Send for RmCallbacks {}

type OnBlob = extern "C" fn(ctx: *mut c_void, what: u8, id: u32, kind: u8, header: *const c_char, data: *const u8, len: usize, complete: u8);

fn give_blob(f: OnBlob, ctx: *mut c_void, event: &BlobEvent) {
    match event {
        BlobEvent::Open { id, kind, header } => {
            let header = CString::new(header.replace('\0', " ")).unwrap_or_default();
            f(ctx, 0, *id, *kind, header.as_ptr(), std::ptr::null(), 0, 0);
        }
        BlobEvent::Data { id, bytes } => f(ctx, 1, *id, 0, std::ptr::null(), bytes.as_ptr(), bytes.len(), 0),
        BlobEvent::Close { id, complete } => f(ctx, 2, *id, 0, std::ptr::null(), std::ptr::null(), 0, *complete as u8),
    }
}

fn give_message(f: extern "C" fn(ctx: *mut c_void, json: *const c_char), ctx: *mut c_void, message: &Message) {
    if let Ok(json) = CString::new(serde_json::to_string(message).unwrap_or_default()) {
        f(ctx, json.as_ptr());
    }
}

impl Handler for RmCallbacks {
    fn stream(&mut self, s: &StreamInfo) {
        if let Some(f) = self.on_stream {
            let chroma = if s.mode.chroma == Chroma::C444 { 444 } else { 420 };
            f(self.ctx, s.epoch, (s.mode.codec == Codec::Av1) as u8, chroma, s.mode.depth, s.width, s.height, s.refresh, (s.colour == "bt2020-pq") as u8);
        }
    }

    fn frame(&mut self, frame: &Frame, age_us: Option<i64>) {
        if let Some(f) = self.on_frame {
            f(self.ctx, frame.data.as_ptr(), frame.data.len(), frame.key as u8 | (frame.recovery as u8) << 1, frame.number, frame.capture_us, age_us.unwrap_or(-1));
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

    fn message(&mut self, message: &Message) {
        if let Some(f) = self.on_message {
            give_message(f, self.ctx, message);
        }
    }

    fn sound(&mut self, p: &Packet) {
        if let Some(f) = self.on_sound {
            f(self.ctx, p.data.as_ptr(), p.data.len(), p.start as u8, p.seq, p.time_us);
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        if let Some(f) = self.on_blob {
            give_blob(f, self.ctx, event);
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
/// `hevc`. With `pair` non-zero, a host not seen before is accepted and remembered. `options` is
/// a list of `key=value` pairs separated by `;` (null or empty for none); so far
/// `placement=extend|main|only`, where the display sits among the host's own. Returns null on
/// failure, with the reason in `err`.
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
    options: *const c_char,
    callbacks: RmCallbacks,
    err: *mut c_char,
    err_len: usize,
) -> *mut Client {
    let (host, state, name, decoders, options) = unsafe { (text(host), text(state_dir), text(name), text(decoders), text(options)) };
    let mut placement = Placement::Extend;
    for pair in options.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        match pair.split_once('=') {
            Some(("placement", "extend")) => placement = Placement::Extend,
            Some(("placement", "main")) => placement = Placement::Main,
            Some(("placement", "only")) => placement = Placement::Only,
            _ => {
                report(err, err_len, &format!("unknown option {pair}"));
                return std::ptr::null_mut();
            }
        }
    }
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
    let options = Options { host, state: PathBuf::from(state), name, pair: pair != 0, display: Display { width, height, scale, refresh, placement }, decoders: modes, test_drop_one_in: None, test_delay_lost_ms: None, cursor: callbacks.on_cursor_shape.is_some() };
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

/// Send one feature message, as JSON (docs/PROTOCOL.md, "Features": `sound`, `mic`, `camera`,
/// `configure`, `clipboard`, `file_result`). Returns 1, or 0 if it isn't a message the core knows.
///
/// # Safety
/// `client` must come from `rm_client_connect`; `json` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_send_message(client: *const Client, json: *const c_char) -> i32 {
    match Message::parse(unsafe { text(json) }.as_bytes()) {
        Ok(Message::Unknown) | Err(_) => 0,
        Ok(m) => {
            unsafe { &*client }.send_message(m);
            1
        }
    }
}

/// Send one packet of this device's microphone (Opus). Never blocks.
///
/// # Safety
/// `client` must come from `rm_client_connect`; `data` must be readable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_send_mic(client: *const Client, data: *const u8, len: usize, start: u8, seq: u32, time_us: u64) {
    unsafe { &*client }.send_mic(start != 0, seq, time_us, unsafe { std::slice::from_raw_parts(data, len) });
}

/// Send one whole frame of this device's camera (H.264 or HEVC, Annex-B). Never blocks.
///
/// # Safety
/// `client` must come from `rm_client_connect`; `data` must be readable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_send_camera(client: *const Client, data: *const u8, len: usize, key: u8, time_us: u64) {
    unsafe { &*client }.send_camera(unsafe { std::slice::from_raw_parts(data, len) }, key != 0, time_us);
}

/// Start sending a blob to the host: `kind` 0x40 clipboard, 0x41 file; `header` its JSON.
/// Write the content with `rm_blob_write`, then `rm_blob_finish` (or `rm_blob_cancel`).
///
/// # Safety
/// `client` must come from `rm_client_connect`; `header` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_client_blob_open(client: *const Client, kind: u8, header: *const c_char) -> *mut BlobSender {
    Box::into_raw(Box::new(unsafe { &*client }.open_blob(kind, unsafe { text(header) })))
}

/// Send the next piece of a blob. Waits while the network is behind, so never call it from
/// the main thread or from one of the core's callbacks. Returns 0 once the other side is gone.
///
/// # Safety
/// `blob` must come from a `*_blob_open` and not have been finished or cancelled; `data` must
/// be readable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_blob_write(blob: *const BlobSender, data: *const u8, len: usize) -> i32 {
    unsafe { &*blob }.write(unsafe { std::slice::from_raw_parts(data, len) }) as i32
}

/// That was all of the blob. Frees it.
///
/// # Safety
/// `blob` must come from a `*_blob_open` and isn't used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_blob_finish(blob: *mut BlobSender) -> i32 {
    unsafe { Box::from_raw(blob) }.finish() as i32
}

/// Give the blob up: the receiver throws away what it has. Frees it.
///
/// # Safety
/// `blob` must come from a `*_blob_open` and isn't used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_blob_cancel(blob: *mut BlobSender) {
    drop(unsafe { Box::from_raw(blob) });
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

// ---------------------------------------------------------------------------------------------
// The host role.

use crate::host::{Accept, ClientInfo, Host, HostHandler, HostOptions, Link};

/// A client that said hello. The strings are valid only during the call.
#[repr(C)]
pub struct RmClientInfo {
    pub name: *const c_char,
    pub fingerprint: *const c_char,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub refresh: u32,
    /// What it decodes, most preferred first: a comma-separated list from `av1`, `hevc444`, `hevc`.
    pub decoders: *const c_char,
    /// Non-zero: it draws the cursor itself.
    pub cursor_apart: u8,
    /// Where its display should sit: 0 an extra display, 1 the main display, 2 the only
    /// display (this computer's own screens off while it is connected).
    pub placement: u8,
}

/// The host's answer, filled in by `on_client`.
#[repr(C)]
pub struct RmAccept {
    /// 0 HEVC, 1 AV1.
    pub codec: u8,
    pub depth: u8,
    /// 420 or 444.
    pub chroma: u16,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// 0 BT.709, 1 BT.2020 PQ.
    pub hdr: u8,
    /// When refusing: why, as a C string the client is shown.
    pub refuse: [c_char; 200],
}

/// One input event, as docs/PROTOCOL.md numbers them. Fields that a type doesn't use are zero.
#[repr(C)]
pub struct RmInput {
    /// 1 pointer to (x, y as fractions), 2 pointer by (x, y in pixels), 3 button, 4 scroll
    /// (x, y in pixels), 5 key, 6 touch (x, y as fractions), 7 text.
    pub kind: u8,
    /// 0 left, 1 right, 2 middle.
    pub button: u8,
    pub down: u8,
    /// 0 down, 1 move, 2 up.
    pub phase: u8,
    pub slot: u8,
    /// A USB HID usage on page 7.
    pub usage: u16,
    pub x: f32,
    pub y: f32,
    /// UTF-8, valid only during the call; null unless `kind` is 7.
    pub text: *const c_char,
}

/// The host program's callbacks, all called on one thread of the core's, one at a time. They
/// may block.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RmHostCallbacks {
    pub ctx: *mut c_void,
    /// Return non-zero having filled `accept` and started the picture, or zero having written
    /// a reason into `accept.refuse`.
    pub on_client: Option<extern "C" fn(ctx: *mut c_void, client: *const RmClientInfo, accept: *mut RmAccept) -> i32>,
    pub on_keyframe: Option<extern "C" fn(ctx: *mut c_void)>,
    pub on_input: Option<extern "C" fn(ctx: *mut c_void, input: *const RmInput)>,
    pub on_client_left: Option<extern "C" fn(ctx: *mut c_void)>,
    /// One of the client's feature messages, as its JSON, valid only during the call.
    pub on_message: Option<extern "C" fn(ctx: *mut c_void, json: *const c_char)>,
    /// One packet of the client's microphone (Opus), valid only during the call.
    pub on_mic: Option<extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, start: u8, seq: u32, time_us: u64)>,
    /// One whole frame of the client's camera, in order and decodable, valid only during the call.
    pub on_camera: Option<extern "C" fn(ctx: *mut c_void, data: *const u8, len: usize, key: u8, number: u32, time_us: u64)>,
    /// A blob from the client: as the client's `on_blob`.
    pub on_blob: Option<OnBlob>,
}

// SAFETY: the host program promises its context may be used from the core's handler thread.
unsafe impl Send for RmHostCallbacks {}

fn mode_name(m: &Mode) -> &'static str {
    match (m.codec, m.chroma) {
        (Codec::Av1, _) => "av1",
        (Codec::Hevc, Chroma::C444) => "hevc444",
        (Codec::Hevc, Chroma::C420) => "hevc",
    }
}

impl HostHandler for RmHostCallbacks {
    fn connected(&mut self, client: &ClientInfo, _link: Link) -> Result<Accept, String> {
        let Some(f) = self.on_client else { return Err("this host takes no clients".into()) };
        let c = |s: &str| CString::new(s.replace('\0', " ")).unwrap_or_default();
        let (name, fingerprint) = (c(&client.name), c(&client.fingerprint));
        let decoders = c(&client.decoders.iter().map(mode_name).collect::<Vec<_>>().join(","));
        let info = RmClientInfo {
            name: name.as_ptr(),
            fingerprint: fingerprint.as_ptr(),
            width: client.display.width,
            height: client.display.height,
            scale: client.display.scale,
            refresh: client.display.refresh,
            decoders: decoders.as_ptr(),
            cursor_apart: client.cursor as u8,
            placement: client.display.placement as u8,
        };
        let mut a = RmAccept { codec: 0, depth: 10, chroma: 420, width: 0, height: 0, refresh: 0, hdr: 0, refuse: [0; 200] };
        if f(self.ctx, &info, &mut a) == 0 {
            a.refuse[199] = 0;
            // SAFETY: terminated just above.
            let why = unsafe { CStr::from_ptr(a.refuse.as_ptr()) }.to_string_lossy().into_owned();
            return Err(if why.is_empty() { "the host refused".into() } else { why });
        }
        Ok(accepted(&a))
    }

    fn keyframe(&mut self) {
        if let Some(f) = self.on_keyframe {
            f(self.ctx);
        }
    }

    fn input(&mut self, event: Event) {
        let Some(f) = self.on_input else { return };
        let mut i = RmInput { kind: 0, button: 0, down: 0, phase: 0, slot: 0, usage: 0, x: 0.0, y: 0.0, text: std::ptr::null() };
        let mut text = None;
        match event {
            Event::PointerTo { x, y } => (i.kind, i.x, i.y) = (1, x, y),
            Event::PointerBy { dx, dy } => (i.kind, i.x, i.y) = (2, dx, dy),
            Event::Button { button, down } => (i.kind, i.button, i.down) = (3, button as u8, down as u8),
            Event::Scroll { dx, dy } => (i.kind, i.x, i.y) = (4, dx, dy),
            Event::Key { usage, down } => (i.kind, i.usage, i.down) = (5, usage, down as u8),
            Event::Touch { phase, slot, x, y } => (i.kind, i.phase, i.slot, i.x, i.y) = (6, phase as u8, slot, x, y),
            Event::Text(s) => {
                i.kind = 7;
                text = Some(CString::new(s.replace('\0', " ")).unwrap_or_default());
            }
        }
        if let Some(t) = &text {
            i.text = t.as_ptr();
        }
        f(self.ctx, &i);
    }

    fn message(&mut self, message: &Message) {
        if let Some(f) = self.on_message {
            give_message(f, self.ctx, message);
        }
    }

    fn mic(&mut self, p: &Packet) {
        if let Some(f) = self.on_mic {
            f(self.ctx, p.data.as_ptr(), p.data.len(), p.start as u8, p.seq, p.time_us);
        }
    }

    fn camera(&mut self, frame: &Frame) {
        if let Some(f) = self.on_camera {
            f(self.ctx, frame.data.as_ptr(), frame.data.len(), frame.key as u8, frame.number, frame.capture_us);
        }
    }

    fn blob(&mut self, event: &BlobEvent) {
        if let Some(f) = self.on_blob {
            give_blob(f, self.ctx, event);
        }
    }

    fn left(&mut self) {
        if let Some(f) = self.on_client_left {
            f(self.ctx);
        }
    }
}

fn accepted(a: &RmAccept) -> Accept {
    Accept {
        mode: Mode { codec: if a.codec == 1 { Codec::Av1 } else { Codec::Hevc }, chroma: if a.chroma == 444 { Chroma::C444 } else { Chroma::C420 }, depth: a.depth },
        width: a.width,
        height: a.height,
        refresh: a.refresh,
        colour: if a.hdr != 0 { "bt2020-pq" } else { "bt709" }.into(),
    }
}

/// Send one feature message to the client, as JSON (`sound_format`, `camera_keyframe`,
/// `status`, `file_result`). Returns 1, or 0 if no client is connected or the core doesn't
/// know the message.
///
/// # Safety
/// `host` must come from `rm_host_start`; `json` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_send_message(host: *const Host, json: *const c_char) -> i32 {
    match (unsafe { &*host }.link(), Message::parse(unsafe { text(json) }.as_bytes())) {
        (_, Ok(Message::Unknown) | Err(_)) | (None, _) => 0,
        (Some(l), Ok(m)) => {
            l.send_message(m);
            1
        }
    }
}

/// Send one packet of the desktop's sound (Opus). Never blocks.
///
/// # Safety
/// `host` must come from `rm_host_start`; `data` must be readable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_send_sound(host: *const Host, data: *const u8, len: usize, start: u8, seq: u32, time_us: u64) {
    if let Some(l) = unsafe { &*host }.link() {
        l.send_sound(start != 0, seq, time_us, unsafe { std::slice::from_raw_parts(data, len) });
    }
}

/// Start sending a blob to the client; as `rm_client_blob_open`. Null if no client is connected.
///
/// # Safety
/// `host` must come from `rm_host_start`; `header` must be a valid C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_blob_open(host: *const Host, kind: u8, header: *const c_char) -> *mut BlobSender {
    match unsafe { &*host }.link() {
        Some(l) => Box::into_raw(Box::new(l.open_blob(kind, unsafe { text(header) }))),
        None => std::ptr::null_mut(),
    }
}

/// The picture starts afresh as `accept` says (another size after a `configure`, another
/// codec): the next frame sent must be a keyframe, and frames count from 0 again. Returns 1 if
/// a client is connected.
///
/// # Safety
/// `host` must come from `rm_host_start`; `accept` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_restart(host: *const Host, accept: *const RmAccept) -> i32 {
    match unsafe { &*host }.link() {
        Some(l) => l.restart(accepted(unsafe { &*accept })) as i32,
        None => 0,
    }
}

/// Start a host listening on `listen` ("0.0.0.0:7791"). `state_dir` holds its identity and the
/// devices it trusts; `name` and `os` ("macos", "linux") are shown on clients; `features` is a
/// comma-separated list of what it can do ("video,input,cursor"). With `pair` non-zero the next
/// unknown device is let in and remembered. Returns null on failure, with the reason in `err`.
///
/// # Safety
/// The strings must be valid C strings and `err` a writable buffer of `err_len` bytes (or null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_start(
    listen: *const c_char,
    state_dir: *const c_char,
    name: *const c_char,
    os: *const c_char,
    features: *const c_char,
    pair: i32,
    callbacks: RmHostCallbacks,
    err: *mut c_char,
    err_len: usize,
) -> *mut Host {
    let (listen, state, name, os, features) = unsafe { (text(listen), text(state_dir), text(name), text(os), text(features)) };
    let listen = match listen.parse() {
        Ok(a) => a,
        Err(_) => {
            report(err, err_len, &format!("{listen} isn't an address and port"));
            return std::ptr::null_mut();
        }
    };
    let features = features.split(',').map(str::trim).filter(|f| !f.is_empty()).map(str::to_owned).collect();
    match Host::start(HostOptions { listen, state: PathBuf::from(state), name, os, features, pair: pair != 0, test_drop_one_in: None }, callbacks) {
        Ok(h) => Box::into_raw(Box::new(h)),
        Err(e) => {
            report(err, err_len, &format!("{e:#}"));
            std::ptr::null_mut()
        }
    }
}

/// "address:port\tfingerprint" of this host, copied into `out`.
///
/// # Safety
/// `host` must come from `rm_host_start`; `out` must be writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_info(host: *const Host, out: *mut c_char, len: usize) {
    let h = unsafe { &*host };
    report(out, len, &format!("{}\t{}", h.address, h.fingerprint));
}

/// Let the next unknown device in (it is remembered, and pairing turns itself off), or stop.
///
/// # Safety
/// `host` must come from `rm_host_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_set_pairing(host: *const Host, on: i32) {
    unsafe { &*host }.set_pairing(on != 0);
}

/// Whether pairing is on.
///
/// # Safety
/// `host` must come from `rm_host_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_pairing(host: *const Host) -> i32 {
    unsafe { &*host }.pairing() as i32
}

/// The connected client's session clock in microseconds, to stamp frames with at capture; 0
/// when no client is connected.
///
/// # Safety
/// `host` must come from `rm_host_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_now_us(host: *const Host) -> u64 {
    unsafe { &*host }.link().map_or(0, |l| l.now_us().max(1))
}

/// Send one encoded frame to the connected client. Waits while the network is behind, so call
/// it from the encoder's thread, never the main one. Returns 1 if it was taken, 0 if no client
/// is connected.
///
/// # Safety
/// `host` must come from `rm_host_start`; `data` must be readable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_send_frame(host: *const Host, data: *const u8, len: usize, key: u8, capture_us: u64) -> i32 {
    match unsafe { &*host }.link() {
        Some(l) => l.send_frame(unsafe { std::slice::from_raw_parts(data, len) }, key != 0, capture_us) as i32,
        None => 0,
    }
}

/// The cursor's image changed: `width * height * 4` bytes of premultiplied RGBA, rows top to
/// bottom, hotspot in its pixels. `width` 0: no cursor. Only for clients that asked
/// (`cursor_apart`).
///
/// # Safety
/// `host` must come from `rm_host_start`; `rgba` must be readable for `width * height * 4` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_cursor_shape(host: *const Host, rgba: *const u8, width: u32, height: u32, hot_x: i32, hot_y: i32) {
    if let Some(l) = unsafe { &*host }.link() {
        let n = width as usize * height as usize * 4;
        let rgba = if n == 0 || rgba.is_null() { Vec::new() } else { unsafe { std::slice::from_raw_parts(rgba, n) }.to_vec() };
        l.cursor_shape(CursorShape { width, height, hot_x, hot_y, rgba });
    }
}

/// Where the cursor is, as fractions of the display; `visible` 0 when it isn't on it.
///
/// # Safety
/// `host` must come from `rm_host_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_cursor_at(host: *const Host, x: f32, y: f32, visible: u8) {
    if let Some(l) = unsafe { &*host }.link() {
        l.cursor_at(x, y, visible != 0);
    }
}

/// Stop listening, drop the client, and free the host.
///
/// # Safety
/// `host` must come from `rm_host_start` and not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rm_host_stop(host: *mut Host) {
    if !host.is_null() {
        unsafe { Box::from_raw(host) }.stop();
    }
}
