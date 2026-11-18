//! H.264 frames in, pictures of the camera's fixed size out. Decoding is FFmpeg's software
//! decoder (a 720p frame takes about a millisecond; the GPU is left to the screen).
//!
//! A picture of another size or shape than the camera's is scaled to fit, keeping its
//! proportions, and centred on black: see the crate documentation for why the camera's size
//! never changes.

use std::sync::Once;

use ff::codec::{self, threading};
use ff::ffi;
use ff::format::Pixel;
use ff::software::scaling;
use ff::util::frame::Video;
use ffmpeg_next as ff;

use crate::picture::{Picture, fill_black};

/// What became of one frame.
pub(crate) enum Decoded {
    Picture(Picture),
    /// It could not be decoded; nothing is until the next keyframe.
    Lost,
    /// Withheld: still waiting for that keyframe.
    Waiting,
}

#[derive(PartialEq)]
enum Need {
    /// No frame seen yet.
    First,
    /// A keyframe, after a frame that could not be decoded.
    Keyframe,
    Nothing,
}

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = ff::init();
        // The decoder would otherwise write a line to stderr for every damaged frame.
        ff::util::log::set_level(ff::util::log::Level::Fatal);
    });
}

struct Scaler(scaling::Context);

// SAFETY: a scaling context has no ties to the thread that made it; it is only ever used through
// `&mut`, by one thread at a time.
unsafe impl Send for Scaler {}

/// The scaler for one kind of incoming picture, and its output.
struct Fit {
    source: (Pixel, u32, u32),
    scaler: Scaler,
    scaled: Video,
}

pub(crate) struct Decoder {
    decoder: ff::decoder::Video,
    frame: Video,
    fit: Option<Fit>,
    /// The camera's size.
    width: u32,
    height: u32,
    need: Need,
}

impl Decoder {
    /// A decoder whose pictures are all `width` by `height` (both even).
    pub fn new(width: u32, height: u32) -> anyhow::Result<Decoder> {
        use anyhow::Context;
        init();
        // By name: `find` could hand back a hardware wrapper where FFmpeg is built with one.
        let found = ff::decoder::find_by_name("h264").context("FFmpeg has no software H.264 decoder")?;
        let mut ctx = codec::Context::new_with_codec(found);
        // One frame in, its picture out, on the calling thread: no B-frames to wait for, and
        // nothing here is slow enough to be worth more threads.
        ctx.set_threading(threading::Config { kind: threading::Type::Slice, count: 1 });
        ctx.set_flags(codec::Flags::LOW_DELAY);
        let decoder = ctx.decoder().video().context("open the H.264 decoder")?;
        Ok(Decoder { decoder, frame: Video::empty(), fit: None, width, height, need: Need::First })
    }

    /// One whole frame (Annex-B), `key` as the sender marked it.
    pub fn decode(&mut self, data: &[u8], key: bool) -> Decoded {
        if self.need != Need::Nothing && !key {
            // Nothing to decode it against. The first time, say so, so a keyframe gets asked for.
            let first = self.need == Need::First;
            self.need = Need::Keyframe;
            return if first { Decoded::Lost } else { Decoded::Waiting };
        }
        if self.need == Need::Keyframe {
            // Forget the damaged sequence.
            self.decoder.flush();
        }
        match self.picture(data) {
            Some(picture) => {
                self.need = Need::Nothing;
                Decoded::Picture(picture)
            }
            None => {
                self.need = Need::Keyframe;
                Decoded::Lost
            }
        }
    }

    fn picture(&mut self, data: &[u8]) -> Option<Picture> {
        // (An empty packet would tell the decoder the stream has ended.)
        if data.is_empty() {
            return None;
        }
        let packet = ff::Packet::copy(data);
        self.decoder.send_packet(&packet).ok()?;
        self.decoder.receive_frame(&mut self.frame).ok()?;
        // SAFETY: `receive_frame` succeeded, so this is a valid frame.
        let (errors, flags) = unsafe { ((*self.frame.as_ptr()).decode_error_flags, (*self.frame.as_ptr()).flags) };
        // A picture the decoder had to patch up (missing slices or references) is not shown.
        if errors != 0 || flags & ffi::AV_FRAME_FLAG_CORRUPT != 0 {
            return None;
        }
        self.fitted()
    }

