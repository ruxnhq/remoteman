//! A device's camera as a camera on this PC: the device sends H.264 frames, this decodes them
//! (Windows' own H.264 decoder, in software: a 720p frame is a millisecond or two) and hands the
//! pictures to the virtual camera (`remoteman-vcam-windows`), which Windows' Frame Server shows
//! to every program as "<device> camera (remoteman)".
//!
//! The camera has one size, 1280x720: a picture of another shape (a device turned on its side)
//! is scaled to fit and centred on black, as on the Linux host, for the same reason: programs
//! pick a size when they open a camera and don't follow a change.
//!
//! [`Feed`] is what the session holds: it keeps the camera's thread, the frames that overtook
//! the message that switches the camera on, and the tally for the log.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use remoteman_core::host::Link;
use remoteman_core::{Frame, Message};
use remoteman_vcam_windows::host::{Access, VirtualCamera};
use remoteman_vcam_windows::ring::picture_bytes;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

/// The camera's picture size (what the virtual camera offers).
const WIDTH: u32 = remoteman_vcam_windows::source::WIDTH;
const HEIGHT: u32 = remoteman_vcam_windows::source::HEIGHT;

/// Can this host present a camera? Only where the installer registered the media source.
pub fn available() -> bool {
    remoteman_vcam_windows::host::installed()
}

// ---------------------------------------------------------------------------------------------
// Putting a decoded picture on the camera's canvas.

/// A decoded NV12 picture as the decoder left it.
struct Nv12<'a> {
    data: &'a [u8],
    stride: usize,
    /// Rows the decoder allocated for the luma plane: where the chroma plane starts.
    alloc_height: usize,
    /// The part to show (even offsets).
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

/// The largest even size with the proportions of `source` that fits inside `canvas`.
fn fit_size(source: (usize, usize), canvas: (usize, usize)) -> (usize, usize) {
    let (sw, sh, cw, ch) = (source.0 as u64, source.1 as u64, canvas.0 as u64, canvas.1 as u64);
    let (w, h) = if sw * ch >= sh * cw { (cw, (sh * cw + sw / 2) / sw) } else { ((sw * ch + sh / 2) / sh, ch) };
    let even = |v: u64, most: u64| ((v + 1) & !1).clamp(2, most) as usize;
    (even(w, cw), even(h, ch))
}

fn black(canvas: &mut [u8]) {
    let (y, uv) = canvas.split_at_mut((WIDTH * HEIGHT) as usize);
    y.fill(16);
    uv.fill(128);
}

