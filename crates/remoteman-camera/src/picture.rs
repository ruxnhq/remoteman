//! One picture as both outputs take it: 8-bit 4:2:0 in three planes (I420), rows packed with no
//! padding, video range. The packers turn it into the other layouts camera consumers read.

/// A picture of the camera's fixed size.
pub(crate) struct Picture {
    pub width: u32,
    pub height: u32,
    /// Y (`width * height`), then Cb, then Cr (`width / 2 * height / 2` each).
    pub data: Vec<u8>,
}

/// The pixel layouts offered to consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Y, then interleaved CbCr.
    Nv12,
    /// Y, Cb, Cr.
    I420,
    /// Packed 4:2:2: Y0 Cb Y1 Cr. What V4L2 calls YUYV.
    Yuy2,
}

impl Layout {
    /// Bytes of one picture.
    pub fn size(self, width: u32, height: u32) -> usize {
        let pixels = width as usize * height as usize;
        match self {
            Layout::Nv12 | Layout::I420 => pixels * 3 / 2,
            Layout::Yuy2 => pixels * 2,
        }
    }

    /// Bytes of one row of the first plane.
    pub fn stride(self, width: u32) -> usize {
        match self {
            Layout::Nv12 | Layout::I420 => width as usize,
            Layout::Yuy2 => width as usize * 2,
        }
    }
}

impl Picture {
    /// Bytes of an I420 picture of this size (both sides even).
    pub fn bytes(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3 / 2
    }

    /// The neutral picture: black.
    pub fn black(width: u32, height: u32) -> Picture {
        let mut data = vec![0u8; Self::bytes(width, height)];
        fill_black(&mut data, width, height);
        Picture { width, height, data }
    }

    /// Write the picture into `out` (at least `layout.size(..)` bytes) in `layout`.
    pub fn pack(&self, layout: Layout, out: &mut [u8]) {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch) = (w / 2, h / 2);
        let (y, chroma) = self.data.split_at(w * h);
        let (cb, cr) = chroma.split_at(cw * ch);
        match layout {
            Layout::I420 => out[..self.data.len()].copy_from_slice(&self.data),
            Layout::Nv12 => {
                out[..w * h].copy_from_slice(y);
                for ((pair, &b), &r) in out[w * h..w * h + cw * ch * 2].chunks_exact_mut(2).zip(cb).zip(cr) {
                    pair[0] = b;
                    pair[1] = r;
                }
            }
            Layout::Yuy2 => {
                // Each chroma row serves two picture rows.
                for (row, line) in out[..w * h * 2].chunks_exact_mut(w * 2).enumerate() {
                    let y = &y[row * w..(row + 1) * w];
                    let cb = &cb[row / 2 * cw..(row / 2 + 1) * cw];
                    let cr = &cr[row / 2 * cw..(row / 2 + 1) * cw];
                    for (((quad, y), &b), &r) in line.chunks_exact_mut(4).zip(y.chunks_exact(2)).zip(cb).zip(cr) {
                        quad[0] = y[0];
                        quad[1] = b;
                        quad[2] = y[1];
                        quad[3] = r;
                    }
                }
            }
        }
    }
}

/// Black in video range: Y 16, Cb and Cr 128.
pub(crate) fn fill_black(data: &mut [u8], width: u32, height: u32) {
    let pixels = width as usize * height as usize;
    data[..pixels].fill(16);
    data[pixels..].fill(128);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Picture {
        // 4x2: Y 1..8, Cb 50 51, Cr 60 61.
        Picture { width: 4, height: 2, data: vec![1, 2, 3, 4, 5, 6, 7, 8, 50, 51, 60, 61] }
    }

    #[test]
    fn packs_nv12() {
        let mut out = vec![0; Layout::Nv12.size(4, 2)];
        sample().pack(Layout::Nv12, &mut out);
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 50, 60, 51, 61]);
    }

    #[test]
    fn packs_i420() {
        let mut out = vec![0; Layout::I420.size(4, 2)];
        sample().pack(Layout::I420, &mut out);
        assert_eq!(out, sample().data);
    }

    #[test]
    fn packs_yuy2() {
        let mut out = vec![0; Layout::Yuy2.size(4, 2)];
        sample().pack(Layout::Yuy2, &mut out);
        assert_eq!(out, [1, 50, 2, 60, 3, 51, 4, 61, 5, 50, 6, 60, 7, 51, 8, 61]);
    }

    #[test]
    fn black_is_video_black() {
        let p = Picture::black(8, 2);
        assert!(p.data[..16].iter().all(|&v| v == 16));
        assert!(p.data[16..].iter().all(|&v| v == 128));
        assert_eq!(p.data.len(), 24);
    }
}
