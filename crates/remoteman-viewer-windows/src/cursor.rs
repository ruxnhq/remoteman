//! The host's cursor shape as a Windows cursor.

use remoteman_core::CursorShape;
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HCURSOR, ICONINFO};

/// `None` for a shape with no size (no cursor), or if Windows won't make one.
pub fn make(shape: &CursorShape) -> Option<HCURSOR> {
    let (w, h) = (shape.width as usize, shape.height as usize);
    if w == 0 || h == 0 || shape.rgba.len() != w * h * 4 {
        return None;
    }
    // SAFETY: the DIB section's memory is `w * h * 4` bytes, as asked for; both bitmaps are
    // deleted once the cursor has its own copies.
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                // Negative: rows top to bottom, as the shape has them.
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let colour = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let out = std::slice::from_raw_parts_mut(bits as *mut u8, w * h * 4);
        for (to, from) in out.chunks_exact_mut(4).zip(shape.rgba.chunks_exact(4)) {
            // The shape is premultiplied RGBA; Windows wants BGRA with the colours as they are.
            let a = from[3] as u32;
            let plain = |c: u8| if a == 0 { 0 } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            to.copy_from_slice(&[plain(from[2]), plain(from[1]), plain(from[0]), from[3]]);
        }
        // With an alpha channel in the colour bitmap the mask isn't looked at, but there has
        // to be one.
        let mask = CreateBitmap(w as i32, h as i32, 1, 1, None);
        let icon = ICONINFO { fIcon: false.into(), xHotspot: shape.hot_x.max(0) as u32, yHotspot: shape.hot_y.max(0) as u32, hbmMask: mask, hbmColor: colour };
        let made = CreateIconIndirect(&icon);
        let _ = DeleteObject(colour.into());
        let _ = DeleteObject(mask.into());
        made.ok().map(|icon| HCURSOR(icon.0))
    }
}
