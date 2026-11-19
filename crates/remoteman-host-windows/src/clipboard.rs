//! The clipboard shared with a device: text and pictures, both ways. What is copied on this desktop goes to
//! the device as a clipboard blob; what the device copied becomes what this desktop pastes.
//!
//! Windows tells a window when the clipboard changes (`AddClipboardFormatListener`), so this
//! is a thread with a window nobody sees. Putting the device's copy on the clipboard is a
//! change too, and Windows reports it like any other: the clipboard's own count of changes
//! says which reports are our own doing, and those are not sent back.
//!
//! A picture goes as a PNG, as the protocol says: the one a program put on the clipboard under
//! the name "PNG" if there is one, else made from the plain bitmap (`CF_DIB`) every program
//! gives. A PNG from the device is put on the clipboard both ways, so that every program can
//! paste it. Text wins when a copy is both, as on Linux.
//!
//! What was copied is never logged, only how much.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use remoteman_core::host::Link;
use remoteman_core::media::{BLOB_CLIPBOARD, CLIPBOARD_LIMIT, ClipboardHeader};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, PostMessageW, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WNDCLASSW,
};
use windows::core::w;

/// `CF_UNICODETEXT`.
const UNICODE_TEXT: u32 = 13;
/// `CF_DIB`: a bitmap with its header in front.
const DIB: u32 = 8;
pub const TEXT: &str = "text/plain;charset=utf-8";
pub const PNG: &str = "image/png";

/// One copy, as the protocol carries it: its type and its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Copied {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// The clipboard format programs put a PNG under.
fn png_format() -> u32 {
    // SAFETY: plain call; the same number for the same name for as long as Windows runs.
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}
/// The clipboard changed (passed on from the window's procedure to its thread's loop).
const CHANGED: u32 = WM_APP + 1;
/// There is a copy of the device's to put on the clipboard.
const OFFER: u32 = WM_APP + 2;
const QUIT: u32 = WM_APP + 3;

thread_local! {
    static WINDOW: Cell<isize> = const { Cell::new(0) };
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if message == WM_CLIPBOARDUPDATE {
        // Whether Windows sent or posted it, the thread's loop gets to hear.
        // SAFETY: our own window.
        let _ = unsafe { PostMessageW(Some(window), CHANGED, WPARAM(0), LPARAM(0)) };
        return LRESULT(0);
    }
    // SAFETY: passing on what was given.
    unsafe { DefWindowProcW(window, message, w, l) }
}

/// The clipboard, opened by this thread's window; closed when dropped. Another program may
/// hold it for a moment, so a few tries.
struct Open;
impl Open {
    fn by(window: HWND) -> Option<Self> {
        for _ in 0..10 {
            // SAFETY: plain call.
            if unsafe { OpenClipboard(Some(window)) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        None
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: opened by this thread.
        let _ = unsafe { CloseClipboard() };
    }
}

/// One format's content on the open clipboard, as bytes, if it isn't too much.
///
/// # Safety
/// The clipboard must be open.
unsafe fn bytes_of(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the memory is the clipboard's and is only read, within its size, while locked.
    unsafe {
        let memory = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(memory);
        if size == 0 || size as u64 > CLIPBOARD_LIMIT {
            return None;
        }
        let at = GlobalLock(memory) as *const u8;
        if at.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(at, size).to_vec();
        let _ = GlobalUnlock(memory);
        Some(bytes)
    }
}

/// What is on the clipboard: text if there is any, else a picture as a PNG.
fn read(window: HWND) -> Option<Copied> {
    let _open = Open::by(window)?;
    // SAFETY: the clipboard is open.
    unsafe {
        if IsClipboardFormatAvailable(UNICODE_TEXT).is_ok() {
            let bytes = bytes_of(UNICODE_TEXT)?;
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let text = String::from_utf16_lossy(&units[..units.iter().position(|&c| c == 0).unwrap_or(units.len())]);
            return Some(Copied { mime: TEXT, bytes: text.into_bytes() });
        }
        if let Some(png) = bytes_of(png_format()).filter(|b| b.starts_with(&[0x89, b'P', b'N', b'G'])) {
            return Some(Copied { mime: PNG, bytes: png });
        }
        let dib = bytes_of(DIB)?;
        Some(Copied { mime: PNG, bytes: dib_to_png(&dib)? })
    }
}

/// A plain bitmap (its header, then rows from the bottom up, blue first) as a PNG. Plain
/// colours of 24 or 32 bits only, as programs give when they copy a picture.
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < size_of::<BITMAPINFOHEADER>() {
        return None;
    }
    // SAFETY: there are at least that many bytes; read unaligned.
    let head = unsafe { std::ptr::read_unaligned(dib.as_ptr() as *const BITMAPINFOHEADER) };
    let (w, h, bits) = (head.biWidth.max(0) as usize, head.biHeight.unsigned_abs() as usize, head.biBitCount as usize);
    if w == 0 || h == 0 || w > 16384 || h > 16384 || !(bits == 24 || bits == 32) || !(head.biCompression == 0 || head.biCompression == 3) {
        return None;
    }
    // (The three masks some programs put after a short header.)
    let start = head.biSize as usize + if head.biCompression == 3 && head.biSize == 40 { 12 } else { 0 };
    let stride = (w * bits / 8).div_ceil(4) * 4;
    if dib.len() < start + stride * h {
        return None;
    }
    // A 32-bit bitmap whose fourth bytes are all 0 has no transparency, whatever it says.
    let alpha = bits == 32 && (0..h).any(|y| (0..w).any(|x| dib[start + y * stride + x * 4 + 3] != 0));
    let step = if alpha { 4 } else { 3 };
    let mut pixels = Vec::with_capacity(w * h * step);
    for y in 0..h {
        let row = if head.biHeight > 0 { h - 1 - y } else { y };
        let line = &dib[start + row * stride..];
        for x in 0..w {
            let p = &line[x * bits / 8..];
            pixels.extend_from_slice(&[p[2], p[1], p[0]]);
            if alpha {
                pixels.push(p[3]);
            }
        }
    }
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, w as u32, h as u32);
    encoder.set_color(if alpha { png::ColorType::Rgba } else { png::ColorType::Rgb });
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().ok()?.write_image_data(&pixels).ok()?;
    Some(png)
}

