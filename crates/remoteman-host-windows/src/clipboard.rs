//! The clipboard shared with a device: text, both ways. What is copied on this desktop goes to
//! the device as a clipboard blob; what the device copied becomes what this desktop pastes.
//!
//! Windows tells a window when the clipboard changes (`AddClipboardFormatListener`), so this
//! is a thread with a window nobody sees. Putting the device's copy on the clipboard is a
//! change too, and Windows reports it like any other: the clipboard's own count of changes
//! says which reports are our own doing, and those are not sent back.
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
use windows::Win32::System::DataExchange::{AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard, RemoveClipboardFormatListener, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG, PostMessageW, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE, WNDCLASSW,
};
use windows::core::w;

/// `CF_UNICODETEXT`.
const UNICODE_TEXT: u32 = 13;
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

/// The text on the clipboard, if there is any and it isn't too much.
fn read(window: HWND) -> Option<String> {
    let _open = Open::by(window)?;
    // SAFETY: the clipboard is open; the memory is the clipboard's and is only read, within
    // its size, while locked.
    unsafe {
        let handle = GetClipboardData(UNICODE_TEXT).ok()?;
        let memory = HGLOBAL(handle.0);
        let units = GlobalSize(memory) / 2;
        let at = GlobalLock(memory) as *const u16;
        if at.is_null() || units == 0 || units as u64 > CLIPBOARD_LIMIT {
            return None;
        }
        let all = std::slice::from_raw_parts(at, units);
        let text = String::from_utf16_lossy(&all[..all.iter().position(|&c| c == 0).unwrap_or(units)]);
        let _ = GlobalUnlock(memory);
        Some(text)
    }
}

/// Make `text` what the desktop pastes. Says the clipboard's count of changes afterwards.
fn write(window: HWND, text: &str) -> Option<u32> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let open = Open::by(window)?;
    // SAFETY: the clipboard is open; the memory is ours until the clipboard takes it, and is
    // written within its size.
    unsafe {
        EmptyClipboard().ok()?;
        let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
        let at = GlobalLock(memory) as *mut u16;
        if at.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), at, wide.len());
        let _ = GlobalUnlock(memory);
        SetClipboardData(UNICODE_TEXT, Some(HANDLE(memory.0))).ok()?;
        // The count moves once more when the clipboard is closed: read it after.
        drop(open);
        Some(GetClipboardSequenceNumber())
    }
}

struct Shared {
    window: AtomicIsize,
    /// The device's copies waiting to be put on the clipboard.
    offers: Mutex<VecDeque<String>>,
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
    pub fn offer(&self, text: String) {
        self.shared.offers.lock().unwrap().push_back(text);
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
    let mut own_text = None::<String>;
    let mut last = None::<(u64, Instant)>;
    let mut message = MSG::default();
    // SAFETY: this thread's own message loop.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        match message.message {
            QUIT => break,
            OFFER => {
                let offers: Vec<String> = shared.offers.lock().unwrap().drain(..).collect();
                // Only the newest matters.
                if let Some(text) = offers.last() {
                    match write(window, text) {
                        Some(count) => {
                            own = Some(count);
                            own_text = Some(text.clone());
                            // The next copy on the desktop is news even if it is the same.
                            last = None;
                            tracing::info!(kind = "text/plain;charset=utf-8", bytes = text.len(), "a copy on the device is on the desktop's clipboard");
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
                let Some(text) = read(window).filter(|t| !t.is_empty()) else { continue };
                // (And by what it says, should the count have moved for a reason of Windows'.)
                if own_text.take().as_ref() == Some(&text) {
                    continue;
                }
                let mark = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    text.hash(&mut h);
                    h.finish()
                };
                let now = Instant::now();
                if last.replace((mark, now)).is_some_and(|(m, at)| m == mark && now - at < Duration::from_secs(2)) {
                    continue;
                }
                tracing::info!(kind = "text/plain;charset=utf-8", bytes = text.len(), "a copy on the desktop goes to the device");
                let header = ClipboardHeader { seq: copies.fetch_add(1, Ordering::Relaxed) + 1, mime: "text/plain;charset=utf-8".into(), size: text.len() as u64 };
                let blob = link.open_blob(BLOB_CLIPBOARD, serde_json::to_string(&header).unwrap_or_default());
                // Writing waits while the network is behind, which this loop mustn't.
                let _ = std::thread::Builder::new().name("copy".into()).spawn(move || blob.write(text.as_bytes()) && blob.finish());
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