/// Draw `src` into the camera's canvas (NV12, WIDTHxHEIGHT): as it is if it fits exactly, else
/// scaled (nearest sample) to fit, centred, on black.
fn compose(canvas: &mut [u8], src: &Nv12) {
    let (cw, ch) = (WIDTH as usize, HEIGHT as usize);
    debug_assert_eq!(canvas.len(), picture_bytes(WIDTH, HEIGHT));
    let uv_at = src.stride * src.alloc_height;
    let (canvas_y, canvas_uv) = canvas.split_at_mut(cw * ch);
    if src.width == cw && src.height == ch {
        for row in 0..ch {
            let from = (src.y + row) * src.stride + src.x;
            canvas_y[row * cw..(row + 1) * cw].copy_from_slice(&src.data[from..from + cw]);
        }
        for row in 0..ch / 2 {
            let from = uv_at + (src.y / 2 + row) * src.stride + src.x;
            canvas_uv[row * cw..(row + 1) * cw].copy_from_slice(&src.data[from..from + cw]);
        }
        return;
    }
    canvas_y.fill(16);
    canvas_uv.fill(128);
    let (dw, dh) = fit_size((src.width, src.height), (cw, ch));
    // Even offsets, so the chroma samples line up.
    let (ox, oy) = (((cw - dw) / 2) & !1, ((ch - dh) / 2) & !1);
    for dy in 0..dh {
        let sy = src.y + dy * src.height / dh;
        let row = &src.data[sy * src.stride + src.x..sy * src.stride + src.x + src.width];
        let out = &mut canvas_y[(oy + dy) * cw + ox..(oy + dy) * cw + ox + dw];
        for (dx, o) in out.iter_mut().enumerate() {
            *o = row[dx * src.width / dw];
        }
    }
    let (sw2, sh2, dw2, dh2) = (src.width / 2, src.height / 2, dw / 2, dh / 2);
    for dy in 0..dh2 {
        let sy = src.y / 2 + dy * sh2 / dh2;
        let row = &src.data[uv_at + sy * src.stride + src.x..uv_at + sy * src.stride + src.x + sw2 * 2];
        let out = &mut canvas_uv[(oy / 2 + dy) * cw + ox..(oy / 2 + dy) * cw + ox + dw2 * 2];
        for dx in 0..dw2 {
            let sx = dx * sw2 / dw2;
            out[dx * 2] = row[sx * 2];
            out[dx * 2 + 1] = row[sx * 2 + 1];
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The decoder.

/// What became of one frame.
enum Decoded {
    Picture,
    /// Nothing to show yet (the decoder wants more), nothing wrong.
    Pending,
    /// It could not be decoded; nothing will be until a keyframe.
    Lost,
    /// Withheld: still waiting for that keyframe.
    Waiting,
}

#[derive(PartialEq)]
enum Need {
    First,
    Keyframe,
    Nothing,
}

struct Decoder {
    mft: IMFTransform,
    need: Need,
    /// What the output is, as the decoder last said.
    out: OutputFormat,
}

#[derive(Clone, Copy)]
struct OutputFormat {
    height: usize,
    stride: usize,
    crop: (usize, usize, usize, usize),
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

impl Decoder {
    fn new() -> Result<Decoder> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, 0)?;
            let mft: IMFTransform = CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).context("Windows' H.264 decoder")?;
            if let Ok(attributes) = mft.GetAttributes() {
                // One frame in, its picture out.
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            input.SetUINT64(&MF_MT_FRAME_SIZE, (WIDTH as u64) << 32 | HEIGHT as u64)?;
            input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &input, 0).context("the decoder's input type")?;
            let mut decoder = Decoder { mft, need: Need::First, out: OutputFormat { height: HEIGHT as usize, stride: WIDTH as usize, crop: (0, 0, WIDTH as usize, HEIGHT as usize) } };
            decoder.choose_output()?;
            decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            decoder.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(decoder)
        }
    }

    /// Take NV12 as the output and read what it says of the picture's shape.
    fn choose_output(&mut self) -> Result<()> {
        unsafe {
            let mut index = 0;
            let ty = loop {
                let ty = self.mft.GetOutputAvailableType(0, index).context("no NV12 output on the decoder")?;
                if ty.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    break ty;
                }
                index += 1;
            };
            self.mft.SetOutputType(0, &ty, 0).context("the decoder's output type")?;
            self.read_output_type()
        }
    }

    fn read_output_type(&mut self) -> Result<()> {
        unsafe {
            let ty = self.mft.GetOutputCurrentType(0)?;
            let size = ty.GetUINT64(&MF_MT_FRAME_SIZE)?;
            let (width, height) = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
            let stride = ty.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|s| s as i32).ok().filter(|s| *s > 0).map_or(width, |s| s as usize);
            let mut crop = (0, 0, width, height);
            let mut area = MFVideoArea::default();
            let bytes = std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, std::mem::size_of::<MFVideoArea>());
            if ty.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                // Even offsets, so that the chroma lines up.
                crop = ((area.OffsetX.value.max(0) as usize) & !1, (area.OffsetY.value.max(0) as usize) & !1, area.Area.cx as usize, area.Area.cy as usize);
            }
            self.out = OutputFormat { height, stride, crop };
            Ok(())
        }
    }

    /// One whole frame (Annex-B), `key` as the sender marked it. A picture, if there is one, is
    /// drawn on `canvas`.
    fn decode(&mut self, data: &[u8], key: bool, capture_us: u64, canvas: &mut [u8]) -> Decoded {
        if self.need != Need::Nothing && !key {
            let first = self.need == Need::First;
            self.need = Need::Keyframe;
            return if first { Decoded::Lost } else { Decoded::Waiting };
        }
        if data.is_empty() {
            self.need = Need::Keyframe;
            return Decoded::Lost;
        }
        if self.need == Need::Keyframe {
            // Forget the damaged sequence.
            unsafe {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            }
        }
        match self.feed(data, capture_us, canvas) {
            Ok(true) => {
                self.need = Need::Nothing;
                Decoded::Picture
            }
            Ok(false) => {
                // A keyframe the decoder took but gave nothing for yet is still a start.
                if key {
                    self.need = Need::Nothing;
                }
                Decoded::Pending
            }
            Err(_) => {
                self.need = Need::Keyframe;
                Decoded::Lost
            }
        }
    }

    fn feed(&mut self, data: &[u8], capture_us: u64, canvas: &mut [u8]) -> Result<bool> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut at = std::ptr::null_mut();
            buffer.Lock(&mut at, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), at, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime((capture_us as i64).saturating_mul(10))?;
            self.mft.ProcessInput(0, &sample, 0)?;
            // The decoder may have more than one picture for what it was given (it has been fed
            // whole frames, but be ready); the last one is what is shown.
            let mut shown = false;
            for _ in 0..4 {
                match self.output(canvas) {
                    Ok(true) => shown = true,
                    Ok(false) => break,
                    Err(e) => return Err(e),
                }
            }
            Ok(shown)
        }
    }

    /// Ask for one picture. `Ok(false)`: the decoder wants more input.
    fn output(&mut self, canvas: &mut [u8]) -> Result<bool> {
        unsafe {
            let info = self.mft.GetOutputStreamInfo(0)?;
            let provides = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let mut ours = None;
            if !provides {
                let size = info.cbSize.max((self.out.stride * self.out.height * 3 / 2) as u32).max(64);
                let buffer = MFCreateMemoryBuffer(size)?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                ours = Some(sample);
            }
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(ours), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) }];
            let mut status = 0u32;
            let result = self.mft.ProcessOutput(0, &mut buffers, &mut status);
            let sample = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
            let _events = std::mem::ManuallyDrop::take(&mut buffers[0].pEvents);
            match result {
                Ok(()) => {
                    let Some(sample) = sample else { return Ok(false) };
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    let (mut at, mut len) = (std::ptr::null_mut(), 0u32);
                    buffer.Lock(&mut at, None, Some(&mut len))?;
                    let data = std::slice::from_raw_parts(at, len as usize);
                    let f = self.out;
                    let alloc_height = if f.stride == 0 { 0 } else { data.len() * 2 / (3 * f.stride) };
                    let (x, y, w, h) = f.crop;
                    let fits = w >= 16 && h >= 16 && x + w <= f.stride && y + h <= alloc_height && w % 2 == 0 && h % 2 == 0;
                    if fits {
                        compose(canvas, &Nv12 { data, stride: f.stride, alloc_height, x, y, width: w, height: h });
                    }
                    let _ = buffer.Unlock();
                    if fits { Ok(true) } else { bail!("a picture that doesn't add up ({w}x{h} in {} bytes)", data.len()) }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // The decoder has read the stream's parameters: NV12 at its size.
                    self.choose_output()?;
                    self.output(canvas)
                }
                Err(e) => Err(e.into()),
            }
        }
    }

}

