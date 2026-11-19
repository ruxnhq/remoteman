//! The session's part of the window: the bar with its controls (the window's own title bar, or
//! one that floats over the picture when the title bar is hidden or the window is full
//! screen), the figures, and a line of news at the bottom. The picture itself is the draw
//! thread's; whatever of the window has nothing of this on it is the other computer's.

use crate::app::{App, MenuKind, Msg, Session};
use crate::features::{FEATURES, gate};
use crate::ui::icons::Icon;
use crate::ui::theme::{self, Color};
use crate::ui::{Align, En, Font, Id, Kind, ON, Op, Ui, id, off, rect};

/// The height of the bar, docked or floating.
pub const BAR: f32 = 44.0;

enum Piece {
    Logo,
    Name(String),
    /// A green dot and a few quiet words.
    Status(String),
    Sep,
    /// Takes whatever room is left.
    Flex,
    Button { id: Id, label: &'static str, icon: Option<Icon>, chevron: bool, bad: bool, on: bool, en: En, tip: &'static str, msg: Option<Msg>, menu: Option<MenuKind> },
}

fn width(ui: &Ui, piece: &Piece, labels: bool) -> f32 {
    match piece {
        Piece::Logo => 16.0,
        Piece::Name(name) => ui.fonts.width(name, Font::Strong).min(260.0) + 16.0,
        Piece::Status(words) => 14.0 + ui.fonts.width(words, Font::Caption) + 10.0,
        Piece::Sep => 9.0,
        Piece::Flex => 0.0,
        Piece::Button { label, icon, chevron, .. } => {
            let text = !label.is_empty() && (labels || icon.is_none());
            let mut w = 18.0;
            if icon.is_some() {
                w += 16.0;
            }
            if text {
                w += ui.fonts.width(label, Font::Body) + if icon.is_some() { 6.0 } else { 0.0 };
            }
            if *chevron && labels {
                w += 16.0;
            }
            w.max(36.0)
        }
    }
}

fn pieces(app: &App, s: &Session, floating: bool) -> Vec<Piece> {
    let open = |kind: &MenuKind| app.menu.as_ref().is_some_and(|m| m.kind == *kind);
    let menu = |name: &str, label: &'static str, icon: Icon, kind: MenuKind| Piece::Button { id: id(name), label, icon: Some(icon), chevron: true, bad: false, on: open(&kind), en: ON, tip: label, msg: None, menu: Some(kind) };
    let toggle = |name: &str, icon: Icon, on: bool, en: En, tip: &'static str, msg: Msg| Piece::Button { id: id(name), label: "", icon: Some(icon), chevron: false, bad: false, on, en, tip, msg: Some(msg), menu: None };
    let status = match app.format() {
        Some((w, h, hz)) if !floating => format!("{w} × {h} · {hz} Hz"),
        Some((_, _, hz)) => format!("{hz} Hz"),
        None => "Connected".into(),
    };
    let sound = if !FEATURES.sound {
        gate(false)
    } else if s.offers("sound") {
        ON
    } else {
        off("This computer doesn't send its sound")
    };
    let mut out = Vec::new();
    if !floating {
        out.push(Piece::Logo);
    }
    out.extend([
        Piece::Name(s.target.name.clone()),
        Piece::Status(status),
        Piece::Sep,
        Piece::Button { id: id("bar.disconnect"), label: "Disconnect", icon: None, chevron: false, bad: true, on: false, en: ON, tip: "", msg: Some(Msg::Disconnect), menu: None },
        Piece::Sep,
        menu("bar.keys", "Send keys", Icon::Keyboard, MenuKind::Keys),
        menu("bar.display", "Display", Icon::Computer, MenuKind::Display),
        menu("bar.picture", "Picture", Icon::Sliders, MenuKind::Picture),
        Piece::Sep,
        toggle("bar.sound", Icon::Sound, s.sound_on, sound, if s.sound_on { "Sound, on" } else { "Sound, off" }, Msg::ToggleSound),
        toggle("bar.mic", Icon::Microphone, false, gate(FEATURES.microphone), "Microphone", Msg::CloseMenu),
        toggle("bar.camera", Icon::Camera, false, gate(FEATURES.camera), "Camera", Msg::CloseMenu),
        Piece::Button { id: id("bar.share"), label: "", icon: Some(Icon::Folder), chevron: false, bad: false, on: open(&MenuKind::Share), en: ON, tip: "Clipboard and files", msg: None, menu: Some(MenuKind::Share) },
        if floating { Piece::Sep } else { Piece::Flex },
        if app.fullscreen {
            toggle("bar.full", Icon::LeaveFullScreen, false, ON, "Leave full screen (F11)", Msg::ToggleFullscreen)
        } else {
            toggle("bar.full", Icon::FullScreen, false, ON, "Full screen (F11)", Msg::ToggleFullscreen)
        },
        Piece::Button { id: id("bar.more"), label: "", icon: Some(Icon::More), chevron: false, bad: false, on: open(&MenuKind::More), en: ON, tip: "More", msg: None, menu: Some(MenuKind::More) },
    ]);
    out
}

