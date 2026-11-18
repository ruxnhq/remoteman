//! The window and what moves it from one screen to the next: the list of computers, an attempt
//! to connect, the session, and back.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::Ordering;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use remoteman_core::{Chroma, Codec, Display, Event, Mode, Placement};

use crate::features::FEATURES;
use crate::store::{Quality, Store};
use crate::stream::{Request, Session};
use crate::video::Video;
use crate::{Args, computers, pair, prefs, session, shot, status};

/// A computer to connect to.
#[derive(Clone)]
pub struct Target {
    /// `host:port`.
    pub address: String,
    pub name: String,
}

pub struct App {
    pub args: Args,
    pub app: adw::Application,
    pub window: adw::ApplicationWindow,
    pub toasts: adw::ToastOverlay,
    /// The pages before a session (`front`) and the session itself (`stream`).
    stack: gtk::Stack,
    pub nav: adw::NavigationView,
    pub store: RefCell<Store>,
    pub list: computers::List,
    pub view: session::View,
    /// The picture itself.
    pub video: Rc<Video>,
    pub session: RefCell<Option<Session>>,
    /// The clipboard is shared with the computer of the session (while it allows that).
    pub clipboard_on: Cell<bool>,
    /// A mark of what was last put on this clipboard from the computer, so that it isn't sent
    /// straight back.
    from_computer: Cell<Option<u64>>,
    /// Who the session, or the attempt under way, is with.
    pub target: RefCell<Option<Target>>,
    /// Counts attempts to connect, so that an answer to one that was given up is recognised.
    attempt: Cell<u32>,
    /// Hide the window's bar while connected.
    pub hover_bar: Cell<bool>,
    /// The pointer is at the top edge (or on the bar), so a hidden bar is out.
    pub bar_out: Cell<bool>,
    /// The placement from the command line, until another is chosen in the window.
    pub said_placement: Cell<Option<Placement>>,
    /// What is known of each computer, by address. Today that is only that it didn't answer
    /// when it was last tried in this run; online and asleep wait for `Features::presence`.
    pub reach: RefCell<HashMap<String, status::Reach>>,
    /// How the last attempt or session ended, for `--quit-after`.
    ended: RefCell<String>,
    /// Keys the other computer has been told are down.
    pub held: RefCell<HashSet<u16>>,
    /// The window's size before a session made it larger.
    front_size: Cell<Option<(i32, i32)>>,
    /// Go back to full screen when the next session starts (it was left to reconnect).
    refull: Cell<bool>,
    /// The count of pictures shown when the session started, to tell whether it showed any.
    shown_before: Cell<u32>,
}

