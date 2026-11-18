//! The session: the other computer's picture, the bar over it with its controls and menus, the
//! figures, and the keys and pointer going back.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use remoteman_core::{Button, Event, Placement, keys};

use crate::features::{FEATURES, gate};
use crate::store::Quality;
use crate::video::Video;
use crate::window::App;

/// One line of a menu: a mark (radio or tick), what it does, and a quiet note at the end.
struct Item {
    button: gtk::Button,
    mark: Option<gtk::CheckButton>,
    label: gtk::Label,
    hint: gtk::Label,
}

enum Mark<'a> {
    None,
    Tick,
    /// One of several; `Some` joins the group of an earlier one.
    Radio(Option<&'a gtk::CheckButton>),
}

fn item(label: &str, hint: &str, mark: Mark) -> Item {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mark = match mark {
        Mark::None => None,
        Mark::Tick => Some(gtk::CheckButton::new()),
        Mark::Radio(group) => {
            let radio = gtk::CheckButton::new();
            // A group of one still draws as a radio button.
            radio.set_group(Some(group.unwrap_or(&gtk::CheckButton::new())));
            Some(radio)
        }
    };
    if let Some(mark) = &mark {
        // Only a picture of the state: the whole line is the button.
        mark.set_can_target(false);
        mark.set_focusable(false);
        row.append(mark);
    }
    let label = gtk::Label::builder().label(label).xalign(0.0).hexpand(true).build();
    row.append(&label);
    let hint = gtk::Label::builder().label(hint).css_classes(["rm-hint", "dim-label"]).build();
    row.append(&hint);
    let button = gtk::Button::builder().child(&row).css_classes(["flat", "rm-item"]).build();
    Item { button, mark, label, hint }
}

fn section(title: &str) -> gtk::Label {
    gtk::Label::builder().label(title).xalign(0.0).css_classes(["rm-section", "dim-label"]).build()
}

fn popover(lines: &[&gtk::Widget], width: i32) -> gtk::Popover {
    let list = gtk::Box::builder().orientation(gtk::Orientation::Vertical).width_request(width).css_classes(["rm-menu"]).build();
    for line in lines {
        list.append(*line);
    }
    let popover = gtk::Popover::builder().child(&list).build();
    popover.add_css_class("menu");
    popover
}

/// Two sliders: the design's sign for picture quality, which the icon theme doesn't have.
fn sliders() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(16).content_height(16).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
    area.set_draw_func(|area, cr, w, h| {
        // In the text colour, as a symbolic icon would be.
        let c = area.color();
        cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), c.alpha().into());
        let k = w.min(h) as f64 / 24.0;
        cr.scale(k, k);
        cr.set_line_width(2.3);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        for (from, to, y) in [(4.0, 13.0, 7.0), (19.0, 20.0, 7.0), (4.0, 5.0, 17.0), (11.0, 20.0, 17.0)] {
            cr.move_to(from, y);
            cr.line_to(to, y);
        }
        for (x, y) in [(16.0, 7.0), (8.0, 17.0)] {
            cr.new_sub_path();
            cr.arc(x, y, 2.5, 0.0, std::f64::consts::TAU);
        }
        let _ = cr.stroke();
    });
    area
}

/// The numbers over the picture.
pub struct Figures {
    root: gtk::Grid,
    values: Vec<gtk::Label>,
}

const FIGURES: [&str; 10] = ["FRAMES PER SECOND", "DECODED PER SECOND", "PICTURE AGE", "DECODE TIME, AT MOST", "BITRATE", "ROUND TRIP", "LOST FRAMES", "REPLACED UNSEEN", "DRAWN", "DECODER"];

