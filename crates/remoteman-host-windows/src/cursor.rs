//! The cursor, for clients that draw it themselves: its image when it changes and its place
//! many times a second, asked of Windows (`GetCursorInfo`), so it works with any display.
//!
//! What keeps the cursor out of the picture is the display driver, not this file: with its
//! hardware cursor on (the Guesthouse driver's default) Windows hands the driver ordinary
//! cursors apart from the frames, so the frames this host captures have no cursor in them.
//! That holds for cursors with see-through pixels only. The driver declares no support for
//! cursors that invert what is under them (the text cursor, as a rule), and Windows is taken
//! to draw those into the frames itself; for them the client is sent an empty image, so that
//! it doesn't draw a second one. Seen on a run in a Guesthouse guest (2026-11-18): the arrow
//! is not in the captured frames, the text cursor is.
//!
//! The other side of it: a client that does not ask for the cursor, and every second display
//! (the protocol has no cursor channel for one), gets it drawn into the picture: the same
//! image and place go into a [`Drawn`] that the picture's pipeline reads, and each change wakes
//! the pipeline, which takes the driver's last frame again and encodes it with the pointer on
//! it (the driver sends no new frame when only the pointer moves).
//!
//! Like input, this only works in the desktop's own session (see `input.rs`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use remoteman_core::CursorShape;
use remoteman_core::host::Link;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC};
use windows::Win32::UI::WindowsAndMessaging::{CURSOR_SHOWING, CURSORINFO, GetCursorInfo, GetIconInfo, HICON, ICONINFO};

use crate::display::Area;

/// The pointer as a picture draws it: its image (RGBA, colour premultiplied), where its top
/// left corner is in the picture's pixels, and whether it is shown. `serial` counts changes;
/// `shape_serial` changes of the image alone.
#[derive(Default)]
pub struct Pointer {
    pub shape: Option<CursorShape>,
    pub shape_serial: u64,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    pub serial: u64,
}