const STYLE: &str = "
.rm-video { background: black; }
.rm-state { font-size: 13px; font-weight: 700; }
.rm-dot { min-width: 8px; min-height: 8px; border-radius: 4px; background-color: currentColor; }
.rm-code { font-size: 52px; font-weight: 800; letter-spacing: 2px; font-feature-settings: 'tnum'; }
.rm-figures { padding: 10px 14px; border-radius: 12px; }
.rm-figures .rm-key { font-size: 11px; font-weight: 700; letter-spacing: 0.2px; color: #c9c9cf; }
.rm-figures .rm-value { font-size: 15px; font-weight: 700; font-feature-settings: 'tnum'; }
.rm-menu { padding: 0; }
.rm-icon > button { padding-left: 9px; padding-right: 9px; }
.rm-menu .rm-item { min-height: 32px; padding: 0 9px; border-radius: 6px; font-weight: 400; }
.rm-menu .rm-section { font-size: 13px; font-weight: 700; padding: 6px 9px 4px; }
.rm-menu .rm-hint { font-size: 13px; }
.rm-menu separator { margin: 6px 0; }
.rm-mono .subtitle { font-family: monospace; }
.rm-hint-below { font-size: 13px; }
.rm-keycap { min-width: 14px; min-height: 26px; padding: 0 7px; border-radius: 6px; font-size: 13px; font-weight: 700; background-color: alpha(currentColor, 0.08); box-shadow: inset 0 -2px alpha(currentColor, 0.13); }
";

/// A stream as the menus name it: `AV1, 10-bit`, `HEVC 4:4:4, 10-bit`.
fn mode_name(mode: &Mode) -> String {
    let codec = match mode.codec {
        Codec::Av1 => "AV1",
        Codec::Hevc => "HEVC",
    };
    let chroma = if mode.chroma == Chroma::C444 { " 4:4:4" } else { "" };
    format!("{codec}{chroma}, {}-bit", mode.depth)
}

impl App {
    pub fn streaming(&self) -> bool {
        self.session.borrow().is_some()
    }

    pub fn send(&self, event: Event) {
        if let Some(s) = self.session.borrow().as_ref() {
            s.client.send(event);
        }
    }

    /// Keys typed in the window go to the other computer: there is a session, and nothing of
    /// this app's own (a dialog, a menu) is open in front of it.
    pub fn forwarding(&self) -> bool {
        self.streaming() && self.window.visible_dialog().is_none() && !self.view.menu_open()
    }

    /// Show or hide the window's bar. While a stream is up it is hidden in full screen, and in
    /// a window too if the user chose that; the pointer at the top edge brings it out.
    pub fn place_bar(&self) {
        let hidden = self.streaming() && (self.window.is_fullscreen() || self.hover_bar.get());
        // Over the picture when it comes and goes, so the picture doesn't jump.
        self.view.toolbar.set_extend_content_to_top_edge(hidden);
        self.view.toolbar.set_reveal_top_bars(!hidden || self.bar_out.get() || self.view.menu_open());
        self.view.place_figures(hidden);
    }

    pub fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() { self.window.unfullscreen() } else { self.window.fullscreen() }
    }

    pub fn toast(&self, text: &str) {
        // Plain text: a computer's name may hold anything.
        self.toasts.add_toast(adw::Toast::builder().title(text).use_markup(false).build());
    }

    /// The display to ask the host for: this monitor's, unless the command line says otherwise.
    pub fn display(&self, placement: Placement) -> Display {
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
        Display { width: w & !1, height: h & !1, scale: self.args.scale.unwrap_or(scale), refresh: self.args.refresh.unwrap_or(refresh), placement }
    }

    /// How this display is to sit among a computer's own, and which stream to ask it for.
    pub fn choices(&self, address: &str) -> (Placement, Quality) {
        let store = self.store.borrow();
        let (placement, quality) = match store.find(address) {
            Some(c) => (c.placement, c.quality),
            // Not in the list: a computer named on the command line gets the placement last
            // used, as it always has; one being paired starts as an extra display.
            None => (if self.args.host.is_some() { store.settings.placement } else { Placement::Extend }, store.settings.quality),
        };
        (self.said_placement.get().unwrap_or(placement), quality)
    }

    /// The decoders to offer for a quality, most wanted first.
    pub fn decoders(&self, quality: Quality) -> Vec<Mode> {
        quality.order(&self.args.decoders)
    }

    /// What is known of a computer before trying it, as far as this build may say.
    pub fn reach_of(&self, address: &str) -> Option<status::Reach> {
        self.reach.borrow().get(address).copied().filter(|r| FEATURES.presence || *r == status::Reach::Unreachable)
    }

    /// The name a computer goes by here.
    pub fn name_of(&self, address: &str) -> String {
        self.store.borrow().find(address).map_or_else(|| crate::store::host_part(address).to_owned(), |c| c.name.clone())
    }

    pub fn target_of(&self, address: &str) -> Target {
        Target { address: address.to_owned(), name: self.name_of(address) }
    }

    /// Show a page over the list, in place of whatever was over it.
    pub fn show_page(&self, page: &adw::NavigationPage) {
        self.nav.replace(&[self.list.page.clone(), page.clone()]);
    }

    pub fn show_list(&self) {
        self.nav.replace(&[self.list.page.clone()]);
    }

    /// Try to connect, off the main thread. `done` gets the session or what went wrong, unless
    /// the attempt was given up meanwhile (`give_up`). Returns the attempt's number.
    pub fn attempt(self: &Rc<Self>, target: &Target, pair: bool, done: impl FnOnce(&Rc<Self>, Result<Session, String>) + 'static) -> u32 {
        let id = self.attempt.get().wrapping_add(1);
        self.attempt.set(id);
        let (placement, quality) = self.choices(&target.address);
        let request = Request { host: target.address.clone(), state: self.args.state.clone(), pair, display: self.display(placement), decoders: self.decoders(quality) };
        let app = self.clone();
        glib::spawn_future_local(async move {
            let result = match gio::spawn_blocking(move || Session::start(request)).await {
                Ok(Ok(session)) => Ok(session),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Err(_) => Err("the connection attempt crashed".to_owned()),
            };
            if app.attempt.get() != id {
                // Nobody is waiting for this one any more.
                if let Ok(session) = result {
                    session.client.close();
                }
                return;
            }
            done(&app, result);
        });
        id
    }

    /// Stop waiting for attempt `id`, if it is the one under way.
    pub fn give_up(&self, id: u32) {
        if self.attempt.get() == id {
            self.attempt.set(id.wrapping_add(1));
        }
    }

    /// Connect to a computer: the connecting page, then its session or what went wrong.
    pub fn connect(self: &Rc<Self>, target: Target, pair: bool) {
        if self.streaming() {
            return;
        }
        if self.reach_of(&target.address) == Some(status::Reach::Asleep) {
            // Known to be asleep: offer to wake it rather than wait for no answer.
            self.show_page(&status::problem(self, &target, status::Problem::Asleep));
            return;
        }
        let (placement, _) = self.choices(&target.address);
        let to = target.clone();
        let id = self.attempt(&target, pair, move |app, result| match result {
            Ok(session) => app.enter(session, &to),
            Err(e) => app.failed(&to, &e),
        });
        let page = status::connecting(self, &target, self.display(placement), id);
        self.show_page(&page);
        *self.target.borrow_mut() = Some(target);
    }

    /// An attempt to connect came to nothing.
    fn failed(self: &Rc<Self>, target: &Target, error: &str) {
        *self.ended.borrow_mut() = error.to_owned();
        if self.refull.replace(false) {
            self.window.unfullscreen();
        }
        let problem = status::classify(error);
        if matches!(problem, status::Problem::Unreachable(_)) {
            self.reach.borrow_mut().insert(target.address.clone(), status::Reach::Unreachable);
        }
        computers::refresh(self);
        self.show_page(&status::problem(self, target, problem));
    }

    /// A session has started: show it.
    pub fn enter(self: &Rc<Self>, session: Session, target: &Target) {
        let (placement, quality) = self.choices(&target.address);
        {
            let mut store = self.store.borrow_mut();
            store.used(&target.address, &session.client.host_name, &session.client.host_os);
            store.update(&target.address, |c| (c.placement, c.quality) = (placement, quality));
            store.settings.placement = placement;
            store.save_settings();
        }
        let _ = std::fs::write(self.args.state.join("last-host"), &target.address);
        self.reach.borrow_mut().remove(&target.address);
        let name = self.name_of(&target.address);
        self.window.set_title(Some(&format!("{name} — remoteman")));
        let can_place = session.client.host_features.iter().any(|f| f == "placement");
        // What each choice of quality asks the computer for first.
        let first = |q: Quality| self.decoders(q).first().map(mode_name).unwrap_or_default();
        let sharp = if self.decoders(Quality::Sharp).first().is_some_and(|m| m.chroma == Chroma::C444) { first(Quality::Sharp) } else { String::new() };
        self.view.begin(&name, can_place, placement, quality, (first(Quality::Automatic), sharp));
        self.view.show_figures(self.store.borrow().settings.figures);
        self.video.attach(session.shared.clone());
        self.shown_before.set(self.video.shown_total.get());
        // So does a shared clipboard.
        let shares = session.offers("clipboard") && self.clipboard_on.get();
        self.view.offer_sharing(session.offers("clipboard"), session.offers("files"), shares);
        if shares {
            session.share_clipboard(true);
        }
        // Sound that was on carries over when the same computer is joined again.
        self.view.offer_sound(session.offers("sound"));
        if self.args.sound && session.offers("sound") {
            self.view.sound.set_active(true);
        }
        if self.view.sound.is_active() {
            if let Err(e) = session.set_sound(true, self.args.sound_to.as_deref()) {
                self.toast(&format!("The sound can't be played here: {e:#}"));
            }
        }
        // And the microphone.
        self.view.offer_microphone(session.offers("mic"));
        if self.args.mic && session.offers("mic") {
            self.view.microphone.set_active(true);
        }
        if self.view.microphone.is_active() {
            if let Err(e) = session.set_microphone(true, self.args.mic_from.as_deref()) {
                self.toast(&format!("The microphone can't be used: {e:#}"));
            }
        }
        // And the camera.
        self.view.offer_camera(session.offers("camera"));
        if self.args.camera && session.offers("camera") {
            self.view.camera.set_active(true);
        }
        if self.view.camera.is_active() {
            if let Err(e) = session.set_camera(true, self.args.camera_from.as_deref()) {
                self.toast(&format!("The camera can't be used: {e:#}"));
                self.view.camera.set_active(false);
            }
        }
        *self.session.borrow_mut() = Some(session);
        *self.target.borrow_mut() = Some(Target { address: target.address.clone(), name });
        self.ended.borrow_mut().clear();
        // Room for the picture, if the list's window was small.
        if !self.window.is_maximized() && !self.window.is_fullscreen() && (self.window.width() < 1280 || self.window.height() < 800) && self.window.width() > 0 {
            self.front_size.set(Some((self.window.width(), self.window.height())));
            self.window.set_default_size(1280, 800);
        }
        self.stack.set_visible_child_name("stream");
        self.show_list();
        computers::refresh(self);
        self.place_bar();
        self.video.picture.grab_focus();
        self.grab_shortcuts();
        if self.refull.replace(false) || self.store.borrow().settings.fullscreen {
            self.window.fullscreen();
        }
    }

    /// Super, Alt+Tab and the like go to the other computer while this has focus, if wanted.
    pub fn grab_shortcuts(&self) {
        let Some(top) = self.window.surface().and_downcast::<gdk::Toplevel>() else { return };
        if self.streaming() && self.store.borrow().settings.shortcuts {
            top.inhibit_system_shortcuts(None::<&gdk::Event>);
        } else {
            top.restore_system_shortcuts();
        }
    }

    /// End the session and go back to the list. `reason` is shown if there is one.
    pub fn stop(self: &Rc<Self>, reason: Option<&str>) {
        if !self.end(false) {
            return;
        }
        match reason {
            Some(r) => {
                *self.ended.borrow_mut() = r.to_owned();
                if let Some(target) = self.target.borrow().clone() {
                    let why = r.strip_prefix("The connection ended: ").unwrap_or(r);
                    let shown = self.video.shown_total.get() != self.shown_before.get();
                    self.show_page(&status::problem(self, &target, status::ended(why, shown)));
                }
            }
            None => self.ended.borrow_mut().clear(),
        }
    }

    /// End the session; `false` if there was none. `stay`: the window keeps its size and full
    /// screen, because another session is about to start.
    fn end(self: &Rc<Self>, stay: bool) -> bool {
        let Some(session) = self.session.borrow_mut().take() else { return false };
        self.video.free_gl();
        let _ = session.set_sound(false, None);
        let _ = session.set_microphone(false, None);
        let _ = session.set_camera(false, None);
        session.client.close();
        if !stay {
            self.view.sound.set_active(false);
            self.view.microphone.set_active(false);
            self.view.camera.set_active(false);
            self.clipboard_on.set(false);
            self.view.show_clipboard(false);
        }
        if let Some(top) = self.window.surface().and_downcast::<gdk::Toplevel>() {
            top.restore_system_shortcuts();
        }
        self.held.borrow_mut().clear();
        if stay {
            self.refull.set(self.window.is_fullscreen());
        } else {
            // Back to the size the list had, if the session made the window larger. Leaving
            // full screen brings its own resize from the compositor, so wait for that first.
            let was = self.front_size.take();
            let app = self.clone();
            let shrink = move || {
                if let Some((w, h)) = was.filter(|_| !app.streaming() && !app.window.is_maximized() && !app.window.is_fullscreen()) {
                    app.window.set_default_size(w, h);
                }
            };
            if self.window.is_fullscreen() {
                self.window.unfullscreen();
                glib::timeout_add_local_once(std::time::Duration::from_millis(250), shrink);
            } else {
                shrink();
            }
        }
        self.window.set_title(Some("remoteman"));
        self.video.detach();
        self.stack.set_visible_child_name("front");
        self.bar_out.set(false);
        self.place_bar();
        true
    }

    /// Start the session again, after a choice that the computer only takes at the start.
    fn reconnect(self: &Rc<Self>) {
        let Some(target) = self.target.borrow().clone() else { return };
        if self.end(true) {
            self.connect(target, false);
        }
    }

    /// The Display menu: where this display sits among the computer's own.
    pub fn set_placement(self: &Rc<Self>, placement: Placement) {
        let Some(target) = self.target.borrow().clone() else { return };
        self.said_placement.set(None);
        self.store.borrow_mut().update(&target.address, |c| c.placement = placement);
        self.reconnect();
    }

    /// The Picture menu: which stream to ask for.
    pub fn set_quality(self: &Rc<Self>, quality: Quality) {
        let Some(target) = self.target.borrow().clone() else { return };
        self.store.borrow_mut().update(&target.address, |c| c.quality = quality);
        self.reconnect();
    }

    pub fn set_figures(&self, on: bool) {
        {
            let mut store = self.store.borrow_mut();
            store.settings.figures = on;
            store.save_settings();
        }
        self.view.show_figures(on);
    }

    /// Share the clipboard with the session's computer, or stop.
    pub fn set_clipboard(self: &Rc<Self>, on: bool) {
        let offered = self.session.borrow().as_ref().is_some_and(|s| s.offers("clipboard"));
        let on = on && offered;
        self.clipboard_on.set(on);
        self.view.show_clipboard(on);
        if let Some(s) = self.session.borrow().as_ref() {
            s.share_clipboard(on);
        }
        if on {
            // What is on this clipboard now goes over, as the computer's comes here.
            self.from_computer.set(None);
            self.copied_here();
        }
    }

    /// This computer's clipboard changed (or sharing has just begun): send text if it holds
    /// any, else a picture. What came from the other computer is not sent back.
    pub fn copied_here(self: &Rc<Self>) {
        if !self.clipboard_on.get() || self.session.borrow().is_none() {
            return;
        }
        let clipboard = self.window.clipboard();
        let formats = clipboard.formats();
        let app = self.clone();
        if formats.contains_type(glib::Type::STRING) {
            clipboard.read_text_async(gio::Cancellable::NONE, move |text| {
                if let Ok(Some(text)) = text {
                    app.send_copy("text/plain;charset=utf-8", text.as_bytes().to_vec());
                }
            });
        } else if formats.contains_type(gdk::Texture::static_type()) {
            clipboard.read_texture_async(gio::Cancellable::NONE, move |picture| {
                if let Ok(Some(picture)) = picture {
                    app.send_copy("image/png", picture.save_to_png_bytes().to_vec());
                }
            });
        }
    }

    fn send_copy(&self, kind: &str, bytes: Vec<u8>) {
        if self.from_computer.get() == Some(mark_of(kind, &bytes)) || !self.clipboard_on.get() {
            return;
        }
        if let Some(s) = self.session.borrow().as_ref() {
            s.send_copy(kind, bytes);
        }
    }

    /// Send a file to the session's computer.
    pub fn send_file(&self, path: &std::path::Path) {
        let sent = self.session.borrow().as_ref().map(|s| s.send_file(path));
        match sent {
            Some(Ok(name)) => self.toast(&format!("Sending {name}…")),
            Some(Err(e)) => self.toast(&format!("{} can't be sent: {e}", path.display())),
            None => {}
        }
    }

    /// The computer's owner switched something on or off while this device is connected: the
    /// bar offers what it offers now. A clipboard that was shared, or chosen before it was
    /// allowed, follows; sound and a microphone that are withdrawn stop.
    fn follow_features(&self) {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        if !s.shared.features_changed.swap(false, Ordering::Relaxed) {
            return;
        }
        let was_shared = self.view.clipboard_shown();
        let shares = s.offers("clipboard") && self.clipboard_on.get();
        self.view.offer_sharing(s.offers("clipboard"), s.offers("files"), shares);
        if shares && !was_shared {
            s.share_clipboard(true);
        }
        self.view.offer_sound(s.offers("sound"));
        self.view.offer_microphone(s.offers("mic"));
        self.view.offer_camera(s.offers("camera"));
        if self.args.ui.is_some() {
            println!("~ the computer now offers: clipboard {}, files {}", s.offers("clipboard"), s.offers("files"));
        }
    }

    /// What arrived from the computer since the last frame: copies for this clipboard, and
    /// word of files sent.
    fn arrivals(&self) {
        self.follow_features();
        let (copies, results) = match self.session.borrow().as_ref() {
            Some(s) => (std::mem::take(&mut *s.shared.copies.lock().unwrap()), std::mem::take(&mut *s.shared.results.lock().unwrap())),
            None => return,
        };
        // Only the newest copy matters.
        if let (Some((kind, bytes)), true) = (copies.into_iter().last(), self.clipboard_on.get()) {
            self.from_computer.set(Some(mark_of(&kind, &bytes)));
            let clipboard = self.window.clipboard();
            if kind.starts_with("text/plain") {
                clipboard.set_text(&String::from_utf8_lossy(&bytes));
            } else if kind == "image/png" {
                let content = gdk::ContentProvider::for_bytes("image/png", &glib::Bytes::from_owned(bytes));
                let _ = clipboard.set_content(Some(&content));
            }
        }
        for (ok, said) in results {
            self.toast(&if ok { format!("Sent: it is at {said}") } else { format!("The file wasn't taken: {said}") });
        }
    }

    /// Once per frame of this window: show the newest picture, and notice the session ending.
    fn tick(self: &Rc<Self>) {
        self.arrivals();
        let (picture, closed, cursor) = match self.session.borrow().as_ref() {
            Some(s) => (s.shared.picture.lock().unwrap().take(), s.shared.closed.lock().unwrap().clone(), s.shared.cursor.lock().unwrap().take()),
            None => return,
        };
        if let Some(c) = cursor {
            // The other computer's cursor becomes this window's own pointer, so it moves with
            // the hand and not a pipeline's delay later. Its image is in the other display's
            // pixels; GDK counts in this one's, so it is scaled to match.
            let pointer = if c.width == 0 {
                gdk::Cursor::from_name("none", None)
            } else {
                let texture = gdk::MemoryTexture::new(c.width as i32, c.height as i32, gdk::MemoryFormat::R8g8b8a8Premultiplied, &glib::Bytes::from_owned(c.rgba), c.width as usize * 4);
                Some(gdk::Cursor::from_texture(&texture, c.hot_x, c.hot_y, None))
            };
            self.video.stack.set_cursor(pointer.as_ref());
        }
        if let Some(p) = picture {
            self.video.show(p);
        }
        if let Some(reason) = closed {
            // Looking at the window with a stand-in host, whose frames are not video: stay.
            if self.args.ui.is_some() && reason.starts_with("decode") {
                return;
            }
            self.stop(Some(&format!("The connection ended: {reason}")));
        }
    }

    /// Once a second: the figures over the picture.
    fn second(&self) {
        let session = self.session.borrow();
        let Some(s) = session.as_ref() else { return };
        let stats = s.client.stats();
        let shown = self.video.shown.replace(0);
        let decoder = s.shared.decoder.lock().unwrap().clone();
        let decoded = s.shared.decoded.swap(0, Ordering::Relaxed);
        let age = s.shared.age_us.load(Ordering::Relaxed) as f64 / 1000.0;
        let decode = s.shared.decode_us_max.swap(0, Ordering::Relaxed) as f64 / 1000.0;
        let replaced = s.shared.replaced.load(Ordering::Relaxed);
        let round_trip = stats.round_trip_us as f64 / 1000.0;
        if self.args.quit_after.is_some() {
            let line = format!(
                "{} · {}\n{} shown, {} decoded a second · {:.1} ms old · decode up to {:.1} ms\n{} lost, {} replaced unseen · round trip {:.1} ms",
                decoder,
                self.video.path.get(),
                shown,
                decoded,
                age,
                decode,
                stats.lost,
                replaced,
                round_trip,
            );
            println!("{} | drawn {}", line.replace('\n', " | "), self.video.drawn.replace(0));
        }
        let megabits = s.shared.bytes.swap(0, Ordering::Relaxed) as f64 * 8.0 / 1e6;
        self.view.figures.set(&[
            shown.to_string(),
            decoded.to_string(),
            format!("{age:.1} ms"),
            format!("{decode:.1} ms"),
            format!("{megabits:.1} Mb/s"),
            format!("{round_trip:.1} ms"),
            stats.lost.to_string(),
            replaced.to_string(),
            self.video.path.get().to_owned(),
            decoder,
        ]);
        self.view.set_format(*s.shared.format.lock().unwrap());
    }

    /// The menu behind the three lines, the same on every page.
    pub fn main_menu(&self) -> gtk::MenuButton {
        let menu = gio::Menu::new();
        menu.append(Some("Preferences"), Some("win.preferences"));
        menu.append(Some("About remoteman"), Some("win.about"));
        gtk::MenuButton::builder().icon_name("open-menu-symbolic").tooltip_text("Main Menu").menu_model(&menu).primary(true).build()
    }
}

