//! remoteman for Linux: a GTK 4 viewer.
//!
//!     remoteman [HOST:PORT] [--pair] [--size WxH] [--scale S] [--refresh HZ]
//!               [--decoders av1,hevc444,hevc] [--state DIR]
//!
//! With a host on the command line it connects at once. For testing: `--quit-after SECONDS`
//! leaves by itself and prints what it saw, and `--dump FILE.png` saves the 200th picture shown.
//! F11 switches full screen; everything else typed goes to the other computer.

mod decode;
mod stream;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use remoteman_core::{Button, Chroma, Codec, Display, Event, Mode, keys};

use decode::{Layout, Picture, Pixels, Surface};

unsafe extern "C" {
    fn rm_glview_new(cuda_ctx: *mut std::ffi::c_void, err: *mut std::ffi::c_char, errlen: i32) -> *mut std::ffi::c_void;
    fn rm_glview_upload(v: *mut std::ffi::c_void, layout: i32, width: i32, height: i32, data: *const u64, pitch: *const i32) -> i32;
    fn rm_glview_draw(v: *mut std::ffi::c_void, out_w: *mut i32, out_h: *mut i32);
    fn rm_glview_read(w: i32, h: i32, out: *mut u8);
    fn rm_glview_error(v: *const std::ffi::c_void) -> *const std::ffi::c_char;
    fn rm_glview_free(v: *mut std::ffi::c_void);
}
use stream::{Request, Session};

#[derive(Clone)]
struct Args {
    host: Option<String>,
    pair: bool,
    size: Option<(u32, u32)>,
    scale: Option<f32>,
    refresh: Option<u32>,
    decoders: Vec<Mode>,
    state: PathBuf,
    quit_after: Option<u32>,
    dump: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Args> {
    use anyhow::Context;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/state")).join("remoteman");
    let mut a = Args { host: None, pair: false, size: None, scale: None, refresh: None, decoders: Vec::new(), state, quit_after: None, dump: None };
    let mut decoders = "av1,hevc444,hevc".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => a.pair = true,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                a.size = Some((w.parse()?, h.parse()?));
            }
            "--scale" => a.scale = Some(value()?.parse()?),
            "--refresh" => a.refresh = Some(value()?.parse()?),
            "--decoders" => decoders = value()?,
            "--state" => a.state = value()?.into(),
            "--quit-after" => a.quit_after = Some(value()?.parse()?),
            "--dump" => a.dump = Some(value()?.into()),
            other if other.starts_with("--") => anyhow::bail!("unknown option {other}"),
            host => a.host = Some(host.to_owned()),
        }
    }
    for d in decoders.split(',') {
        a.decoders.push(match d {
            "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
            "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
            "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
            other => anyhow::bail!("unknown decoder {other}"),
        });
    }
    Ok(a)
}

/// The parts of the window that the stream touches.
struct Ui {
    window: adw::ApplicationWindow,
    toolbar: adw::ToolbarView,
    stack: gtk::Stack,
    picture: gtk::Picture,
    /// Where NVDEC pictures are drawn, and the two of them stacked.
    gl: gtk::GLArea,
    video_stack: gtk::Stack,
    /// The C helper's state for `gl` (null until the first picture).
    gl_view: Cell<*mut std::ffi::c_void>,
    /// An NVDEC picture waiting for `gl` to draw it.
    gl_pending: RefCell<Option<Picture>>,
    /// The surface last drawn; holding it keeps the decoder's GPU context alive under `gl_view`.
    gl_last: RefCell<Option<Surface>>,
    gl_dump: Cell<bool>,
    /// Times the GL widget was drawn since the figures were last read.
    drawn: Cell<u32>,
    /// How pictures are reaching the screen, for the figures.
    path: Cell<&'static str>,
    stats: gtk::Label,
    status: gtk::Label,
    connect: gtk::Button,
    leave: gtk::Button,
    host: adw::EntryRow,
    pair: adw::SwitchRow,
    args: Args,
    session: RefCell<Option<Session>>,
    colour: gdk::ColorState,
    /// Pictures shown since the stats were last read, and in all.
    shown: Cell<u32>,
    shown_total: Cell<u32>,
    /// The size of the picture on screen, to place the pointer.
    video: Cell<(u32, u32)>,
}