/// A pointer drawn into a picture, shared between the cursor's thread and the picture's.
#[derive(Default)]
pub struct Drawn {
    pub now: Mutex<Pointer>,
    /// Wakes the picture's thread (set by the pipeline when it starts).
    wake: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Drawn {
    pub fn set_wake(&self, wake: Box<dyn Fn() + Send>) {
        *self.wake.lock().unwrap() = Some(wake);
    }

    fn changed(&self, change: impl FnOnce(&mut Pointer) -> bool) {
        let woke = {
            let mut p = self.now.lock().unwrap();
            let did = change(&mut p);
            if did {
                p.serial += 1;
            }
            did
        };
        if woke {
            if let Some(wake) = self.wake.lock().unwrap().as_ref() {
                wake();
            }
        }
    }
}

pub struct Cursor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Cursor {
    /// Report the cursor to `link` for as long as this lives. `area` is where the client's
    /// display is on the Windows desktop.
    /// Report the cursor to `link` (the client draws it), or draw it into a picture through
    /// `drawn`, or both.
    pub fn start(link: Option<Link>, area: Arc<Mutex<Area>>, drawn: Option<Arc<Drawn>>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new().name("cursor".into()).spawn(move || {
            // The image last sent, by Windows' handle for it.
            let mut shown = None::<isize>;
            let mut sent = None::<CursorShape>;
            let mut failed = false;
            while !stopped.load(Ordering::Relaxed) {
                let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
                // SAFETY: `info` is sized as the call wants.
                match unsafe { GetCursorInfo(&mut info) } {
                    Ok(()) => {
                        let showing = info.flags.0 & CURSOR_SHOWING.0 != 0;
                        let handle = info.hCursor.0 as isize;
                        if showing && shown != Some(handle) {
                            shown = Some(handle);
                            let shape = shape(HICON(info.hCursor.0)).unwrap_or_else(|| CursorShape { width: 0, height: 0, hot_x: 0, hot_y: 0, rgba: Vec::new() });
                            // Another handle isn't always another image. Seen with a viewer
                            // running on the desktop it shows: it wears the image it is sent
                            // as a cursor of its own making, which comes back here under a new
                            // handle, and sending that on went round 250 times a second.
                            if sent.as_ref() != Some(&shape) {
                                if let Some(link) = &link {
                                    link.cursor_shape(shape.clone());
                                }
                                if let Some(d) = &drawn {
                                    let s2 = shape.clone();
                                    d.changed(|p| {
                                        p.shape = Some(s2);
                                        p.shape_serial += 1;
                                        true
                                    });
                                }
                                sent = Some(shape);
                            }
                        }
                        let a = *area.lock().unwrap();
                        let (x, y) = (info.ptScreenPos.x - a.left, info.ptScreenPos.y - a.top);
                        let on = x >= 0 && y >= 0 && (x as u32) < a.width && (y as u32) < a.height;
                        if let Some(link) = &link {
                            link.cursor_at(x as f32 / a.width.max(1) as f32, y as f32 / a.height.max(1) as f32, showing && on);
                        }
                        if let Some(d) = &drawn {
                            let (hx, hy) = sent.as_ref().map_or((0, 0), |s| (s.hot_x, s.hot_y));
                            let (cx, cy, visible) = (x - hx, y - hy, showing && on);
                            d.changed(|p| {
                                let moved = (p.x, p.y, p.visible) != (cx, cy, visible);
                                (p.x, p.y, p.visible) = (cx, cy, visible);
                                moved
                            });
                        }
                    }
                    // The desktop isn't the user's just now (a permission prompt, the lock
                    // screen), or this isn't the desktop's session.
                    Err(e) => {
                        if !std::mem::replace(&mut failed, true) {
                            tracing::warn!("Windows wouldn't say where the cursor is ({e}). Said once.");
                        }
                        if let Some(link) = &link {
                            link.cursor_at(0.0, 0.0, false);
                        }
                    }
                }
                // About 250 times a second; the core sends a position only when it changed.
                std::thread::sleep(Duration::from_millis(4));
            }
        })?;
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Cursor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A bitmap's pixels as 32-bit BGRA rows from the top, and its size.
fn pixels(bitmap: HBITMAP) -> Option<(Vec<u8>, u32, u32)> {
    // SAFETY: GDI calls as documented; the buffer is `width * height * 4` bytes as asked for.
    unsafe {
        let mut b = BITMAP::default();
        if GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some(&mut b as *mut BITMAP as *mut _)) == 0 || b.bmWidth <= 0 || b.bmHeight <= 0 {
            return None;
        }
        let (width, height) = (b.bmWidth as u32, b.bmHeight as u32);
        let mut info = BITMAPINFO {
            // A negative height asks for rows from the top.
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: b.bmWidth, biHeight: -b.bmHeight, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut out = vec![0u8; (width * height * 4) as usize];
        let dc = GetDC(None);
        let rows = GetDIBits(dc, bitmap, 0, height, Some(out.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        (rows as u32 == height).then_some((out, width, height))
    }
}

/// Pixels of a cursor that invert what is under them (the text cursor is made of nothing
/// else) can't be said in RGBA, and nobody else draws them: the driver keeps the cursor out of
/// the frames. They become black with a white edge one pixel wide, which shows on anything.
fn stand_in(rgba: &mut [u8], width: u32, height: u32, inverting: &[bool]) {
    let (w, h) = (width as usize, height as usize);
    for i in (0..w * h).filter(|i| inverting[*i]) {
        rgba[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 255]);
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let beside = (x > 0 && inverting[i - 1]) || (x + 1 < w && inverting[i + 1]) || (y > 0 && inverting[i - w]) || (y + 1 < h && inverting[i + w]);
            if beside && !inverting[i] && rgba[i * 4 + 3] == 0 {
                rgba[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
}

/// A cursor's image as the client wants it: RGBA with the colours already multiplied by the
/// alpha. `None`: it couldn't be read.
fn shape(cursor: HICON) -> Option<CursorShape> {
    let mut info = ICONINFO::default();
    // SAFETY: `info` is live; the two bitmaps it returns are ours to delete.
    unsafe { GetIconInfo(cursor, &mut info) }.ok()?;
    let colour = (!info.hbmColor.is_invalid()).then(|| pixels(info.hbmColor)).flatten();
    let mask = (!info.hbmMask.is_invalid()).then(|| pixels(info.hbmMask)).flatten();
    // SAFETY: as said above.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
    }
    let (hot_x, hot_y) = (info.xHotspot as i32, info.yHotspot as i32);
    // A one-bit mask read as 32-bit pixels: a set bit comes out white.
    let set = |m: &[u8], i: usize| m[i * 4] != 0;
    let (kind, made) = match (colour, mask) {
        (Some((mut bgra, width, height)), mask) => {
            let n = (width * height) as usize;
            let alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
            if !alpha {
                // An old-style colour cursor: the mask says where it is see-through. A pixel
                // that is both masked out and coloured inverts what is under it.
                let (m, ..) = mask.filter(|(_, w, h)| (*w, *h) == (width, height))?;
                let inverting: Vec<bool> = (0..n).map(|i| set(&m, i) && bgra[i * 4..i * 4 + 3] != [0, 0, 0]).collect();
                for i in 0..n {
                    bgra[i * 4 + 3] = if set(&m, i) { 0 } else { 255 };
                }
                if inverting.contains(&true) {
                    tracing::debug!(width, height, "cursor: colour with inverting pixels, sent as black edged with white");
                    stand_in(&mut bgra, width, height, &inverting);
                }
            }
            // To RGBA, colours multiplied by alpha (Windows keeps them unmultiplied).
            for p in bgra.chunks_exact_mut(4) {
                let a = p[3] as u32;
                let mul = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                (p[0], p[1], p[2]) = (mul(p[2]), mul(p[1]), mul(p[0]));
            }
            (if alpha { "colour with alpha" } else { "colour with a mask" }, CursorShape { width, height, hot_x, hot_y, rgba: bgra })
        }
        (None, Some((m, width, double))) => {
            // A black-and-white cursor: the upper half of the bitmap is the mask, the lower
            // half the picture. Masked out and white means "invert".
            let height = double / 2;
            let n = (width * height) as usize;
            if m.len() < n * 8 || height == 0 {
                return None;
            }
            let inverting: Vec<bool> = (0..n).map(|i| set(&m, i) && set(&m, n + i)).collect();
            let mut rgba = vec![0u8; n * 4];
            for i in 0..n {
                if !set(&m, i) {
                    let v = if set(&m, n + i) { 255 } else { 0 };
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            if inverting.contains(&true) {
                tracing::debug!(width, height, "cursor: black and white with inverting pixels, sent as black edged with white");
                stand_in(&mut rgba, width, height, &inverting);
            }
            ("black and white", CursorShape { width, height, hot_x, hot_y, rgba })
        }
        (None, None) => return None,
    };
    tracing::debug!(width = made.width, height = made.height, hot_x, hot_y, "cursor: {kind}");
    Some(made)
}