pub fn build(application: &adw::Application, args: Args) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(STYLE);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
    if args.ui.is_some() {
        // For pictures of the window: nothing caught half way through a transition.
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_enable_animations(false);
        }
    }

    let store = Store::load(&args.state);
    // With a computer on the command line the window opens for its picture; otherwise for the list.
    let (width, height) = if args.host.is_some() { (1280, 800) } else { (900, 680) };
    let window = adw::ApplicationWindow::builder().application(application).title("remoteman").default_width(width).default_height(height).build();

    let video = Video::new(args.dump.clone());
    let list = computers::build();
    let nav = adw::NavigationView::new();
    nav.add(&list.page);
    let view = session::build(&video);
    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
    stack.add_named(&nav, Some("front"));
    stack.add_named(&view.toolbar, Some("stream"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    window.set_content(Some(&toasts));

    let hover_bar = args.hover_bar.unwrap_or(store.settings.hover_bar);
    let app = Rc::new(App {
        app: application.clone(),
        window: window.clone(),
        toasts,
        stack,
        nav,
        store: RefCell::new(store),
        list,
        view,
        video,
        session: RefCell::new(None),
        clipboard_on: Cell::new(false),
        from_computer: Cell::new(None),
        target: RefCell::new(None),
        attempt: Cell::new(0),
        hover_bar: Cell::new(hover_bar),
        bar_out: Cell::new(false),
        said_placement: Cell::new(args.placement),
        reach: RefCell::new(HashMap::new()),
        ended: RefCell::new(String::new()),
        held: RefCell::new(HashSet::new()),
        front_size: Cell::new(None),
        refull: Cell::new(false),
        shown_before: Cell::new(0),
        args: args.clone(),
    });

    let action = |name: &str, run: fn(&Rc<App>)| {
        let a = gio::SimpleAction::new(name, None);
        let app = app.clone();
        a.connect_activate(move |_, _| run(&app));
        window.add_action(&a);
    };
    action("preferences", prefs::preferences);
    action("about", |app| {
        let about = adw::AboutDialog::builder().application_name("remoteman").version(env!("CARGO_PKG_VERSION")).comments("See and use another computer's screen from this one.").build();
        about.present(Some(&app.window));
    });

    computers::wire(&app);
    session::wire(&app);
    // Whatever is copied on this computer while the clipboard is shared goes to the other.
    window.clipboard().connect_changed({
        let app = app.clone();
        move |_| app.copied_here()
    });
    computers::refresh(&app);

    window.connect_fullscreened_notify({
        let app = app.clone();
        move |w| {
            app.view.set_fullscreen(w.is_fullscreen());
            app.place_bar();
        }
    });
    window.add_tick_callback({
        let app = app.clone();
        move |_, _| {
            app.tick();
            glib::ControlFlow::Continue
        }
    });
    glib::timeout_add_seconds_local(1, {
        let app = app.clone();
        move || {
            app.second();
            glib::ControlFlow::Continue
        }
    });
    window.connect_close_request({
        let app = app.clone();
        move |_| {
            app.stop(None);
            glib::Propagation::Proceed
        }
    });

    window.present();
    match &args.host {
        // A computer on the command line: connect at once.
        Some(host) => {
            let address = crate::store::with_port(host);
            let target = app.target_of(&address);
            app.connect(target, args.pair);
        }
        None if args.pair => pair::open(&app, None, false),
        None => {}
    }
    if let Some(secs) = args.quit_after {
        let app = app.clone();
        glib::timeout_add_seconds_local_once(secs, move || {
            let lost = app.session.borrow().as_ref().map(|s| s.client.stats().lost);
            println!("pictures shown {}; frames lost {:?}; ended with: {}", app.video.shown_total.get(), lost, app.ended.borrow());
            app.stop(None);
            app.app.quit();
        });
    }
    if let Some(script) = args.ui.clone() {
        shot::run(&app, script);
    }
}

/// A mark of a copy, to tell it from the next one.
fn mark_of(kind: &str, bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, bytes).hash(&mut h);
    h.finish()
}