impl Ui {
    fn send(&self, event: Event) {
        if let Some(s) = self.session.borrow().as_ref() {
            s.client.send(event);
        }
    }

    fn streaming(&self) -> bool {
        self.session.borrow().is_some()
    }

    /// A spot in the picture widget as fractions of the remote display, allowing for the bars
    /// that keep its shape.
    fn fraction(&self, x: f64, y: f64) -> (f32, f32) {
        let (vw, vh) = self.video.get();
        let (ww, wh) = (self.video_stack.width() as f64, self.video_stack.height() as f64);
        if vw == 0 || vh == 0 || ww <= 0.0 || wh <= 0.0 {
            return (0.0, 0.0);
        }
        let scale = (ww / vw as f64).min(wh / vh as f64);
        let (dw, dh) = (vw as f64 * scale, vh as f64 * scale);
        let (ox, oy) = ((ww - dw) / 2.0, (wh - dh) / 2.0);
        (((x - ox) / dw).clamp(0.0, 1.0) as f32, ((y - oy) / dh).clamp(0.0, 1.0) as f32)
    }

    /// The display to ask the host for: this monitor's, unless the command line says otherwise.
    fn display(&self) -> Display {
        let monitor = self
            .window
            .surface()
            .and_then(|s| gdk::Display::default().and_then(|d| d.monitor_at_surface(&s)))
            .or_else(|| gdk::Display::default().and_then(|d| d.monitors().item(0)).and_downcast::<gdk::Monitor>());
        let (mut w, mut h, mut scale, mut refresh) = (1920u32, 1080u32, 1.0f32, 60u32);
        if let Some(m) = monitor {
            let g = m.geometry();
            scale = m.scale() as f32;
            w = (g.width() as f64 * m.scale()).round() as u32;
            h = (g.height() as f64 * m.scale()).round() as u32;
            // Decoding and uploading is sized for 120 a second for now.
            refresh = ((m.refresh_rate() as f64 / 1000.0).round() as u32).clamp(30, 120);
        }
        if let Some((sw, sh)) = self.args.size {
            (w, h) = (sw, sh);
        }
        Display { width: w & !1, height: h & !1, scale: self.args.scale.unwrap_or(scale), refresh: self.args.refresh.unwrap_or(refresh) }
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
                    .set_display(&gtk::prelude::WidgetExt::display(&self.window))
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
                        if let Some(s) = self.session.borrow().as_ref() {
                            if !s.shared.no_dmabuf.swap(true, Ordering::Relaxed) {
                                eprintln!("remoteman: GTK won't take the decoder's GPU pictures ({e}); copying them instead");
                            }
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
            if let Some(s) = self.session.borrow().as_ref() {
                if !s.shared.no_dmabuf.swap(true, Ordering::Relaxed) {
                    eprintln!("remoteman: can't draw the decoder's GPU pictures ({what}); copying them instead");
                }
            }
        };
        if let Some(p) = self.gl_pending.borrow_mut().take() {
            if let Pixels::Cuda { layout, context, data, pitch, hold } = p.pixels {
                // SAFETY: GTK has made the widget's GL context current for this call; the
                // pointers are a live CUDA surface, kept by `hold`.
                unsafe {
                    if self.gl_view.get().is_null() {
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
            if let Some(path) = &self.args.dump {
                match texture.save_to_png(path) {
                    Ok(()) => println!("saved picture 200 to {}", path.display()),
                    Err(e) => println!("couldn't save {}: {e}", path.display()),
                }
            }
        }
    }

    /// Let go of the GL widget's textures and their tie to the decoder's GPU context.
    fn free_gl(&self) {
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

    fn start(self: &Rc<Self>, host: String, pair: bool) {
        if self.streaming() {
            return;
        }
        self.status.set_text("Connecting…");
        self.status.remove_css_class("error");
        self.connect.set_sensitive(false);
        let request = Request { host: host.clone(), state: self.args.state.clone(), pair, display: self.display(), decoders: self.args.decoders.clone() };
        let ui = self.clone();
        glib::spawn_future_local(async move {
            let result = gio::spawn_blocking(move || Session::start(request)).await;
            ui.connect.set_sensitive(true);
            match result {
                Ok(Ok(session)) => {
                    let _ = std::fs::write(ui.args.state.join("last-host"), &host);
                    ui.window.set_title(Some(&format!("{} — remoteman", session.client.host_name)));
                    *ui.session.borrow_mut() = Some(session);
                    ui.status.set_text("");
                    ui.stack.set_visible_child_name("stream");
                    ui.leave.set_visible(true);
                    ui.picture.grab_focus();
                    // Super, Alt+Tab and the like go to the other computer while this has focus.
                    if let Some(top) = ui.window.surface().and_downcast::<gdk::Toplevel>() {
                        top.inhibit_system_shortcuts(None::<&gdk::Event>);
                    }
                }
                Ok(Err(e)) => ui.fail(&format!("{e:#}")),
                Err(_) => ui.fail("the connection attempt crashed"),
            }
        });
    }

    fn fail(&self, message: &str) {
        self.status.set_text(message);
        self.status.add_css_class("error");
    }

    /// End the session and go back to the first page. `reason` is shown if there is one.
    fn stop(&self, reason: Option<&str>) {
        let Some(session) = self.session.borrow_mut().take() else { return };
        self.free_gl();
        session.client.close();
        if let Some(top) = self.window.surface().and_downcast::<gdk::Toplevel>() {
            top.restore_system_shortcuts();
        }
        self.window.unfullscreen();
        self.window.set_title(Some("remoteman"));
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.video.set((0, 0));
        self.leave.set_visible(false);
        self.stack.set_visible_child_name("connect");
        match reason {
            Some(r) => self.fail(r),
            None => self.status.set_text(""),
        }
    }

    /// Once per frame of this window: show the newest picture, and notice the session ending.
    fn tick(&self) {
        let (picture, closed) = match self.session.borrow().as_ref() {
            Some(s) => (s.shared.picture.lock().unwrap().take(), s.shared.closed.lock().unwrap().clone()),
            None => return,
        };
        if let Some(p) = picture {
            self.video.set((p.width, p.height));
            let shown = match &p.pixels {
                Pixels::Cuda { .. } => {
                    if let Some(e) = self.gl.error() {
                        // No OpenGL here: tell the decoder to send the slow kind instead.
                        if let Some(s) = self.session.borrow().as_ref() {
                            if !s.shared.no_dmabuf.swap(true, Ordering::Relaxed) {
                                eprintln!("remoteman: no OpenGL for the decoder's GPU pictures ({e}); copying them instead");
                            }
                        }
                        return;
                    }
                    self.path.set("GPU to GPU");
                    self.video_stack.set_visible_child_name("gl");
                    if self.shown_total.get() + 1 == 200 && self.args.dump.is_some() {
                        self.gl_dump.set(true);
                    }
                    *self.gl_pending.borrow_mut() = Some(p);
                    self.gl.queue_render();
                    true
                }
                other => {
                    self.path.set(if matches!(other, Pixels::Dmabuf { .. }) { "no copy" } else { "COPIED THROUGH CPU MEMORY (slow)" });
                    self.video_stack.set_visible_child_name("picture");
                    match self.texture(p) {
                        Some(texture) => {
                            self.picture.set_paintable(Some(&texture));
                            self.drawn.set(self.drawn.get() + 1);
                            if self.shown_total.get() + 1 == 200 {
                                if let Some(path) = &self.args.dump {
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
        if let Some(reason) = closed {
            self.stop(Some(&format!("The connection ended: {reason}")));
        }
    }

    /// Once a second: the figures over the picture.
    fn second(&self) {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        let stats = s.client.stats();
        let shown = self.shown.replace(0);
        let line = format!(
            "{} · {}\n{} shown, {} decoded a second · {:.1} ms old · decode up to {:.1} ms\n{} lost, {} replaced unseen · round trip {:.1} ms",
            s.shared.decoder.lock().unwrap(),
            self.path.get(),
            shown,
            s.shared.decoded.swap(0, Ordering::Relaxed),
            s.shared.age_us.load(Ordering::Relaxed) as f64 / 1000.0,
            s.shared.decode_us_max.swap(0, Ordering::Relaxed) as f64 / 1000.0,
            stats.lost,
            s.shared.replaced.load(Ordering::Relaxed),
            stats.round_trip_us as f64 / 1000.0,
        );
        self.stats.set_text(&line);
        if self.args.quit_after.is_some() {
            println!("{} | drawn {}", line.replace('\n', " | "), self.drawn.replace(0));
        }
    }
}

fn build(app: &adw::Application, args: Args) {
    let window = adw::ApplicationWindow::builder().application(app).title("remoteman").default_width(1280).default_height(800).build();
    let header = adw::HeaderBar::new();
    let leave = gtk::Button::builder().label("Disconnect").visible(false).build();
    let fullscreen = gtk::Button::builder().icon_name("view-fullscreen-symbolic").tooltip_text("Full screen (F11)").build();
    header.pack_start(&leave);
    header.pack_end(&fullscreen);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);

    // The first page: where to connect.
    let last = std::fs::read_to_string(args.state.join("last-host")).unwrap_or_default();
    let host = adw::EntryRow::builder().title("Address").text(args.host.clone().unwrap_or(last.trim().to_owned())).build();
    let pair = adw::SwitchRow::builder().title("Pair").subtitle("Trust this computer the first time. It must be in pairing mode.").active(args.pair).build();
    let group = adw::PreferencesGroup::builder().title("Connect to a computer").description("Its name or address, and port: blackbox:7791").build();
    group.add(&host);
    group.add(&pair);
    let connect = gtk::Button::builder().label("Connect").halign(gtk::Align::Center).build();
    connect.add_css_class("suggested-action");
    connect.add_css_class("pill");
    let status = gtk::Label::builder().wrap(true).justify(gtk::Justification::Center).build();
    let page = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(36).margin_bottom(36).margin_start(18).margin_end(18).valign(gtk::Align::Center).build();
    page.append(&group);
    page.append(&connect);
    page.append(&status);
    let clamp = adw::Clamp::builder().maximum_size(480).child(&page).build();

    // The second page: the other computer's screen.
    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Contain).can_shrink(true).focusable(true).build();
    let stats = gtk::Label::builder().halign(gtk::Align::Start).valign(gtk::Align::Start).margin_start(12).margin_top(12).xalign(0.0).can_target(false).build();
    stats.add_css_class("osd");
    stats.add_css_class("numeric");
    // Lets the compositor show the decoder's surface itself where it can, with no pass through GTK.
    let offload = gtk::GraphicsOffload::new(Some(&picture));
    offload.set_black_background(true);
    // NVDEC pictures are drawn here instead, through OpenGL.
    let gl = gtk::GLArea::builder().hexpand(true).vexpand(true).auto_render(false).build();
    let video_stack = gtk::Stack::new();
    video_stack.add_named(&offload, Some("picture"));
    video_stack.add_named(&gl, Some("gl"));
    let overlay = gtk::Overlay::builder().child(&video_stack).build();
    overlay.add_overlay(&stats);
    overlay.add_css_class("view");

    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    stack.add_named(&clamp, Some("connect"));
    stack.add_named(&overlay, Some("stream"));
    toolbar.set_content(Some(&stack));
    window.set_content(Some(&toolbar));

    // Desktop pixels as they are: BT.709 primaries, the sRGB curve, BT.709 matrix, limited range.
    let cicp = gdk::CicpParams::new();
    cicp.set_color_primaries(1);
    cicp.set_transfer_function(13);
    cicp.set_matrix_coefficients(1);
    cicp.set_range(gdk::CicpRange::Narrow);
    let colour = cicp.build_color_state().expect("a standard colour description");

    let ui = Rc::new(Ui {
        window: window.clone(),
        toolbar,
        stack,
        picture: picture.clone(),
        gl: gl.clone(),
        video_stack: video_stack.clone(),
        gl_view: Cell::new(std::ptr::null_mut()),
        gl_pending: RefCell::new(None),
        gl_last: RefCell::new(None),
        gl_dump: Cell::new(false),
        drawn: Cell::new(0),
        path: Cell::new(""),
        stats,
        status,
        connect: connect.clone(),
        leave: leave.clone(),
        host: host.clone(),
        pair,
        args: args.clone(),
        session: RefCell::new(None),
        colour,
        shown: Cell::new(0),
        shown_total: Cell::new(0),
        video: Cell::new((0, 0)),
    });

    let go = {
        let ui = ui.clone();
        move || {
            let host = ui.host.text().trim().to_owned();
            if host.is_empty() {
                ui.fail("Enter the computer's address first.");
            } else {
                ui.start(if host.contains(':') { host } else { format!("{host}:7791") }, ui.pair.is_active());
            }
        }
    };
    connect.connect_clicked({
        let go = go.clone();
        move |_| go()
    });
    host.connect_entry_activated({
        let go = go.clone();
        move |_| go()
    });
    leave.connect_clicked({
        let ui = ui.clone();
        move |_| ui.stop(None)
    });
    let toggle_fullscreen = {
        let ui = ui.clone();
        move || {
            if ui.window.is_fullscreen() { ui.window.unfullscreen() } else { ui.window.fullscreen() }
        }
    };
    fullscreen.connect_clicked({
        let t = toggle_fullscreen.clone();
        move |_| t()
    });
    window.connect_fullscreened_notify({
        let ui = ui.clone();
        move |w| ui.toolbar.set_reveal_top_bars(!w.is_fullscreen())
    });

    // Keys: by physical position, to the other computer, before anything here sees them.
    let keys_ctl = gtk::EventControllerKey::new();
    keys_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys_ctl.connect_key_pressed({
        let ui = ui.clone();
        let t = toggle_fullscreen.clone();
        move |_, keyval, keycode, _| {
            if !ui.streaming() {
                return glib::Propagation::Proceed;
            }
            if keyval == gdk::Key::F11 {
                t();
            } else if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                ui.send(Event::Key { usage, down: true });
            }
            glib::Propagation::Stop
        }
    });
    keys_ctl.connect_key_released({
        let ui = ui.clone();
        move |_, keyval, keycode, _| {
            if ui.streaming() && keyval != gdk::Key::F11 {
                if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                    ui.send(Event::Key { usage, down: false });
                }
            }
        }
    });
    window.add_controller(keys_ctl);

    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion({
        let ui = ui.clone();
        move |_, x, y| {
            let (x, y) = ui.fraction(x, y);
            ui.send(Event::PointerTo { x, y });
        }
    });
    video_stack.add_controller(motion);

    let click = gtk::GestureClick::builder().button(0).build();
    let button = |g: &gtk::GestureClick| match g.current_button() {
        2 => Button::Middle,
        3 => Button::Right,
        _ => Button::Left,
    };
    click.connect_pressed({
        let ui = ui.clone();
        move |g, _, x, y| {
            ui.picture.grab_focus();
            let (x, y) = ui.fraction(x, y);
            ui.send(Event::PointerTo { x, y });
            ui.send(Event::Button { button: button(g), down: true });
        }
    });
    click.connect_released({
        let ui = ui.clone();
        move |g, _, _, _| ui.send(Event::Button { button: button(g), down: false })
    });
    video_stack.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.connect_scroll({
        let ui = ui.clone();
        move |c, dx, dy| {
            // A wheel reports notches; a touchpad reports pixels.
            let k = if c.unit() == gdk::ScrollUnit::Wheel { 40.0 } else { 1.0 };
            ui.send(Event::Scroll { dx: (dx * k) as f32, dy: (dy * k) as f32 });
            glib::Propagation::Stop
        }
    });
    video_stack.add_controller(scroll);

    window.add_tick_callback({
        let ui = ui.clone();
        move |_, _| {
            ui.tick();
            glib::ControlFlow::Continue
        }
    });
    gl.connect_render({
        let ui = ui.clone();
        move |_, _| {
            ui.render();
            glib::Propagation::Stop
        }
    });
    gl.connect_unrealize({
        let ui = ui.clone();
        move |_| ui.free_gl()
    });
    glib::timeout_add_seconds_local(1, {
        let ui = ui.clone();
        move || {
            ui.second();
            glib::ControlFlow::Continue
        }
    });
    window.connect_close_request({
        let ui = ui.clone();
        move |_| {
            ui.stop(None);
            glib::Propagation::Proceed
        }
    });

    window.present();
    if args.host.is_some() {
        go();
    }
    if let Some(secs) = args.quit_after {
        let ui = ui.clone();
        let app = app.clone();
        glib::timeout_add_seconds_local_once(secs, move || {
            let lost = ui.session.borrow().as_ref().map(|s| s.client.stats().lost);
            println!("pictures shown {}; frames lost {:?}; ended with: {}", ui.shown_total.get(), lost, ui.status.text());
            ui.stop(None);
            app.quit();
        });
    }
}

fn main() -> glib::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("remoteman: {e:#}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder().application_id("dev.remoteman.Viewer").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(move |app| build(app, args.clone()));
    app.run_with_args::<&str>(&[])
}