impl Figures {
    fn new() -> Self {
        let root = gtk::Grid::builder().column_spacing(22).row_spacing(8).halign(gtk::Align::End).valign(gtk::Align::Start).margin_end(16).margin_top(16).can_target(false).visible(false).css_classes(["osd", "rm-figures"]).build();
        let mut values = Vec::new();
        for (i, key) in FIGURES.iter().enumerate() {
            let cell = gtk::Box::new(gtk::Orientation::Vertical, 0);
            cell.append(&gtk::Label::builder().label(*key).xalign(0.0).css_classes(["rm-key"]).build());
            let value = gtk::Label::builder().label("–").xalign(0.0).css_classes(["rm-value"]).build();
            cell.append(&value);
            // Three across; the decoder's name is long and gets the last row's remainder.
            root.attach(&cell, (i % 3) as i32, (i / 3) as i32, if i == FIGURES.len() - 1 { 3 } else { 1 }, 1);
            values.push(value);
        }
        Self { root, values }
    }

    /// The values, in the order of `FIGURES`.
    pub fn set(&self, values: &[String]) {
        for (label, value) in self.values.iter().zip(values) {
            label.set_text(if value.is_empty() { "–" } else { value });
        }
    }
}

/// The session's part of the window.
pub struct View {
    pub toolbar: adw::ToolbarView,
    header: adw::HeaderBar,
    title: adw::WindowTitle,
    disconnect: gtk::Button,
    fullscreen: gtk::Button,
    /// The buttons that open menus: send keys, display, picture, the main one, and sharing.
    menus: [gtk::MenuButton; 5],
    share_clipboard: Item,
    send_file: Item,
    placements: [(Placement, Item); 3],
    qualities: [(Quality, Item); 2],
    mirror: Item,
    show_figures: Item,
    /// Plays the computer's sound here while it is pressed in.
    pub sound: gtk::ToggleButton,
    /// Lets the computer use this device's microphone while it is pressed in.
    pub microphone: gtk::ToggleButton,
    pub figures: Figures,
}

fn icon_button(icon: &str, name: &str) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(name).build()
}

fn icon_toggle(icon: &str, name: &str) -> gtk::ToggleButton {
    gtk::ToggleButton::builder().icon_name(icon).tooltip_text(name).build()
}

