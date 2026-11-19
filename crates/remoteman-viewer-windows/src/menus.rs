//! The flyouts: the menus of the session's bar, a computer's "more", and the lists behind the
//! drop-downs. One is open at a time, over everything else, and a press anywhere outside it
//! closes it.

use remoteman_core::Placement;

use crate::app::{App, BITRATES, Flag, Menu, MenuKind, Msg};
use crate::features::{FEATURES, gate};
use crate::store::{PLACEMENT_NAMES, Quality, SIZES};
use crate::ui::icons::Icon;
use crate::ui::{Align, En, Font, Id, Kind, ON, Op, Rect, Ui, id, idn, off, rect};

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    None,
    /// One of several, chosen or not.
    Radio(bool),
    /// The chosen entry of a drop-down's list.
    Tick(bool),
}

struct Item {
    id: Id,
    label: String,
    caption: String,
    /// Quiet words at the end of the line: a shortcut, or what the line stands at.
    hint: String,
    mark: Mark,
    /// A switch at the end of the line, on or off.
    toggle: Option<bool>,
    /// An arrow at the end: the line leads to more.
    arrow: bool,
    bad: bool,
    msg: Option<Msg>,
    en: En,
}

enum Line {
    Item(Item),
    Rule,
    Heading(&'static str),
    /// A slider with words under each end.
    Slider { id: Id, part: f32, left: String, right: String, en: En },
}

fn item(name: &str, label: &str, msg: Msg) -> Item {
    Item { id: id(name), label: label.to_owned(), caption: String::new(), hint: String::new(), mark: Mark::None, toggle: None, arrow: false, bad: false, msg: Some(msg), en: ON }
}

impl Item {
    fn caption(mut self, words: &str) -> Self {
        self.caption = words.to_owned();
        self
    }

    fn hint(mut self, words: &str) -> Self {
        self.hint = words.to_owned();
        self
    }

    fn radio(mut self, on: bool) -> Self {
        self.mark = Mark::Radio(on);
        self
    }

    fn toggle(mut self, on: bool) -> Self {
        self.toggle = Some(on);
        self
    }

    fn en(mut self, en: En) -> Self {
        self.en = en;
        self
    }

