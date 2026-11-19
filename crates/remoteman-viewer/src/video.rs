//! The picture: decoded frames onto the screen. NVDEC pictures go from the decoder's GPU memory
//! into OpenGL textures and are drawn by the C helper (`shim/glview.c`); VA-API pictures are
//! handed to GTK as DMA-BUFs; anything else is a copy in memory. Nothing here knows about the
//! rest of the window.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::decode::{Layout, Picture, Pixels, Surface};
use crate::stream::Shared;

unsafe extern "C" {
    fn rm_glview_new(cuda_ctx: *mut std::ffi::c_void, err: *mut std::ffi::c_char, errlen: i32) -> *mut std::ffi::c_void;
    fn rm_glview_upload(v: *mut std::ffi::c_void, layout: i32, width: i32, height: i32, data: *const u64, pitch: *const i32) -> i32;
    fn rm_glview_draw(v: *mut std::ffi::c_void, out_w: *mut i32, out_h: *mut i32);
    fn rm_glview_read(w: i32, h: i32, out: *mut u8);
    fn rm_glview_error(v: *const std::ffi::c_void) -> *const std::ffi::c_char;
    fn rm_glview_free(v: *mut std::ffi::c_void);
}

pub struct Video {
    pub picture: gtk::Picture,
    /// Where NVDEC pictures are drawn, and the two of them stacked.
    gl: gtk::GLArea,
    pub stack: gtk::Stack,
    /// The C helper's state for `gl` (null until the first picture).
    gl_view: Cell<*mut std::ffi::c_void>,
    /// The decoder's GPU context `gl_view` was made for.
    gl_context: Cell<*mut std::ffi::c_void>,
    /// An NVDEC picture waiting for `gl` to draw it.
    gl_pending: RefCell<Option<Picture>>,
    /// The surface last drawn; holding it keeps the decoder's GPU context alive under `gl_view`.
    gl_last: RefCell<Option<Surface>>,
    gl_dump: Cell<bool>,
    /// Times the GL widget was drawn since the figures were last read.
    pub drawn: Cell<u32>,
    /// How pictures are reaching the screen, for the figures.
    pub path: Cell<&'static str>,
    colour: gdk::ColorState,
    /// Pictures shown since the stats were last read, and in all.
    pub shown: Cell<u32>,
    pub shown_total: Cell<u32>,
    /// The size of the picture on screen, to place the pointer.
    pub size: Cell<(u32, u32)>,
    /// Where to save the 200th picture shown, if asked.
    dump: Option<PathBuf>,
    /// The running session's side of the decode thread, to tell it when GPU pictures can't be
    /// drawn.
    shared: RefCell<Option<Arc<Shared>>>,
}

impl Video {
    pub fn new(dump: Option<PathBuf>) -> Rc<Self> {
        let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).can_shrink(true).focusable(true).build();
        // Lets the compositor show the decoder's surface itself where it can, with no pass through GTK.
        let offload = gtk::GraphicsOffload::new(Some(&picture));
        offload.set_black_background(true);
        // NVDEC pictures are drawn here instead, through OpenGL.
        let gl = gtk::GLArea::builder().hexpand(true).vexpand(true).auto_render(false).build();
        let stack = gtk::Stack::new();
        stack.add_named(&offload, Some("picture"));
        stack.add_named(&gl, Some("gl"));

        // Desktop pixels as they are: BT.709 primaries, the sRGB curve, BT.709 matrix, limited range.
        let cicp = gdk::CicpParams::new();
        cicp.set_color_primaries(1);
        cicp.set_transfer_function(13);
        cicp.set_matrix_coefficients(1);
        cicp.set_range(gdk::CicpRange::Narrow);
        let colour = cicp.build_color_state().expect("a standard colour description");