pub fn build(video: &Video) -> View {
    let header = adw::HeaderBar::new();
    let title = adw::WindowTitle::new("", "");
    header.set_title_widget(Some(&title));
    let disconnect = gtk::Button::builder().label("Disconnect").css_classes(["flat"]).build();
    header.pack_start(&disconnect);

    // Send keys: the ones this computer or this window would keep for itself.
    let keys_menu = gio::Menu::new();
    for (label, combo) in [("Ctrl+Alt+Delete", "ctrl-alt-delete"), ("Super", "super"), ("Alt+Tab", "alt-tab"), ("Alt+F4", "alt-f4"), ("Print Screen", "print"), ("F11", "f11")] {
        keys_menu.append(Some(label), Some(&format!("win.send-keys::{combo}")));
    }
    let send_keys = gtk::MenuButton::builder().icon_name("input-keyboard-symbolic").tooltip_text("Send Keys").menu_model(&keys_menu).build();

    // Display: where this display sits among the computer's own.
    let extra = item("Extra Display", "Beside the computer's own", Mark::Radio(None));
    let main = item("Main Display", "", Mark::Radio(extra.mark.as_ref()));
    let only = item("Only Display", "Computer's screens off", Mark::Radio(extra.mark.as_ref()));
    let second = item("Add a Second Display…", "", Mark::None);
    gate(second.button.clone(), FEATURES.second_display);
    let mirror = item("Mirror a Monitor", "", Mark::None);
    if let Some(row) = mirror.button.child().and_downcast::<gtk::Box>() {
        row.append(&gtk::Image::builder().icon_name("go-next-symbolic").css_classes(["dim-label"]).build());
    }
    gate(mirror.button.clone(), FEATURES.mirror);
    let display_popover = popover(
        &[section("Use This Display As").upcast_ref(), extra.button.upcast_ref(), main.button.upcast_ref(), only.button.upcast_ref(), gtk::Separator::new(gtk::Orientation::Horizontal).upcast_ref(), second.button.upcast_ref(), mirror.button.upcast_ref()],
        328,
    );
    let display = gtk::MenuButton::builder().icon_name("video-display-symbolic").tooltip_text("Display").popover(&display_popover).build();

    // Picture: which stream, and the figures.
    let automatic = item("Automatic", "", Mark::Radio(None));
    let sharp = item("Sharp Text (4:4:4)", "", Mark::Radio(automatic.mark.as_ref()));
    let hdr = item("HDR", "When mirroring", Mark::Radio(automatic.mark.as_ref()));
    gate(hdr.button.clone(), FEATURES.hdr);
    let show_figures = item("Show Figures", "", Mark::Tick);
    let picture_popover = popover(
        &[section("Picture Quality").upcast_ref(), automatic.button.upcast_ref(), sharp.button.upcast_ref(), hdr.button.upcast_ref(), gtk::Separator::new(gtk::Orientation::Horizontal).upcast_ref(), show_figures.button.upcast_ref()],
        280,
    );
    let picture = gtk::MenuButton::builder().child(&sliders()).tooltip_text("Picture Quality").popover(&picture_popover).css_classes(["flat", "rm-icon"]).build();

    // Things the product can't do yet, where they will be.
    let pointer = gate(icon_button("input-mouse-symbolic", "Pointer Mode"), FEATURES.pointer_capture);
    let camera = gate(icon_toggle("camera-video-symbolic", "Camera"), FEATURES.camera);
    let microphone = gate(icon_toggle("audio-input-microphone-symbolic", "Microphone"), FEATURES.microphone);
    let sound = gate(icon_toggle("audio-volume-high-symbolic", "Sound"), FEATURES.sound);
    // Sharing: the clipboard and files.
    let share_clipboard = item("Share the Clipboard", "Both ways", Mark::Tick);
    gate(share_clipboard.button.clone(), FEATURES.clipboard);
    let send_file = item("Send a File…", "", Mark::None);
    gate(send_file.button.clone(), FEATURES.files);
    let sharing_popover = popover(&[section("Clipboard and Files").upcast_ref(), share_clipboard.button.upcast_ref(), send_file.button.upcast_ref()], 280);
    let sharing = gtk::MenuButton::builder().icon_name("folder-symbolic").tooltip_text("Clipboard and Files").popover(&sharing_popover).css_classes(["flat", "rm-icon"]).build();

    let fullscreen = icon_button("view-fullscreen-symbolic", "Full Screen (F11)");
    let menu_model = gio::Menu::new();
    menu_model.append(Some("Preferences"), Some("win.preferences"));
    menu_model.append(Some("About remoteman"), Some("win.about"));
    let menu = gtk::MenuButton::builder().icon_name("open-menu-symbolic").tooltip_text("Main Menu").menu_model(&menu_model).primary(true).build();

    // Packed from the end inwards.
    header.pack_end(&menu);
    header.pack_end(&fullscreen);
    header.pack_end(&sharing);
    header.pack_end(&sound);
    header.pack_end(&microphone);
    header.pack_end(&camera);
    header.pack_end(&pointer);
    header.pack_end(&picture);
    header.pack_end(&display);
    header.pack_end(&send_keys);

    let figures = Figures::new();
    let overlay = gtk::Overlay::builder().child(&video.stack).build();
    overlay.add_overlay(&figures.root);
    overlay.add_css_class("rm-video");

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_top_bar_style(adw::ToolbarStyle::Raised);
    toolbar.set_content(Some(&overlay));

    View {
        toolbar,
        header,
        title,
        disconnect,
        fullscreen,
        menus: [send_keys, display, picture, menu, sharing],
        share_clipboard,
        send_file,
        placements: [(Placement::Extend, extra), (Placement::Main, main), (Placement::Only, only)],
        qualities: [(Quality::Automatic, automatic), (Quality::Sharp, sharp)],
        mirror,
        show_figures,
        sound,
        microphone,
        figures,
    }
}