/// Lay the pieces out from `x` in a bar whose top is `y`, in `room`; with no room for the
/// words, the buttons that have an icon lose them, and with too little even then, the
/// stream's size goes as well.
fn lay(ui: &mut Ui, mut list: Vec<Piece>, x: f32, y: f32, room: f32) {
    let t = ui.t;
    let total = |ui: &Ui, list: &[Piece], labels: bool| list.iter().map(|p| width(ui, p, labels) + 2.0).sum::<f32>();
    let labels = total(ui, &list, true) <= room;
    if !labels && total(ui, &list, false) > room {
        list.retain(|p| !matches!(p, Piece::Status(_)));
    }
    let used = total(ui, &list, labels);
    let mut x = x;
    for piece in list {
        let w = width(ui, &piece, labels);
        match piece {
            Piece::Logo => ui.ops.push(Op::Logo { x, y: y + 14.0, size: 16.0 }),
            Piece::Name(name) => ui.label_in(rect(x + 8.0, y, w - 8.0, BAR), &name, Font::Strong, t.tx, Align::Left),
            Piece::Status(words) => {
                ui.dot(x + 4.0, y + BAR / 2.0, 4.0, t.ok);
                ui.label_in(rect(x + 14.0, y, w - 14.0, BAR), &words, Font::Caption, t.t2, Align::Left);
            }
            Piece::Sep => ui.fill(rect(x + 4.0, y + 12.0, 1.0, 20.0), 0.0, t.line),
            Piece::Flex => x += (room - used).max(0.0),
            Piece::Button { id: bid, label, icon, chevron, bad, on, en, tip, msg, menu } => {
                let r = rect(x, y + 6.0, w, 32.0);
                let msg = menu.map(|kind| Msg::OpenMenu(kind, r)).or(msg);
                let text = !label.is_empty() && (labels || icon.is_none());
                let state = ui.region(bid, r, Kind::Button, msg, en, if text { "" } else { tip }, true);
                if on || state.hot {
                    ui.fill(r, 4.0, if state.down { t.sub.fade(1.7) } else { t.sub });
                }
                let ink = if bad { t.bad } else if on { t.at } else { t.tx };
                let ink = if en.on { ink } else { ink.fade(0.4) };
                let mut at = x + 9.0;
                if let Some(icon) = icon {
                    ui.icon(icon, at, y + 14.0, ink);
                    at += 22.0;
                }
                if text {
                    let lw = ui.fonts.width(label, Font::Body);
                    ui.label_in(rect(at, y + 6.0, lw + 2.0, 32.0), label, Font::Body, ink, Align::Left);
                    at += lw + 4.0;
                }
                if chevron && labels {
                    ui.ops.push(Op::Icon { icon: Icon::ChevronDown, x: at, y: y + 16.0, size: 12.0, color: ink.fade(0.8), stroke: 1.5 });
                }
                ui.focus_ring(r, 4.0, state);
            }
        }
        x += w + 2.0;
    }
}