// ---------------------------------------------------------------------------------------------
// The thread.

/// How a device's camera went, for the log when it is switched off.
#[derive(Default)]
struct Tally {
    frames: u32,
    keys: u32,
    shown: u32,
    lost: u32,
    waiting: u32,
    asked: u32,
    asked_at: Option<Instant>,
    last: Option<Instant>,
    gap: Duration,
    decode: Duration,
}

/// The device's camera, as far as the session is concerned.
#[derive(Default)]
pub struct Feed {
    tx: Option<mpsc::SyncSender<Frame>>,
    /// Frames that came before the message saying the camera is on (they travel apart, so the
    /// first can overtake it), with when each came.
    early: Vec<(Instant, Frame)>,
}

impl Feed {
    /// The device switched its camera on or off. `name` is the device's name.
    pub fn take(&mut self, on: bool, codec: &str, name: &str, link: Link) {
        // The frames that overtook this message are kept for the camera it switches on.
        self.tx = None;
        if !on {
            self.early.clear();
        }
        if !on {
            return;
        }
        if !codec.is_empty() && codec != "h264" {
            tracing::warn!(codec, "the device's camera is sent in a codec this host doesn't take; only h264 is");
            return;
        }
        if !available() {
            tracing::warn!("the device's camera can't be shown: the camera driver of this host isn't installed");
            return;
        }
        let friendly = format!("{name} camera (remoteman)");
        // On a thread of its own: decoding takes a few milliseconds a frame and must not hold up
        // the device's input, which comes through the session too.
        let (tx, rx) = mpsc::sync_channel::<Frame>(30);
        let started = std::thread::Builder::new().name("camera".into()).spawn(move || {
            if let Err(e) = run(&friendly, rx, link) {
                tracing::warn!("the device's camera: {e:#}");
            }
        });
        match started {
            Ok(_) => {
                for (came, frame) in self.early.drain(..) {
                    if came.elapsed() < Duration::from_millis(500) {
                        let _ = tx.try_send(frame);
                    }
                }
                self.tx = Some(tx);
            }
            Err(e) => tracing::warn!("couldn't start the camera's thread: {e}"),
        }
    }