/// A PNG as a plain 32-bit bitmap, top row last, for programs that don't read PNG.
pub fn png_to_dib(data: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // Palettes, grey and 16 bits come out as 8-bit colour.
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let step = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        _ => return None,
    };
    let header = size_of::<BITMAPINFOHEADER>();
    let head = BITMAPINFOHEADER { biSize: header as u32, biWidth: w as i32, biHeight: h as i32, biPlanes: 1, biBitCount: 32, biSizeImage: (w * h * 4) as u32, ..Default::default() };
    let mut dib = vec![0u8; header + w * h * 4];
    // SAFETY: the header is written into room made for it.
    unsafe { std::ptr::write_unaligned(dib.as_mut_ptr() as *mut BITMAPINFOHEADER, head) };
    for y in 0..h {
        let from = &pixels[(h - 1 - y) * info.line_size..];
        let to = &mut dib[header + y * w * 4..];
        for x in 0..w {
            let p = &from[x * step..];
            let (r, g, b, a) = match step {
                4 => (p[0], p[1], p[2], p[3]),
                3 => (p[0], p[1], p[2], 255),
                2 => (p[0], p[0], p[0], p[1]),
                _ => (p[0], p[0], p[0], 255),
            };
            to[x * 4..x * 4 + 4].copy_from_slice(&[b, g, r, a]);
        }
    }
    Some(dib)
}

/// Make `copy` what the desktop pastes. Says the clipboard's count of changes afterwards.
fn write(window: HWND, copy: &Copied) -> Option<u32> {
    let blocks: Vec<(u32, Vec<u8>)> = if copy.mime == PNG {
        let dib = png_to_dib(&copy.bytes)?;
        vec![(png_format(), copy.bytes.clone()), (DIB, dib)]
    } else {
        let wide: Vec<u8> = String::from_utf8_lossy(&copy.bytes).encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
        vec![(UNICODE_TEXT, wide)]
    };
    let open = Open::by(window)?;
    // SAFETY: the clipboard is open; each block is ours until the clipboard takes it, and is
    // written within its size.
    unsafe {
        EmptyClipboard().ok()?;
        for (format, bytes) in blocks {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
            let at = GlobalLock(memory) as *mut u8;
            if at.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
            let _ = GlobalUnlock(memory);
            SetClipboardData(format, Some(HANDLE(memory.0))).ok()?;
        }
        // The count moves once more when the clipboard is closed: read it after.
        drop(open);
        Some(GetClipboardSequenceNumber())
    }
}

struct Shared {
    window: AtomicIsize,
    /// The device's copies waiting to be put on the clipboard.
    offers: Mutex<VecDeque<Copied>>,
}