impl View {
    /// A session has started: the bar says who with, and the menus what was asked for.
    /// `streams`: what Automatic and Sharp Text ask for first, in words; the second is empty
    /// if there is nothing sharper to ask for.
    pub fn begin(&self, name: &str, can_place: bool, placement: Placement, quality: Quality, streams: (String, String)) {
        self.title.set_title(name);
        self.title.set_subtitle("Connected");
        self.mirror.label.set_text(&format!("Mirror a Monitor of {name}"));
        for (p, item) in &self.placements {
            if let Some(mark) = &item.mark {
                mark.set_active(*p == placement);
            }
            // Only for computers that offer the choice.
            item.button.set_sensitive(can_place);
            item.button.set_tooltip_text((!can_place).then_some("This computer decides where the display goes"));
        }
        for (q, item) in &self.qualities {
            if let Some(mark) = &item.mark {
                mark.set_active(*q == quality);
            }
        }
        self.qualities[0].1.hint.set_text(&streams.0);
        self.qualities[1].1.hint.set_text(&streams.1);
        self.qualities[1].1.button.set_sensitive(!streams.1.is_empty());
    }

    /// The stream's size and rate, once known.
    /// What this computer shares: the lines of the sharing menu are there for what it does.
    /// `clipboard_on`: the clipboard is shared now.
    pub fn offer_sharing(&self, clipboard: bool, files: bool, clipboard_on: bool) {
        let no = "This computer doesn't allow it";
        if FEATURES.clipboard {
            self.share_clipboard.button.set_sensitive(clipboard);
            self.share_clipboard.button.set_tooltip_text((!clipboard).then_some(no));
        }
        if FEATURES.files {
            self.send_file.button.set_sensitive(files);
            self.send_file.button.set_tooltip_text((!files).then_some(no));
        }
        self.show_clipboard(clipboard && clipboard_on);
    }

    pub fn show_clipboard(&self, on: bool) {
        if let Some(mark) = &self.share_clipboard.mark {
            mark.set_active(on);
        }
    }

    /// Whether this computer takes a device's microphone: the button is there for those that do.
    pub fn offer_microphone(&self, offered: bool) {
        if FEATURES.microphone {
            self.microphone.set_sensitive(offered);
            self.microphone.set_tooltip_text(Some(if offered { "Microphone" } else { "This computer doesn't take a microphone" }));
        }
        if !offered {
            self.microphone.set_active(false);
        }
    }

    /// Whether this computer sends its sound: the button is there for those that do. A button
    /// left pressed in from the session before (the same computer, joined again) stays so.
    pub fn offer_sound(&self, offered: bool) {
        if FEATURES.sound {
            self.sound.set_sensitive(offered);
            self.sound.set_tooltip_text(Some(if offered { "Sound" } else { "This computer doesn't send its sound" }));
        }
        if !offered {
            self.sound.set_active(false);
        }
    }

    pub fn set_format(&self, format: Option<(u32, u32, u32)>) {
        if let Some((w, h, hz)) = format {
            self.title.set_subtitle(&format!("{w} × {h} at {hz} Hz"));
        }
    }

    /// One of the bar's menus is open.
    pub fn menu_open(&self) -> bool {
        self.menus.iter().any(|m| m.is_active())
    }

    pub fn set_fullscreen(&self, full: bool) {
        self.fullscreen.set_icon_name(if full { "view-restore-symbolic" } else { "view-fullscreen-symbolic" });
        self.fullscreen.set_tooltip_text(Some(if full { "Leave Full Screen (F11)" } else { "Full Screen (F11)" }));
        self.header.set_show_end_title_buttons(!full);
    }

    pub fn show_figures(&self, on: bool) {
        self.figures.root.set_visible(on);
        if let Some(mark) = &self.show_figures.mark {
            mark.set_active(on);
        }
    }

    /// Keep the figures clear of a bar that comes out over the picture.
    pub fn place_figures(&self, bar_hidden: bool) {
        self.figures.root.set_margin_top(if bar_hidden { 44 } else { 16 });
    }