    /// One frame of the device's camera.
    pub fn frame(&mut self, frame: &Frame) {
        match &self.tx {
            // If the thread is a second behind, this frame is left out; what follows can't be
            // decoded then, and the thread asks the device for a keyframe.
            Some(tx) => {
                let _ = tx.try_send(frame.clone());
            }
            None => {
                self.early.retain(|(came, _)| came.elapsed() < Duration::from_millis(500));
                if self.early.len() < 30 {
                    self.early.push((Instant::now(), frame.clone()));
                }
            }
        }
    }

    /// The device is gone: the camera leaves (its thread ends when the channel closes).
    pub fn clear(&mut self) {
        self.tx = None;
        self.early.clear();
    }
}

fn run(name: &str, rx: mpsc::Receiver<Frame>, link: Link) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut decoder = Decoder::new()?;
    // Seen by programs and by users of every account where this host is the PC's service (SYSTEM);
    // a host somebody started themselves shows it to them.
    let access = if crate::desk::is_system() { Access::AllUsers } else { Access::CurrentUser };
    let mut camera = VirtualCamera::start(name, access).context("making the camera")?;
    tracing::info!(name, ring = camera.backing, ?access, "the device's camera is on this PC");
    let mut canvas = vec![0u8; picture_bytes(WIDTH, HEIGHT)];
    black(&mut canvas);
    let mut t = Tally::default();
    loop {
        let frame = match rx.recv_timeout(Duration::from_millis(remoteman_vcam_windows::ring::BEAT_MS / 2)) {
            Ok(frame) => frame,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                camera.beat();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let began = Instant::now();
        if let Some(last) = t.last.replace(began) {
            t.gap = t.gap.max(began - last);
        }
        t.frames += 1;
        t.keys += frame.key as u32;
        match decoder.decode(&frame.data, frame.key, frame.capture_us, &mut canvas) {
            Decoded::Picture => {
                t.shown += 1;
                camera.push(&canvas, frame.capture_us);
            }
            Decoded::Pending => camera.beat(),
            Decoded::Waiting => t.waiting += 1,
            Decoded::Lost => {
                t.lost += 1;
                if t.asked_at.is_none_or(|at| at.elapsed() > Duration::from_millis(250)) {
                    t.asked_at = Some(Instant::now());
                    t.asked += 1;
                    link.send_message(Message::CameraKeyframe);
                }
            }
        }
        t.decode = t.decode.max(began.elapsed());
    }
    tracing::info!(
        frames = t.frames,
        keyframes = t.keys,
        shown = t.shown,
        undecodable = t.lost,
        skipped = t.waiting,
        asked_for_keyframe = t.asked,
        longest_gap_ms = t.gap.as_millis() as u64,
        longest_decode_us = t.decode.as_micros() as u64,
        "the device's camera is off this PC"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_keeping_proportions() {
        assert_eq!(fit_size((1280, 720), (1280, 720)), (1280, 720));
        assert_eq!(fit_size((1920, 1080), (1280, 720)), (1280, 720));
        assert_eq!(fit_size((720, 1280), (1280, 720)), (406, 720));
        assert_eq!(fit_size((640, 480), (1280, 720)), (960, 720));
        assert_eq!(fit_size((4000, 2), (1280, 720)), (1280, 2));
    }

    fn picture(w: usize, h: usize, stride: usize, alloc: usize, y: u8, c: (u8, u8)) -> Vec<u8> {
        let mut d = vec![0u8; stride * alloc * 3 / 2];
        for row in 0..h {
            d[row * stride..row * stride + w].fill(y);
        }
        for row in 0..h / 2 {
            for x in 0..w / 2 {
                d[stride * alloc + row * stride + x * 2] = c.0;
                d[stride * alloc + row * stride + x * 2 + 1] = c.1;
            }
        }
        d
    }

    #[test]
    fn a_picture_of_the_cameras_size_is_copied() {
        let d = picture(1280, 720, 1280, 720, 90, (60, 200));
        let mut canvas = vec![0u8; picture_bytes(WIDTH, HEIGHT)];
        compose(&mut canvas, &Nv12 { data: &d, stride: 1280, alloc_height: 720, x: 0, y: 0, width: 1280, height: 720 });
        assert_eq!(canvas, d);
    }

    #[test]
    fn a_taller_picture_is_centred_on_black() {
        // 720x1280 (a device on its side) in 736-wide rows and 1280 allocated rows.
        let d = picture(720, 1280, 736, 1280, 150, (70, 180));
        let mut canvas = vec![0u8; picture_bytes(WIDTH, HEIGHT)];
        compose(&mut canvas, &Nv12 { data: &d, stride: 736, alloc_height: 1280, x: 0, y: 0, width: 720, height: 1280 });
        let (y, uv) = canvas.split_at((WIDTH * HEIGHT) as usize);
        let row = |r: usize| &y[r * WIDTH as usize..(r + 1) * WIDTH as usize];
        // Bars left and right (406 wide, offset 437 -> even 436), picture in the middle.
        assert_eq!(row(360)[0], 16);
        assert_eq!(row(360)[640], 150);
        assert_eq!(row(360)[1279], 16);
        assert_eq!(uv[0], 128);
        assert_eq!(&uv[180 * 1280 + 640..180 * 1280 + 642], &[70, 180]);
    }

    /// Split an Annex-B stream into the frames the core would deliver: (data, key).
    fn frames(stream: &[u8]) -> Vec<(Vec<u8>, bool)> {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 < stream.len() {
            if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
                starts.push(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        let mut out: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut last_slice = false;
        let mut begin = 0;
        for (n, &s) in starts.iter().enumerate() {
            let kind = stream[s + 3] & 0x1f;
            let slice = kind == 1 || kind == 5;
            // A new frame starts at a parameter set, or at a slice after a slice.
            if n > 0 && (matches!(kind, 7 | 8 | 9) && last_slice || slice && last_slice) {
                let from = if begin == 0 { 0 } else { begin };
                out.push((stream[from..s].to_vec(), false));
                begin = s;
            }
            last_slice = slice;
            let _ = slice;
        }
        out.push((stream[begin..].to_vec(), false));
        for (data, key) in out.iter_mut() {
            let mut j = 0;
            while j + 3 < data.len() {
                if data[j] == 0 && data[j + 1] == 0 && data[j + 2] == 1 && data[j + 3] & 0x1f == 5 {
                    *key = true;
                }
                j += 1;
            }
        }
        out
    }

    /// Clips made with ffmpeg (testsrc2, openh264) in the folder named by `VCAM_TEST_CLIPS`:
    /// each is decoded and its pictures land on the camera's canvas, whatever their size.
    #[test]
    fn decodes_clips() {
        let Some(dir) = std::env::var_os("VCAM_TEST_CLIPS") else { return };
        for name in ["1280x720", "720x1280", "1920x1080", "640x480"] {
            let stream = std::fs::read(std::path::Path::new(&dir).join(format!("{name}.h264"))).unwrap();
            let frames = frames(&stream);
            assert!(frames.len() >= 40, "{name}: {} frames", frames.len());
            let mut decoder = Decoder::new().unwrap();
            let mut canvas = vec![0u8; picture_bytes(WIDTH, HEIGHT)];
            black(&mut canvas);
            let (mut shown, mut lost) = (0, 0);
            let mut distinct = std::collections::HashSet::new();
            for (n, (data, key)) in frames.iter().enumerate() {
                match decoder.decode(data, *key, n as u64 * 33_333, &mut canvas) {
                    Decoded::Picture => {
                        shown += 1;
                        distinct.insert(canvas.iter().step_by(997).copied().collect::<Vec<u8>>());
                    }
                    Decoded::Lost => lost += 1,
                    _ => {}
                }
            }
            eprintln!("{name}: {} frames, {shown} shown, {lost} lost, {} distinct pictures, out {}x{} stride {} crop {:?}", frames.len(), distinct.len(), decoder.out.stride, decoder.out.height, decoder.out.stride, decoder.out.crop);
            assert!(shown >= frames.len() - 2, "{name}: shown {shown} of {}", frames.len());
            assert_eq!(lost, 0);
            assert!(distinct.len() > 10, "{name}: the picture moves");
            // Not black, not junk: the test pattern has bright and dark areas.
            let (min, max) = canvas[..(WIDTH * HEIGHT) as usize].iter().fold((255u8, 0u8), |(lo, hi), &y| (lo.min(y), hi.max(y)));
            assert!(max > 180 && min < 60, "{name}: luma {min}..{max}");
            // Frames that can't be decoded are reported, then withheld until a keyframe.
            let mut fresh = Decoder::new().unwrap();
            assert!(matches!(fresh.decode(&frames[1].0, false, 0, &mut canvas), Decoded::Lost));
            assert!(matches!(fresh.decode(&frames[2].0, false, 0, &mut canvas), Decoded::Waiting));
            assert!(matches!(fresh.decode(&[0xff; 500], true, 0, &mut canvas), Decoded::Lost | Decoded::Pending));
            assert!(matches!(fresh.decode(&frames[0].0, true, 0, &mut canvas), Decoded::Picture | Decoded::Pending));
        }
    }

    #[test]
    fn a_cropped_1080p_picture_ignores_the_padding() {
        // 1920x1080 shown out of 1088 allocated rows: the chroma plane starts after row 1088.
        let mut d = picture(1920, 1080, 1920, 1088, 120, (50, 210));
        // The padding rows hold junk that must not show.
        d[1080 * 1920..1088 * 1920].fill(255);
        let mut canvas = vec![0u8; picture_bytes(WIDTH, HEIGHT)];
        compose(&mut canvas, &Nv12 { data: &d, stride: 1920, alloc_height: 1088, x: 0, y: 0, width: 1920, height: 1080 });
        assert!(canvas[..(WIDTH * HEIGHT) as usize].iter().all(|&y| y == 120));
        assert_eq!(&canvas[(WIDTH * HEIGHT) as usize..][..2], &[50, 210]);
    }
}