    fn line(self) -> Line {
        Line::Item(self)
    }
}

const SLIDER: f32 = 52.0;

fn height(ui: &Ui, line: &Line, width: f32) -> f32 {
    match line {
        Line::Item(i) => {
            let text = text_width(ui, i, width);
            let caption = if i.caption.is_empty() { 0.0 } else { ui.fonts.height(&i.caption, Font::Caption, text) };
            (16.0 + ui.fonts.height(&i.label, Font::Body, text) + caption).max(36.0)
        }
        Line::Rule => 9.0,
        Line::Heading(_) => 26.0,
        Line::Slider { .. } => SLIDER,
    }
}

/// How wide the words of an item may be, between its mark and what is at its end.
fn text_width(ui: &Ui, i: &Item, width: f32) -> f32 {
    let before = if i.mark == Mark::None || matches!(i.mark, Mark::Tick(_)) { 0.0 } else { 28.0 };
    let mut after = 0.0;
    if !i.hint.is_empty() {
        after += ui.fonts.width(&i.hint, Font::Caption) + 12.0;
    }
    if i.toggle.is_some() {
        after += 52.0;
    }
    if i.arrow || matches!(i.mark, Mark::Tick(_)) {
        after += 28.0;
    }
    (width - 8.0 - 24.0 - before - after).max(40.0)
}

/// Draw a flyout `width` wide hanging from `anchor`.
fn flyout(ui: &mut Ui, anchor: Rect, width: f32, lines: Vec<Line>) {
    let t = ui.t;
    let total: f32 = lines.iter().map(|l| height(ui, l, width)).sum::<f32>() + 8.0;
    // Under the control it hangs from, moved as little as it must be to stay in the window.
    let x = anchor.x.min(ui.w - width - 8.0).max(8.0);
    let y = (anchor.bottom() + 4.0).min(ui.h - total - 8.0).max(4.0);
    let r = rect(x, y, width, total);
    ui.raise();
    let whole = ui.whole();
    ui.region(id("menu.behind"), whole, Kind::Block, Some(Msg::CloseMenu), ON, "", false);
    ui.panel(r, 8.0, t.pane);
    ui.region(id("menu"), r, Kind::Block, None, ON, "", false);
    let mut y = r.y + 4.0;
    for line in lines {
        let h = height(ui, &line, width);
        let row = rect(r.x + 4.0, y, width - 8.0, h);
        match line {
            Line::Rule => ui.hline(r.x + 4.0, y + 4.0, width - 8.0),
            Line::Heading(words) => {
                ui.label(row.x + 12.0, y + 8.0, words, Font::CaptionStrong, t.t2);
            }
            Line::Slider { id: sid, part, left, right, en } => {
                let track = rect(row.x + 12.0, y + 14.0, row.w - 24.0, 4.0);
                let dim = if en.on { 1.0 } else { 0.4 };
                ui.region(sid, rect(track.x, y + 4.0, track.w, 24.0), Kind::Slider, None, en, "", false);
                ui.fill(track, 2.0, t.line);
                ui.fill(rect(track.x, track.y, track.w * part, 4.0), 2.0, t.ac.fade(dim));
                let (cx, cy) = (track.x + track.w * part, track.y + 2.0);
                ui.dot(cx, cy, 10.0, t.card);
                ui.ops.push(Op::Dot { cx, cy, r: 10.0, color: if en.on { t.ac } else { t.t2 }, ring: 5.0 });
                ui.label(row.x + 12.0, y + 30.0, &left, Font::Caption, t.t2.fade(dim));
                ui.label_in(rect(row.x + 12.0, y + 30.0, row.w - 24.0, 16.0), &right, Font::Caption, t.t2.fade(dim), Align::Right);
            }
            Line::Item(i) => {
                let pressable = i.msg.is_some();
                let state = ui.region(i.id, row, if pressable { Kind::Button } else { Kind::Block }, i.msg.clone(), i.en, "", pressable);
                let chosen = matches!(i.mark, Mark::Radio(true) | Mark::Tick(true));
                if state.down {
                    ui.fill(row, 4.0, t.sub.fade(1.7));
                } else if (state.hot && pressable) || chosen {
                    ui.fill(row, 4.0, t.sub);
                }
                let dim = if i.en.on { 1.0 } else { 0.4 };
                let text = text_width(ui, &i, width);
                let label_h = ui.fonts.height(&i.label, Font::Body, text);
                let caption_h = if i.caption.is_empty() { 0.0 } else { ui.fonts.height(&i.caption, Font::Caption, text) };
                let top = y + ((h - label_h - caption_h) / 2.0).round();
                let mut x = row.x + 12.0;
                if let Mark::Radio(on) = i.mark {
                    ui.radio(x, y + (h - 16.0) / 2.0, on, dim);
                    x += 28.0;
                }
                let ink = if i.bad { t.bad } else { t.tx };
                ui.para(x, top, text, &i.label, Font::Body, ink.fade(dim), Align::Left);
                if !i.caption.is_empty() {
                    ui.para(x, top + label_h, text, &i.caption, Font::Caption, t.t2.fade(dim), Align::Left);
                }
                let mut end = row.right() - 12.0;
                if let Some(on) = i.toggle {
                    // Only the picture of a switch: the whole line is what is pressed.
                    let (sx, sy) = (end - 40.0, y + (h - 20.0) / 2.0);
                    if on {
                        ui.fill(rect(sx, sy, 40.0, 20.0), 10.0, t.ac.fade(dim));
                        ui.dot(sx + 30.0, sy + 10.0, 6.0, t.on);
                    } else {
                        ui.stroke(rect(sx, sy, 40.0, 20.0), 10.0, t.t2.fade(dim));
                        ui.dot(sx + 10.0, sy + 10.0, 6.0, t.t2.fade(dim));
                    }
                    end -= 52.0;
                }
                if i.arrow {
                    ui.icon(Icon::ChevronRight, end - 16.0, y + (h - 16.0) / 2.0, t.tx.fade(dim));
                    end -= 28.0;
                }
                if i.mark == Mark::Tick(true) {
                    ui.icon(Icon::Check, end - 16.0, y + (h - 16.0) / 2.0, t.at);
                }
                if !i.hint.is_empty() {
                    let hint_w = ui.fonts.width(&i.hint, Font::Caption);
                    ui.label(end - hint_w, y + (h - 16.0) / 2.0, &i.hint, Font::Caption, t.t2.fade(dim));
                }
                ui.focus_ring(row.inset(2.0), 4.0, state);
            }
        }
        y += h;
    }
}

pub fn view(app: &App, ui: &mut Ui, menu: &Menu) {
    let name = app.session.as_ref().map(|s| s.target.name.clone()).unwrap_or_default();
    let (width, lines) = match &menu.kind {
        MenuKind::Card(address) => {
            let mut forget = item("card.forget", "Forget…", Msg::AskForget(address.clone()));
            forget.bad = true;
            (240.0, vec![item("card.settings", "Settings for this computer…", Msg::OpenComputer(address.clone())).line(), forget.line()])
        }
        MenuKind::Keys => {
            // The chords this PC or Windows would keep for itself, as Linux key codes: each is
            // pressed in order and let go in reverse.
            let chords: [(&str, &str, &'static [u16]); 7] = [
                ("Ctrl+Alt+Delete", "Windows can't pass this one on", &[29, 56, 111]),
                ("Win", "Super on Linux, Command on a Mac", &[125]),
                ("Win+L", "Locks the other computer", &[125, 38]),
                ("Alt+Tab", "", &[56, 15]),
                ("Alt+F4", "", &[56, 62]),
                ("Ctrl+Shift+Esc", "", &[29, 42, 1]),
                ("Print Screen", "", &[99]),
            ];
            let mut lines: Vec<Line> = chords.iter().enumerate().map(|(n, (label, hint, codes))| Item { id: idn("keys", n), ..item("", label, Msg::SendKeys(codes)).hint(hint) }.line()).collect();
            lines.push(Line::Rule);
            let shortcuts = item("keys.shortcuts", "Send system shortcuts to the computer", Msg::Toggle(Flag::Shortcuts, None))
                .caption("Alt+Tab, the Windows key and others go to it while this window is in front")
                .toggle(FEATURES.system_shortcuts && app.store.settings.shortcuts)
                .en(gate(FEATURES.system_shortcuts));
            lines.push(shortcuts.line());
            let mut release = item("keys.release", "Release the keyboard", Msg::CloseMenu).caption("Gives shortcuts back to this PC").hint("Ctrl+Alt+Shift").en(gate(FEATURES.release_shortcut));
            release.msg = None;
            lines.push(release.line());
            (348.0, lines)
        }
        MenuKind::Display => {
            let s = app.session.as_ref();
            let now = s.map_or(Placement::Extend, |s| s.display.placement);
            // Only for computers that offer the choice.
            let can = if s.is_some_and(|s| s.offers("placement")) { ON } else { off("This computer decides where the display goes") };
            let mirrors = if s.is_some_and(|s| s.offers("mirror")) { can } else { off("This computer can't show its own monitor") };
            let hz = app.format().map_or(app.monitor.2, |f| f.2);
            let address = s.map(|s| s.target.address.clone()).unwrap_or_default();
            let size = app.store.find(&address).map_or(0, |c| c.max_height);
            let size = SIZES.iter().find(|x| x.0 == size).map_or("", |x| x.1);
            let mut lines = vec![Line::Heading("Where this display sits")];
            for (n, (p, label, words)) in PLACEMENT_NAMES.iter().enumerate() {
                let en = if *p != Placement::Mirror { can } else if FEATURES.mirror { mirrors } else { gate(false) };
                let label = if *p == Placement::Mirror && !name.is_empty() { format!("Mirror the main monitor of {name}") } else { (*label).to_owned() };
                lines.push(Item { id: idn("display.place", n), ..item("", &label, Msg::Place(*p)) }.caption(words).radio(now == *p).en(en).line());
            }
            let mut rate = item("display.rate", "Refresh rate", Msg::CloseMenu).hint(&format!("{hz} Hz"));
            rate.msg = None;
            let mut picture_size = item("display.size", "Picture size", Msg::OpenComputer(address)).hint(size).en(gate(FEATURES.follow_window));
            picture_size.arrow = true;
            lines.extend([
                Line::Rule,
                Line::Heading("Size and rate"),
                picture_size.line(),
                rate.line(),
                item("display.hdr", "HDR", Msg::CloseMenu).caption("When the computer offers it and this monitor is in HDR").toggle(false).en(gate(FEATURES.hdr)).line(),
                item("display.vrr", "Variable refresh rate", Msg::CloseMenu).caption("Shows each frame when it arrives, in full screen").toggle(false).en(gate(FEATURES.variable_refresh)).line(),
                Line::Rule,
                item("display.second", if s.is_some_and(|s| s.second_on) { "Remove the second display" } else { "Add a second display" }, Msg::ToggleSecond)
                    .caption("In a window of its own")
                    .en(if !FEATURES.second_display { gate(false) } else if s.is_some_and(|s| s.offers("displays")) { ON } else { off("This computer gives a device one display") })
                    .line(),
            ]);
            (320.0, lines)
        }
        MenuKind::Share => {
            let s = app.session.as_ref();
            let no = off("This computer doesn't allow it");
            let offers = |what: &str| s.is_some_and(|s| s.offers(what));
            let clipboard = if !FEATURES.clipboard { gate(false) } else if offers("clipboard") { ON } else { no };
            let files = if !FEATURES.files { gate(false) } else if offers("files") { ON } else { no };
            (
                300.0,
                vec![
                    Line::Heading("Clipboard and files"),
                    item("share.clipboard", "Share the clipboard", Msg::ToggleClipboard).caption("Text copied on either side can be pasted on the other").toggle(s.is_some_and(|s| s.clipboard_on)).en(clipboard).line(),
                    item("share.file", "Send a file…", Msg::SendFile).caption("The computer decides where it goes, and says where").en(files).line(),
                ],
            )
        }
        MenuKind::Picture => {
            let s = app.session.as_ref();
            let now = s.map_or(Quality::Automatic, |s| s.quality);
            let decodes = |q: Quality| q.order(&app.decoders).first().is_some_and(|m| (q == Quality::Hevc) == (m.codec == remoteman_core::Codec::Hevc));
            let no = off("This PC's graphics card doesn't decode it");
            let pick = |n: &str, q: Quality, en: En| item(n, q.name(), Msg::Pick(q)).radio(now == q).en(en).line();
            let limit = s.map_or(0, |s| s.bitrate);
            let part = BITRATES.iter().position(|b| *b == limit).unwrap_or(BITRATES.len() - 1) as f32 / (BITRATES.len() - 1) as f32;
            let can = if s.is_some_and(|s| s.offers("configure")) { ON } else { off("This computer sets the bitrate itself") };
            (
                300.0,
                vec![
                    Line::Heading("Picture quality"),
                    item("picture.auto", "Automatic", Msg::Pick(Quality::Automatic)).caption("Picks the best stream this PC decodes on its graphics card").radio(now == Quality::Automatic).line(),
                    item("picture.sharp", "Sharp text (4:4:4)", Msg::Pick(Quality::Sharp))
                        .caption("Full colour detail for small text, at a higher bitrate")
                        .radio(now == Quality::Sharp)
                        .en(gate(FEATURES.sharp_text))
                        .line(),
                    pick("picture.hevc", Quality::Hevc, if decodes(Quality::Hevc) { ON } else { no }),
                    pick("picture.av1", Quality::Av1, if decodes(Quality::Av1) { ON } else { no }),
                    Line::Rule,
                    Line::Heading("Bitrate limit"),
                    Line::Slider {
                        id: id("picture.bitrate"),
                        part,
                        left: if limit == 0 { "No limit".into() } else { format!("Up to {limit} Mb/s") },
                        right: format!("Now {:.0} Mb/s", app.figures.megabits),
                        en: can,
                    },
                    Line::Rule,
                    item("picture.figures", "Show figures", Msg::ToggleFigures).hint("Ctrl+Alt+Shift+I").toggle(app.store.settings.figures).line(),
                ],
            )
        }
        MenuKind::More => {
            let address = app.session.as_ref().map(|s| s.target.address.clone()).unwrap_or_default();
            let hover = app.store.settings.hover_bar;
            (
                260.0,
                vec![
                    item("more.computer", "Settings for this computer…", Msg::OpenComputer(address)).line(),
                    item("more.settings", "Settings", Msg::OpenSettings).line(),
                    Line::Rule,
                    Line::Heading("Session window"),
                    item("more.title", "Title bar", Msg::BarMode(false)).radio(!hover).line(),
                    item("more.hover", "Show the bar on hover", Msg::BarMode(true)).radio(hover).line(),
                    Line::Rule,
                    item("more.about", "About remoteman", Msg::About).line(),
                ],
            )
        }
        MenuKind::Choice(choice) => {
            let (options, at) = app.options(choice);
            let widest = options.iter().map(|o| ui.fonts.width(&o.0, Font::Body)).fold(0.0, f32::max);
            let lines = options
                .into_iter()
                .enumerate()
                .map(|(n, (label, on))| Item { id: idn("choice", n), mark: Mark::Tick(n == at), ..item("", &label, Msg::Choose(choice.clone(), n)) }.en(gate(on)).line())
                .collect();
            ((widest + 68.0).max(menu.anchor.w), lines)
        }
    };
    flyout(ui, menu.anchor, width, lines);
}