    /// Open one of the bar's menus by name, closing any other (for pictures of the window).
    pub fn open_menu(&self, name: &str) {
        for menu in &self.menus {
            menu.popdown();
        }
        let index = ["keys", "display", "picture", "main", "sharing"].iter().position(|n| *n == name);
        if let Some(i) = index {
            self.menus[i].popup();
        }
    }
}

/// Press and let go of a few keys on the other computer, by Linux key code.
fn send_keys(app: &App, combo: &str) {
    let codes: &[u16] = match combo {
        "ctrl-alt-delete" => &[29, 56, 111],
        "super" => &[125],
        "alt-tab" => &[56, 15],
        "alt-f4" => &[56, 62],
        "print" => &[99],
        "f11" => &[87],
        _ => &[],
    };
    let usages: Vec<u16> = codes.iter().filter_map(|c| keys::linux_to_hid(*c)).collect();
    for usage in &usages {
        app.send(Event::Key { usage: *usage, down: true });
    }
    for usage in usages.iter().rev() {
        app.send(Event::Key { usage: *usage, down: false });
    }
}

pub fn wire(app: &Rc<App>) {
    let view = &app.view;
    view.disconnect.connect_clicked({
        let app = app.clone();
        move |_| app.stop(None)
    });
    view.fullscreen.connect_clicked({
        let app = app.clone();
        move |_| app.toggle_fullscreen()
    });
    for (placement, item) in &view.placements {
        let (app, placement) = (app.clone(), *placement);
        item.button.connect_clicked(move |_| {
            app.view.menus[1].popdown();
            let now = app.target.borrow().as_ref().map(|t| app.choices(&t.address).0);
            if now != Some(placement) {
                app.set_placement(placement);
            }
        });
    }
    for (quality, item) in &view.qualities {
        let (app, quality) = (app.clone(), *quality);
        item.button.connect_clicked(move |_| {
            app.view.menus[2].popdown();
            let now = app.target.borrow().as_ref().map(|t| app.choices(&t.address).1);
            if now != Some(quality) {
                app.set_quality(quality);
            }
        });
    }
    view.share_clipboard.button.connect_clicked({
        let app = app.clone();
        move |_| {
            app.view.menus[4].popdown();
            app.set_clipboard(!app.clipboard_on.get());
        }
    });
    view.send_file.button.connect_clicked({
        let app = app.clone();
        move |_| {
            app.view.menus[4].popdown();
            let dialog = gtk::FileDialog::builder().title("Send a File").modal(true).build();
            let app = app.clone();
            dialog.open(Some(&app.window.clone()), gio::Cancellable::NONE, move |chosen| {
                if let Some(path) = chosen.ok().and_then(|f| f.path()) {
                    app.send_file(&path);
                }
            });
        }
    });
    view.microphone.connect_toggled({
        let app = app.clone();
        move |button| {
            let on = button.is_active();
            let failed = match app.session.borrow().as_ref() {
                Some(session) => session.set_microphone(on, app.args.mic_from.as_deref()).err(),
                None => None,
            };
            if let Some(e) = failed {
                app.toast(&format!("The microphone can't be used: {e:#}"));
                button.set_active(false);
            }
        }
    });
    view.sound.connect_toggled({
        let app = app.clone();
        move |button| {
            let on = button.is_active();
            let failed = match app.session.borrow().as_ref() {
                Some(session) => session.set_sound(on, app.args.sound_to.as_deref()).err(),
                None => None,
            };
            if let Some(e) = failed {
                app.toast(&format!("The sound can't be played here: {e:#}"));
                button.set_active(false);
            }
        }
    });
    view.show_figures.button.connect_clicked({
        let app = app.clone();
        move |_| {
            app.view.menus[2].popdown();
            let on = app.store.borrow().settings.figures;
            app.set_figures(!on);
        }
    });
    for menu in &view.menus {
        let app = app.clone();
        menu.connect_active_notify(move |m| {
            // A hidden bar stays out while its menu is open; afterwards the keys are the
            // other computer's again.
            app.place_bar();
            if !m.is_active() && app.streaming() {
                // Once the menu has gone and given the focus back to its button.
                let app = app.clone();
                glib::idle_add_local_once(move || {
                    if app.forwarding() {
                        app.video.picture.grab_focus();
                    }
                });
            }
        });
    }
    let keys_action = gio::SimpleAction::new("send-keys", Some(glib::VariantTy::STRING));
    keys_action.connect_activate({
        let app = app.clone();
        move |_, combo| {
            if let Some(combo) = combo.and_then(|c| c.str()) {
                send_keys(&app, combo);
            }
        }
    });
    app.window.add_action(&keys_action);

    // A hidden bar comes out when the pointer reaches the top edge and goes when it has left
    // the bar.
    let edge = gtk::EventControllerMotion::new();
    edge.set_propagation_phase(gtk::PropagationPhase::Capture);
    edge.connect_motion({
        let app = app.clone();
        move |_, _, y| {
            let out = if app.bar_out.get() { y < 64.0 } else { y < 2.0 };
            if out != app.bar_out.get() {
                app.bar_out.set(out);
                app.place_bar();
            }
        }
    });
    edge.connect_leave({
        let app = app.clone();
        move |_| {
            if app.bar_out.replace(false) {
                app.place_bar();
            }
        }
    });
    app.window.add_controller(edge);

    // Keys: by physical position, to the other computer, before anything here sees them.
    let keys_ctl = gtk::EventControllerKey::new();
    keys_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys_ctl.connect_key_pressed({
        let app = app.clone();
        move |_, keyval, keycode, _| {
            if !app.forwarding() {
                return glib::Propagation::Proceed;
            }
            if keyval == gdk::Key::F11 {
                app.toggle_fullscreen();
            } else if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                app.held.borrow_mut().insert(usage);
                app.send(Event::Key { usage, down: true });
            }
            glib::Propagation::Stop
        }
    });
    keys_ctl.connect_key_released({
        let app = app.clone();
        move |_, keyval, keycode, _| {
            if app.streaming() && keyval != gdk::Key::F11 {
                if let Some(usage) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                    // Only what the other computer was told is down: a key let go in one of
                    // this app's own dialogs was never its business.
                    if app.held.borrow_mut().remove(&usage) {
                        app.send(Event::Key { usage, down: false });
                    }
                }
            }
        }
    });
    app.window.add_controller(keys_ctl);

    let stack = &app.video.stack;
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion({
        let app = app.clone();
        move |_, x, y| {
            let (x, y) = app.video.fraction(x, y);
            app.send(Event::PointerTo { x, y });
        }
    });
    stack.add_controller(motion);

    let click = gtk::GestureClick::builder().button(0).build();
    let button = |g: &gtk::GestureClick| match g.current_button() {
        2 => Button::Middle,
        3 => Button::Right,
        _ => Button::Left,
    };
    click.connect_pressed({
        let app = app.clone();
        move |g, _, x, y| {
            app.video.picture.grab_focus();
            let (x, y) = app.video.fraction(x, y);
            app.send(Event::PointerTo { x, y });
            app.send(Event::Button { button: button(g), down: true });
        }
    });
    click.connect_released({
        let app = app.clone();
        move |g, _, _, _| app.send(Event::Button { button: button(g), down: false })
    });
    stack.add_controller(click);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.connect_scroll({
        let app = app.clone();
        move |c, dx, dy| {
            // A wheel reports notches; a touchpad reports pixels.
            let k = if c.unit() == gdk::ScrollUnit::Wheel { 40.0 } else { 1.0 };
            app.send(Event::Scroll { dx: (dx * k) as f32, dy: (dy * k) as f32 });
            glib::Propagation::Stop
        }
    });
    stack.add_controller(scroll);
}