    /// The decoded frame as a picture of the camera's size.
    fn fitted(&mut self) -> Option<Picture> {
        let (cw, ch) = (self.width, self.height);
        let source = (self.frame.format(), self.frame.width(), self.frame.height());
        let (format, sw, sh) = source;
        if sw == 0 || sh == 0 {
            return None;
        }
        let mut data = vec![0u8; Picture::bytes(cw, ch)];
        let (y, chroma) = data.split_at_mut(cw as usize * ch as usize);
        let (cb, cr) = chroma.split_at_mut(cw as usize / 2 * (ch as usize / 2));
        if (sw, sh) == (cw, ch) && format == Pixel::YUV420P {
            // The usual case: as decoded.
            copy_plane(y, cw, (0, 0), &self.frame, 0, (cw, ch));
            copy_plane(cb, cw / 2, (0, 0), &self.frame, 1, (cw / 2, ch / 2));
            copy_plane(cr, cw / 2, (0, 0), &self.frame, 2, (cw / 2, ch / 2));
        } else {
            // Another size (a device turned on its side) or sample format (full range, 4:2:2):
            // scale to fit and centre on black.
            let (dw, dh) = fit_size((sw, sh), (cw, ch));
            if self.fit.as_ref().is_none_or(|f| f.source != source) {
                let scaler = scaling::Context::get(format, sw, sh, Pixel::YUV420P, dw, dh, scaling::Flags::BICUBIC).ok()?;
                self.fit = Some(Fit { source, scaler: Scaler(scaler), scaled: Video::empty() });
            }
            let fit = self.fit.as_mut()?;
            fit.scaler.0.run(&self.frame, &mut fit.scaled).ok()?;
            // Even offsets, so the chroma samples line up.
            let (ox, oy) = (((cw - dw) / 2) & !1, ((ch - dh) / 2) & !1);
            fill_black(y, cw, ch);
            cb.fill(128);
            cr.fill(128);
            copy_plane(y, cw, (ox, oy), &fit.scaled, 0, (dw, dh));
            copy_plane(cb, cw / 2, (ox / 2, oy / 2), &fit.scaled, 1, (dw / 2, dh / 2));
            copy_plane(cr, cw / 2, (ox / 2, oy / 2), &fit.scaled, 2, (dw / 2, dh / 2));
        }
        Some(Picture { width: cw, height: ch, data })
    }
}

/// The largest even size with the proportions of `source` that fits inside `canvas`.
pub(crate) fn fit_size(source: (u32, u32), canvas: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (source.0 as u64, source.1 as u64);
    let (cw, ch) = (canvas.0 as u64, canvas.1 as u64);
    let (w, h) = if sw * ch >= sh * cw {
        // Relatively wider than the canvas: full width.
        (cw, (sh * cw + sw / 2) / sw)
    } else {
        ((sw * ch + sh / 2) / sh, ch)
    };
    let even = |v: u64, most: u64| ((v + 1) & !1).clamp(2, most) as u32;
    (even(w, cw), even(h, ch))
}

/// Copy `size` samples of plane `index` of `frame` into `plane` (rows of `stride`) at `at`.
fn copy_plane(plane: &mut [u8], stride: u32, at: (u32, u32), frame: &Video, index: usize, size: (u32, u32)) {
    let (stride, x, y, w, h) = (stride as usize, at.0 as usize, at.1 as usize, size.0 as usize, size.1 as usize);
    let (source, source_stride) = (frame.data(index), frame.stride(index));
    for row in 0..h {
        let from = &source[row * source_stride..row * source_stride + w];
        plane[(y + row) * stride + x..(y + row) * stride + x + w].copy_from_slice(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_keeping_proportions() {
        // The same shape: the whole canvas.
        assert_eq!(fit_size((1280, 720), (1280, 720)), (1280, 720));
        assert_eq!(fit_size((1920, 1080), (1280, 720)), (1280, 720));
        // Turned on its side: full height, bars left and right.
        assert_eq!(fit_size((720, 1280), (1280, 720)), (406, 720));
        // Wider than the canvas: full width, bars above and below.
        assert_eq!(fit_size((1280, 720), (720, 1280)), (720, 406));
        assert_eq!(fit_size((640, 480), (1280, 720)), (960, 720));
        // Never nothing, never more than the canvas.
        assert_eq!(fit_size((4000, 2), (1280, 720)), (1280, 2));
        assert_eq!(fit_size((1, 4000), (1280, 720)), (2, 720));
    }

    #[test]
    fn bad_data_is_lost_not_fatal() {
        let mut d = Decoder::new(1280, 720).unwrap();
        // Not a keyframe first: reported once, then withheld.
        assert!(matches!(d.decode(&[0, 0, 0, 1, 0x41, 0x9a, 0x00], false), Decoded::Lost));
        assert!(matches!(d.decode(&[0, 0, 0, 1, 0x41, 0x9a, 0x00], false), Decoded::Waiting));
        // "Keyframes" that are nothing of the kind.
        assert!(matches!(d.decode(&[], true), Decoded::Lost));
        assert!(matches!(d.decode(&[0xff; 4096], true), Decoded::Lost));
        assert!(matches!(d.decode(&[0, 0, 0, 1, 0x67, 0xff, 0xff, 0xff, 0, 0, 0, 1, 0x65, 0x88, 0x84], true), Decoded::Lost));
        let noise: Vec<u8> = (0..20000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert!(matches!(d.decode(&noise, true), Decoded::Lost));
        assert!(matches!(d.decode(&noise, false), Decoded::Waiting));
    }
}