/// The figures: what the stream is doing, over the picture's top right corner.
fn figures(app: &App, ui: &mut Ui, top: f32) {
    let f = &app.figures;
    let or = |v: f64, unit: &str| if v == 0.0 { "–".to_owned() } else { format!("{v:.1} {unit}") };
    let cells = [
        ("FRAMES PER SECOND", f.shown.to_string()),
        ("DECODED PER SECOND", f.decoded.to_string()),
        ("PICTURE AGE", or(f.age_ms, "ms")),
        ("ROUND TRIP", or(f.round_trip_ms, "ms")),
        ("BITRATE", format!("{:.1} Mb/s", f.megabits)),
        ("LOST FRAMES", f.lost.to_string()),
        ("REPLACED UNSEEN", f.replaced.to_string()),
        ("DECODE TIME, AT MOST", or(f.decode_ms, "ms")),
    ];
    let rows = cells.len().div_ceil(2) + 1;
    let w = 288.0;
    let r = rect(ui.w - 16.0 - w, top + 16.0, w, 28.0 + rows as f32 * 34.0 + (rows - 1) as f32 * 12.0);
    ui.fill(r, 8.0, theme::FIGURES_FILL);
    ui.stroke(r, 8.0, theme::FIGURES_LINE);
    let column = (w - 32.0 - 16.0) / 2.0;
    let cell = |ui: &mut Ui, x: f32, y: f32, w: f32, key: &str, value: &str| {
        ui.label(x, y, key, Font::Key, theme::FIGURES_KEY);
        ui.label_in(rect(x, y + 14.0, w, 20.0), value, Font::Value, Color::WHITE, Align::Left);
    };
    for (i, (key, value)) in cells.iter().enumerate() {
        cell(ui, r.x + 16.0 + (i % 2) as f32 * (column + 16.0), r.y + 14.0 + (i / 2) as f32 * 46.0, column, key, value);
    }
    let decoder = if f.decoder.is_empty() { "–" } else { &f.decoder };
    cell(ui, r.x + 16.0, r.y + 14.0 + (rows - 1) as f32 * 46.0, w - 32.0, "DECODER", decoder);
}

/// A line of news (a file sent, sound that can't be played) at the bottom of the picture.
pub fn toast(ui: &mut Ui, text: &str) {
    let w = (ui.fonts.width(text, Font::Caption) + 28.0).min(ui.w - 32.0);
    let r = rect(((ui.w - w) / 2.0).round(), ui.h - 56.0, w, 32.0);
    ui.fill(r, 6.0, theme::FIGURES_FILL);
    ui.stroke(r, 6.0, theme::FIGURES_LINE);
    ui.label_in(r.inset(8.0), text, Font::Caption, Color::WHITE, Align::Center);
}

pub fn view(app: &App, ui: &mut Ui) {
    let Some(s) = &app.session else { return };
    let floating = app.bar_hidden();
    let out = floating && (app.bar_slide > 0.0 || app.menu.is_some());
    if app.store.settings.figures {
        figures(app, ui, if floating { if out { 54.0 } else { 0.0 } } else { BAR });
    }
    if floating {
        if out {
            // Dark whatever the system says: what is under it is somebody's desktop.
            ui.t = theme::OVER;
            let slide = if app.menu.is_some() { 1.0 } else { app.bar_slide };
            let list = pieces(app, s, true);
            let labels: f32 = list.iter().map(|p| width(ui, p, true) + 2.0).sum();
            let w = (labels + 12.0).min(ui.w - 16.0);
            let r = rect(((ui.w - w) / 2.0).round(), (-54.0 + 64.0 * slide).round(), w, BAR);
            ui.ops.push(Op::Shadow { r, radius: 8.0, strength: theme::OVER.shadow });
            ui.fill(r, 8.0, theme::OVER_BAR);
            ui.stroke(r, 8.0, theme::OVER.line);
            ui.region(id("bar"), r, Kind::Block, None, ON, "", false);
            lay(ui, list, r.x + 6.0, r.y, w - 12.0);
        }
    } else {
        let (w, t) = (ui.w, ui.t);
        ui.fill(rect(0.0, 0.0, w, BAR), 0.0, t.bg);
        ui.region(id("title"), rect(0.0, 0.0, w, BAR), Kind::Drag, None, ON, "", false);
        let list = pieces(app, s, false);
        lay(ui, list, 12.0, 0.0, w - 12.0 - 142.0);
        crate::pages::caption_buttons(app, ui, BAR);
    }
    if let Some(menu) = &app.menu {
        crate::menus::view(app, ui, menu);
    }
}