/// The clipboard being shared with one device. Dropping it stops the sharing.
pub struct Clipboard {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Clipboard {
    /// Start sharing. `allowed` is the owner's switch: once it is off nothing more leaves,
    /// from that very copy on. `copies` counts what was sent, for the device to tell new from
    /// stale.
    pub fn start(link: Link, allowed: Arc<AtomicBool>, copies: Arc<AtomicU32>) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared { window: AtomicIsize::new(0), offers: Mutex::new(VecDeque::new()) });
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let s = shared.clone();
        let thread = std::thread::Builder::new().name("clipboard".into()).spawn(move || run(s, link, allowed, copies, ready_tx))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { shared, thread: Some(thread) }),
            Ok(Err(e)) => anyhow::bail!("{e}"),
            Err(_) => anyhow::bail!("the clipboard's thread died"),
        }
    }

    /// What the device copied: put it on the desktop's clipboard.
    pub fn offer(&self, copy: Copied) {
        self.shared.offers.lock().unwrap().push_back(copy);
        self.post(OFFER);
    }

    fn post(&self, message: u32) {
        let window = HWND(self.shared.window.load(Ordering::Relaxed) as *mut _);
        // SAFETY: posting to a window of ours from another thread is allowed.
        let _ = unsafe { PostMessageW(Some(window), message, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.post(QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(shared: Arc<Shared>, link: Link, allowed: Arc<AtomicBool>, copies: Arc<AtomicU32>, ready: mpsc::Sender<Result<(), String>>) {
    // SAFETY: a window class and a message-only window of this thread, used on it alone
    // (other threads only post to it), destroyed before the thread ends.
    let window = unsafe {
        let instance = GetModuleHandleW(None).map(|m| m.into()).ok();
        let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance.unwrap_or_default(), lpszClassName: w!("remoteman-clipboard"), ..Default::default() };
        // (Registered before by an earlier sharing: that is fine.)
        RegisterClassW(&class);
        CreateWindowExW(WINDOW_EX_STYLE(0), w!("remoteman-clipboard"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, instance, None)
    };
    let window = match window {
        Ok(w) => w,
        Err(e) => {
            let _ = ready.send(Err(format!("no window to hear of the clipboard with: {e}")));
            return;
        }
    };
    // SAFETY: our window.
    if let Err(e) = unsafe { AddClipboardFormatListener(window) } {
        let _ = ready.send(Err(format!("Windows won't say when the clipboard changes: {e}")));
        // SAFETY: ours.
        let _ = unsafe { DestroyWindow(window) };
        return;
    }
    WINDOW.set(window.0 as isize);
    shared.window.store(window.0 as isize, Ordering::Relaxed);
    let _ = ready.send(Ok(()));

    // The clipboard's count of changes after our own last write: a report of that change is
    // not news. And the last copy sent, to tell a repeat (programs often set the clipboard
    // twice for one copy).
    let mut own = None::<u32>;
    let mut own_text = None::<Copied>;
    let mut last = None::<(u64, Instant)>;
    let mut message = MSG::default();
    // SAFETY: this thread's own message loop.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        match message.message {
            QUIT => break,
            OFFER => {
                let offers: Vec<Copied> = shared.offers.lock().unwrap().drain(..).collect();
                // Only the newest matters.
                if let Some(copy) = offers.last() {
                    match write(window, copy) {
                        Some(count) => {
                            own = Some(count);
                            own_text = Some(copy.clone());
                            // The next copy on the desktop is news even if it is the same.
                            last = None;
                            tracing::info!(kind = copy.mime, bytes = copy.bytes.len(), "a copy on the device is on the desktop's clipboard");
                        }
                        None => tracing::warn!("couldn't put the device's copy on the clipboard ({})", windows::core::Error::from_thread()),
                    }
                }
            }
            CHANGED => {
                // SAFETY: plain call.
                let count = unsafe { GetClipboardSequenceNumber() };
                if own == Some(count) || !allowed.load(Ordering::Relaxed) {
                    continue;
                }
                let Some(copy) = read(window).filter(|c| !c.bytes.is_empty()) else { continue };
                // (And by what it says, should the count have moved for a reason of Windows'.
                // A picture of ours made into a bitmap by Windows comes back as another PNG,
                // and so is told by the count alone.)
                if own_text.take().as_ref() == Some(&copy) {
                    continue;
                }
                let mark = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    (copy.mime, &copy.bytes).hash(&mut h);
                    h.finish()
                };
                let now = Instant::now();
                if last.replace((mark, now)).is_some_and(|(m, at)| m == mark && now - at < Duration::from_secs(2)) {
                    continue;
                }
                if copy.bytes.len() as u64 > CLIPBOARD_LIMIT {
                    tracing::info!(kind = copy.mime, bytes = copy.bytes.len(), "a copy on the desktop is too large for the device");
                    continue;
                }
                tracing::info!(kind = copy.mime, bytes = copy.bytes.len(), "a copy on the desktop goes to the device");
                let header = ClipboardHeader { seq: copies.fetch_add(1, Ordering::Relaxed) + 1, mime: copy.mime.into(), size: copy.bytes.len() as u64 };
                let blob = link.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
                // Writing waits while the network is behind, which this loop mustn't.
                let _ = std::thread::Builder::new().name("copy".into()).spawn(move || blob.write(&copy.bytes) && blob.finish());
            }
            // SAFETY: a message of this thread's.
            _ => drop(unsafe { DispatchMessageW(&message) }),
        }
    }
    // SAFETY: ours, on its own thread.
    unsafe {
        let _ = RemoveClipboardFormatListener(window);
        let _ = DestroyWindow(window);
    }
}