        let video = Rc::new(Self {
            picture,
            gl: gl.clone(),
            stack,
            gl_view: Cell::new(std::ptr::null_mut()),
            gl_context: Cell::new(std::ptr::null_mut()),
            gl_pending: RefCell::new(None),
            gl_last: RefCell::new(None),
            gl_dump: Cell::new(false),
            drawn: Cell::new(0),
            path: Cell::new(""),
            colour,
            shown: Cell::new(0),
            shown_total: Cell::new(0),
            size: Cell::new((0, 0)),
            dump,
            shared: RefCell::new(None),
        });
        gl.connect_render({
            let video = video.clone();
            move |_, _| {
                video.render();
                glib::Propagation::Stop
            }
        });
        gl.connect_unrealize({
            let video = video.clone();
            move |_| video.free_gl()
        });
        video
    }

    /// A session has started: its pictures come here from now on.
    pub fn attach(&self, shared: Arc<Shared>) {
        *self.shared.borrow_mut() = Some(shared);
    }

    /// The session is over (and `free_gl` has been called): forget its last picture.
    pub fn detach(&self) {
        self.shared.borrow_mut().take();
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.size.set((0, 0));
    }

    /// Tell the decoder to send the slower kind of picture from now on; `true` the first time.
    fn stop_gpu_pictures(&self) -> bool {
        match self.shared.borrow().as_ref() {
            Some(s) => !s.no_dmabuf.swap(true, Ordering::Relaxed),
            None => false,
        }
    }

    /// A spot in the picture widget as fractions of the remote display, allowing for the bars
    /// that keep its shape.
    pub fn fraction(&self, x: f64, y: f64) -> (f32, f32) {
        let (vw, vh) = self.size.get();
        let (ww, wh) = (self.stack.width() as f64, self.stack.height() as f64);
        if vw == 0 || vh == 0 || ww <= 0.0 || wh <= 0.0 {
            return (0.0, 0.0);
        }
        let scale = (ww / vw as f64).min(wh / vh as f64);
        let (dw, dh) = (vw as f64 * scale, vh as f64 * scale);
        let (ox, oy) = ((ww - dw) / 2.0, (wh - dh) / 2.0);
        (((x - ox) / dw).clamp(0.0, 1.0) as f32, ((y - oy) / dh).clamp(0.0, 1.0) as f32)
    }

    /// The other way: where a spot of the remote display is in the picture widget, and how
    /// many of this widget's units one remote pixel takes. `None` before the first picture.
    pub fn spot(&self, fx: f32, fy: f32) -> Option<(f64, f64, f64)> {
        let (vw, vh) = self.size.get();
        let (ww, wh) = (self.stack.width() as f64, self.stack.height() as f64);
        if vw == 0 || vh == 0 || ww <= 0.0 || wh <= 0.0 {
            return None;
        }
        let scale = (ww / vw as f64).min(wh / vh as f64);
        let (dw, dh) = (vw as f64 * scale, vh as f64 * scale);
        Some(((ww - dw) / 2.0 + fx as f64 * dw, (wh - dh) / 2.0 + fy as f64 * dh, scale))
    }

    /// The picture as something GTK can draw. `None`: GTK refused it, and the decoder has been
    /// told to send the slower kind.
    fn texture(&self, p: Picture) -> Option<gdk::Texture> {
        match p.pixels {
            Pixels::Memory { layout, data, planes } => {
                let format = match layout {
                    Layout::Nv12 => gdk::MemoryFormat::G8B8r8420,
                    Layout::P010 => gdk::MemoryFormat::G10x6B10x6r10x6420,
                    Layout::Yuv444High16 => gdk::MemoryFormat::G16B16R16444,
                    Layout::Yuv444Low10 => gdk::MemoryFormat::X6g10X6b10X6r10444,
                };
                let mut b = gdk::MemoryTextureBuilder::new()
                    .set_width(p.width as i32)
                    .set_height(p.height as i32)
                    .set_format(format)
                    .set_color_state(&self.colour)
                    .set_stride(planes[0].1);
                for (i, (offset, stride)) in planes.iter().enumerate() {
                    b = b.set_offset(i as u32, *offset).set_stride_for_plane(i as u32, *stride);
                }
                Some(b.set_bytes(Some(&glib::Bytes::from_owned(data))).build())
            }
            // Drawn by `render`, never as a texture.
            Pixels::Cuda { .. } => None,
            Pixels::Dmabuf { fourcc, modifier, planes, hold } => {
                let mut b = gdk::DmabufTextureBuilder::new()
                    .set_display(&gtk::prelude::WidgetExt::display(&self.picture))
                    .set_width(p.width)
                    .set_height(p.height)
                    .set_fourcc(fourcc)
                    .set_modifier(modifier)
                    .set_n_planes(planes.len() as u32)
                    .set_color_state(Some(&self.colour));
                for (i, (fd, offset, stride)) in planes.iter().enumerate() {
                    // SAFETY: the descriptors belong to `hold`, which lives until GTK lets the
                    // texture go.
                    b = unsafe { b.set_fd(i as u32, *fd) }.set_offset(i as u32, *offset).set_stride(i as u32, *stride);
                }
                // SAFETY: as above; the release function is what keeps the surface alive.
                match unsafe { b.build_with_release_func(move || drop(hold)) } {
                    Ok(t) => Some(t),
                    Err(e) => {
                        if self.stop_gpu_pictures() {
                            eprintln!("remoteman: GTK won't take the decoder's GPU pictures ({e}); copying them instead");
                        }
                        None
                    }
                }
            }
        }
    }

    /// GTK asks for the GL widget's contents: move a waiting NVDEC picture into the textures
    /// (on the GPU) and draw.
    fn render(&self) {
        let fail = |what: &str| {
            if self.stop_gpu_pictures() {
                eprintln!("remoteman: can't draw the decoder's GPU pictures ({what}); copying them instead");
            }
        };
        if let Some(p) = self.gl_pending.borrow_mut().take() {
            if let Pixels::Cuda { layout, context, data, pitch, hold } = p.pixels {
                // SAFETY: GTK has made the widget's GL context current for this call; the
                // pointers are a live CUDA surface, kept by `hold`.
                unsafe {
                    // Another decoder (the picture started afresh at another size, or in
                    // another codec) has a GPU context of its own: the helper made for the
                    // old one goes while that context is still held (`gl_last`), and a new
                    // one is made. Drawing the new decoder's pictures through the old
                    // context crashed.
                    if !self.gl_view.get().is_null() && self.gl_context.get() != context as *mut std::ffi::c_void {
                        rm_glview_free(self.gl_view.replace(std::ptr::null_mut()));
                    }
                    if self.gl_view.get().is_null() {
                        self.gl_context.set(context as *mut std::ffi::c_void);
                        let mut err = [0 as std::ffi::c_char; 256];
                        let v = rm_glview_new(context as *mut _, err.as_mut_ptr(), 256);
                        if v.is_null() {
                            fail(&std::ffi::CStr::from_ptr(err.as_ptr()).to_string_lossy());
                        }
                        self.gl_view.set(v);
                    }
                    let v = self.gl_view.get();
                    if !v.is_null() && rm_glview_upload(v, layout, p.width as i32, p.height as i32, data.as_ptr(), pitch.as_ptr()) != 0 {
                        fail(&std::ffi::CStr::from_ptr(rm_glview_error(v)).to_string_lossy());
                    }
                }
                *self.gl_last.borrow_mut() = Some(hold);
            }
        }
        let v = self.gl_view.get();
        if v.is_null() {
            return;
        }
        let (mut w, mut h) = (0, 0);
        // SAFETY: as above.
        unsafe { rm_glview_draw(v, &mut w, &mut h) };
        self.drawn.set(self.drawn.get() + 1);
        if self.gl_dump.replace(false) && w > 0 && h > 0 {
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            // SAFETY: the buffer is exactly w * h * 4 bytes.
            unsafe { rm_glview_read(w, h, rgba.as_mut_ptr()) };
            // OpenGL's first row is the bottom one.
            let row = (w * 4) as usize;
            let flipped: Vec<u8> = rgba.chunks_exact(row).rev().flatten().copied().collect();
            let texture = gdk::MemoryTexture::new(w, h, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(flipped), row);
            if let Some(path) = &self.dump {
                match texture.save_to_png(path) {
                    Ok(()) => println!("saved picture 200 to {}", path.display()),
                    Err(e) => println!("couldn't save {}: {e}", path.display()),
                }
            }
        }
    }

    /// Let go of the GL widget's textures and their tie to the decoder's GPU context.
    pub fn free_gl(&self) {
        let v = self.gl_view.replace(std::ptr::null_mut());
        if !v.is_null() {
            self.gl.make_current();
            // SAFETY: the widget's context is current, and the CUDA context is still alive
            // through `gl_last`.
            unsafe { rm_glview_free(v) };
        }
        self.gl_pending.borrow_mut().take();
        self.gl_last.borrow_mut().take();
    }

    /// Put the newest decoded picture on the screen.
    pub fn show(&self, p: Picture) {
        self.size.set((p.width, p.height));
        let shown = match &p.pixels {
            Pixels::Cuda { .. } => {
                if let Some(e) = self.gl.error() {
                    // No OpenGL here: tell the decoder to send the slow kind instead.
                    if self.stop_gpu_pictures() {
                        eprintln!("remoteman: no OpenGL for the decoder's GPU pictures ({e}); copying them instead");
                    }
                    return;
                }
                self.path.set("GPU to GPU");
                self.stack.set_visible_child_name("gl");
                if self.shown_total.get() + 1 == 200 && self.dump.is_some() {
                    self.gl_dump.set(true);
                }
                *self.gl_pending.borrow_mut() = Some(p);
                self.gl.queue_render();
                true
            }
            other => {
                self.path.set(if matches!(other, Pixels::Dmabuf { .. }) { "no copy" } else { "COPIED THROUGH CPU MEMORY (slow)" });
                self.stack.set_visible_child_name("picture");
                match self.texture(p) {
                    Some(texture) => {
                        self.picture.set_paintable(Some(&texture));
                        self.drawn.set(self.drawn.get() + 1);
                        if self.shown_total.get() + 1 == 200 {
                            if let Some(path) = &self.dump {
                                match texture.save_to_png(path) {
                                    Ok(()) => println!("saved picture 200 to {}", path.display()),
                                    Err(e) => println!("couldn't save {}: {e}", path.display()),
                                }
                            }
                        }
                        true
                    }
                    None => false,
                }
            }
        };
        if shown {
            self.shown.set(self.shown.get() + 1);
            self.shown_total.set(self.shown_total.get() + 1);
        }
    }
}
