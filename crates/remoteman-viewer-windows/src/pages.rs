//! The screens before and beside a session, as the design draws them: the computers, the wait
//! for one to answer and the ways that goes wrong, the settings, the settings of one computer,
//! and the dialogs.

use crate::app::{App, Choice, Dialog, Flag, MenuKind, Msg, Screen, Target};
use crate::features::{FEATURES, NOT_YET, gate};
use crate::status::{Problem, Reach};
use crate::store::{PLACEMENT_NAMES, os_name, short, when};
use crate::ui::icons::Icon;
use crate::ui::theme::{Color, rgb, rgba};
use crate::ui::{Align, En, Font, Id, Kind, Look, ON, Op, Rect, Ui, id, idn, off, rect};

/// The height of the window's own bar on these screens.
pub const TITLE: f32 = 40.0;

pub fn view(app: &App, ui: &mut Ui) {
    // (On Mica the window's backdrop is the page's background.)
    if !app.mica {
        let (whole, bg) = (ui.whole(), ui.t.bg);
        ui.fill(whole, 0.0, bg);
    }
    match &app.screen {
        Screen::Connecting => connecting(app, ui),
        Screen::Problem(target, problem) => trouble(app, ui, target, problem),
        Screen::Settings => settings(app, ui),
        Screen::Computer(address) => computer(app, ui, address),
        Screen::Computers | Screen::Session => computers(app, ui),
    }
    if let Some(menu) = &app.menu {
        crate::menus::view(app, ui, menu);
    }
}

/// The window's bar: the product's mark (or the way back) and name, and the three buttons.
fn titlebar(app: &App, ui: &mut Ui, back: bool) {
    let (w, t) = (ui.w, ui.t);
    ui.region(id("title"), rect(0.0, 0.0, w, TITLE), Kind::Drag, None, ON, "", false);
    if back {
        let r = rect(4.0, 4.0, 36.0, 32.0);
        let state = ui.region(id("title.back"), r, Kind::Button, Some(Msg::Back), ON, "Back", true);
        if state.hot {
            ui.fill(r, 4.0, if state.down { t.sub.fade(1.7) } else { t.sub });
        }
        ui.icon(Icon::Back, 14.0, 12.0, t.tx);
        ui.focus_ring(r, 4.0, state);
    } else {
        ui.ops.push(Op::Logo { x: 14.0, y: 12.0, size: 16.0 });
    }
    ui.label(40.0, 12.0, "remoteman", Font::Caption, t.tx);
    caption_buttons(app, ui, TITLE);
}

/// Minimise, maximise and close, at the right end of a bar `h` tall.
pub fn caption_buttons(app: &App, ui: &mut Ui, h: f32) {
    let (w, t) = (ui.w, ui.t);
    let buttons = [
        ("caption.min", Icon::Minimise, Msg::Minimise, Kind::Button),
        ("caption.max", if app.maximised { Icon::Restore } else { Icon::Maximise }, Msg::Maximise, Kind::Maximise),
        ("caption.close", Icon::Close, Msg::Close, Kind::Button),
    ];
    for (i, (name, icon, msg, kind)) in buttons.into_iter().enumerate() {
        let r = rect(w - 138.0 + 46.0 * i as f32, 0.0, 46.0, h);
        let close = name == "caption.close";
        let state = ui.region(id(name), r, kind, Some(msg), ON, "", false);
        let mut ink = t.tx;
        if state.hot {
            // Windows' own red for the close button under the pointer.
            let fill = if close { rgb(0xC42B1C).fade(if state.down { 0.85 } else { 1.0 }) } else if state.down { t.sub.fade(1.7) } else { t.sub };
            ui.fill(r, 0.0, fill);
            if close {
                ink = Color::WHITE;
            }
        }
        ui.icon(icon, r.x + 15.0, (h - 16.0) / 2.0, ink);
    }
}

/// The thumb of a page that is taller than the window, and how far it can be scrolled.
fn scrolled(app: &App, ui: &mut Ui, area: Rect, content: f32) {
    let room = (content - area.h).max(0.0);
    app.scroll_room.set(room);
    if room > 0.0 {
        let thumb = (area.h * area.h / content).max(32.0);
        let at = area.y + (area.h - thumb) * (app.scroll.min(room) / room);
        let color = ui.t.t2.fade(0.55);
        ui.fill(rect(area.right() - 9.0, at + 2.0, 3.0, thumb - 4.0), 1.5, color);
    }
}

// The computers.

fn thumb_colour(os: &str) -> Color {
    match os_name(os).as_str() {
        "Linux" => rgb(0x1E2A44),
        "Mac" => rgb(0x3A3550),
        "Windows" => rgb(0x1F3A4D),
        _ => rgb(0x2B3340),
    }
}

/// The line under a computer's name: what is known of it, and when it was last used.
fn state_line(app: &App, address: &str) -> (Option<Reach>, String) {
    let Some(c) = app.store.find(address) else { return (None, String::new()) };
    let used = if c.used > 0 {
        format!("used {}", when(c.used))
    } else if c.paired > 0 {
        format!("paired {}", when(c.paired))
    } else {
        String::new()
    };
    let reach = app.reach_of(address);
    let state = match reach {
        Some(Reach::Online) => "Online",
        Some(Reach::Asleep) => "Asleep",
        Some(Reach::Unreachable) => "Not reachable",
        None => "",
    };
    let line = match (state.is_empty(), used.is_empty()) {
        (false, false) => format!("{state} · {used}"),
        (false, true) => state.to_owned(),
        // Nothing can tell that a computer is online before it is tried: only when it was used.
        (true, _) => {
            let mut c = used.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    };
    (reach, line)
}

fn computers(app: &App, ui: &mut Ui) {
    let (w, h, t) = (ui.w, ui.h, ui.t);
    titlebar(app, ui, false);
    let (left, right) = (32.0, w - 32.0);
    ui.label(left, 52.0, "Computers", Font::Title, t.tx);
    let settings = rect(right - 32.0, 54.0, 32.0, 32.0);
    ui.icon_button(id("list.settings"), settings, Icon::Sliders, "Settings", Msg::OpenSettings, ON);
    let pair = ui.button_width("Pair a computer", Look::Accent, true);
    ui.button(id("list.pair"), rect(settings.x - 8.0 - pair, 54.0, pair, 32.0), "Pair a computer", Look::Accent, Some(Icon::Plus), Msg::Pair(None), ON);
    if app.store.computers.is_empty() {
        app.scroll_room.set(0.0);
        return status(
            ui,
            rect(0.0, 88.0, w, h - 88.0),
            Sign::Logo,
            "No computers yet",
            "See and use another computer's screen from this one. Pair a Linux PC, a Mac or another Windows PC that runs the remoteman host.",
            None,
            &[(id("empty.pair"), "Pair a computer", Look::Accent, Msg::Pair(None), ON)],
        );
    }

    let area = rect(0.0, 100.0, w, h - 100.0);
    ui.clip(area);
    let top = 108.0 - app.scroll.min(app.scroll_room.get());
    let mut y = top;
    ui.label(left, y, "Paired computers", Font::Strong, t.tx);
    y += 28.0;
    let room = right - left;
    let columns = (((room + 12.0) / 272.0).floor() as usize).clamp(1, 4);
    let card_w = ((room - 12.0 * (columns - 1) as f32) / columns as f32).floor();
    for (i, c) in app.store.computers.iter().enumerate() {
        let (column, row) = (i % columns, i / columns);
        let r = rect(left + column as f32 * (card_w + 12.0), y + row as f32 * 240.0, card_w, 228.0);
        ui.card(r);
        let (reach, line) = state_line(app, &c.address);
        let thumb = rect(r.x + 12.0, r.y + 12.0, r.w - 24.0, 112.0);
        // The design's place for a picture of the computer's screen; nothing takes one yet,
        // so it is a plain colour by what the computer runs.
        ui.fill(thumb, 4.0, if reach == Some(Reach::Unreachable) { t.sub } else { thumb_colour(&c.os) });
        let os = os_name(&c.os);
        if !os.is_empty() {
            let chip = rect(thumb.x + 10.0, thumb.bottom() - 30.0, ui.fonts.width(&os, Font::Caption) + 16.0, 20.0);
            ui.fill(chip, 4.0, rgba(0x000000, if reach == Some(Reach::Unreachable) { 0.6 } else { 0.45 }));
            ui.label_in(chip, &os, Font::Caption, Color::WHITE, Align::Center);
        }
        ui.label_in(rect(r.x + 12.0, r.y + 136.0, r.w - 24.0, 20.0), &c.name, Font::Strong, t.tx, Align::Left);
        let mut x = r.x + 12.0;
        match reach {
            Some(Reach::Online) => ui.dot(x + 4.0, r.y + 164.0, 4.0, t.ok),
            Some(Reach::Asleep) => ui.fill(rect(x, r.y + 160.0, 8.0, 8.0), 0.0, t.warn),
            Some(Reach::Unreachable) => ui.ops.push(Op::Dot { cx: x + 4.0, cy: r.y + 164.0, r: 4.0, color: t.t2, ring: 1.5 }),
            None => x -= 14.0,
        }
        ui.label_in(rect(x + 14.0, r.y + 156.0, r.right() - 12.0 - x - 14.0, 16.0), &line, Font::Caption, t.t2, Align::Left);
        let go = rect(r.x + 12.0, r.y + 184.0, r.w - 64.0, 32.0);
        if reach == Some(Reach::Asleep) {
            ui.button(idn("card.wake", i), go, "Wake and connect", Look::Plain, None, Msg::Connect(c.address.clone()), gate(FEATURES.wake));
        } else {
            let look = if reach == Some(Reach::Unreachable) { Look::Plain } else { Look::Accent };
            ui.button(idn("card.connect", i), go, "Connect", look, None, Msg::Connect(c.address.clone()), ON);
        }
        let more = rect(r.right() - 44.0, r.y + 184.0, 32.0, 32.0);
        ui.icon_button(idn("card.more", i), more, Icon::More, &format!("More for {}", c.name), Msg::OpenMenu(MenuKind::Card(c.address.clone()), more), ON);
    }
    let rows = app.store.computers.len().div_ceil(columns);
    y += rows as f32 * 240.0 - 12.0 + 20.0;

    ui.label(left, y, "Nearby", Font::Strong, t.tx);
    y += 28.0;
    let near = rect(left, y, room, 60.0);
    ui.card(near);
    if !FEATURES.discovery {
        // Nothing looks for them yet, and nothing is made up: say so where they will be listed.
        ui.region(id("list.nearby"), near, Kind::Block, None, off(NOT_YET), "", false);
        ui.icon(Icon::Computer, near.x + 16.0, near.y + 22.0, t.tx.fade(0.4));
        ui.label(near.x + 46.0, near.y + 12.0, "Looking for computers on this network", Font::Body, t.tx.fade(0.4));
        ui.label(near.x + 46.0, near.y + 32.0, "Not available yet. Pair a computer by its address.", Font::Caption, t.t2.fade(0.6));
    }
    y += 68.0;
    ui.label(left, y, "A computer that is not listed can be paired by its address.", Font::Caption, t.t2);
    y += 16.0 + 28.0;
    ui.unclip();
    scrolled(app, ui, area, y - top + 8.0);
}

// Connecting, and what can go wrong.

enum Sign {
    Ring,
    Logo,
    Icon(Icon),
}

type Act<'a> = (Id, &'a str, Look, Msg, En);

/// A page with one thing to say, in the middle: a sign, a title, a few words, and what can be
/// done. `stage`: how far connecting has come, of its four steps.
fn status(ui: &mut Ui, area: Rect, sign: Sign, title: &str, words: &str, stage: Option<u8>, acts: &[Act]) {
    let t = ui.t;
    let title_w = (area.w - 48.0).min(560.0);
    let title_h = ui.fonts.height(title, Font::Subtitle, title_w);
    let words_h = ui.fonts.height(words, Font::Caption, 300.0);
    let sign_h = if matches!(sign, Sign::Logo) { 48.0 } else { 40.0 };
    let total = sign_h + 10.0 + title_h + 10.0 + words_h + if stage.is_some() { 26.0 } else { 0.0 } + 18.0 + 32.0;
    let cx = area.x + (area.w / 2.0).round();
    let mut y = area.y + ((area.h - total) / 2.0).round().max(8.0);
    match sign {
        Sign::Ring => ui.ring(cx, y + 20.0, 40.0),
        Sign::Logo => ui.ops.push(Op::Logo { x: cx - 24.0, y, size: 48.0 }),
        Sign::Icon(icon) => ui.ops.push(Op::Icon { icon, x: cx - 20.0, y, size: 40.0, color: t.t2, stroke: 1.1 }),
    }
    y += sign_h + 10.0;
    ui.para(cx - title_w / 2.0, y, title_w, title, Font::Subtitle, t.tx, Align::Center);
    y += title_h + 10.0;
    ui.para(cx - 150.0, y, 300.0, words, Font::Caption, t.t2, Align::Center);
    y += words_h;
    if let Some(stage) = stage {
        y += 10.0;
        // Each step as it is passed: done in green, the one under way in bold.
        let steps = ["Found", "Reached", "Keys checked", "Display"];
        let font = |i: usize| if i as u8 == stage { Font::CaptionStrong } else { Font::Caption };
        let widths: Vec<f32> = steps.iter().enumerate().map(|(i, s)| ui.fonts.width(s, font(i))).collect();
        let mut x = cx - ((widths.iter().sum::<f32>() + 42.0) / 2.0).round();
        for (i, step) in steps.iter().enumerate() {
            let color = if (i as u8) < stage { t.ok } else if i as u8 == stage { t.tx } else { t.t2 };
            ui.label(x, y, step, font(i), color);
            x += widths[i] + 14.0;
        }
        y += 16.0;
    }
    y += 18.0;
    let widths: Vec<f32> = acts.iter().map(|a| ui.button_width(a.1, a.2, false)).collect();
    let mut x = cx - ((widths.iter().sum::<f32>() + 8.0 * (acts.len().max(1) - 1) as f32) / 2.0).round();
    for (act, width) in acts.iter().zip(widths) {
        ui.button(act.0, rect(x, y, width, 32.0), act.1, act.2, None, act.3.clone(), act.4);
        x += width + 8.0;
    }
}

fn connecting(app: &App, ui: &mut Ui) {
    titlebar(app, ui, false);
    let area = rect(0.0, TITLE, ui.w, ui.h - TITLE);
    let Some(attempt) = &app.attempt else { return };
    let (name, d) = (&attempt.target.name, attempt.display);
    let stage = attempt.shared.stage.load(std::sync::atomic::Ordering::Relaxed);
    status(
        ui,
        area,
        Sign::Ring,
        &format!("Connecting to {name}"),
        &format!("Asking {name} for a {} × {} display at {} Hz…", d.width, d.height, d.refresh),
        Some(stage),
        &[(id("connecting.cancel"), "Cancel", Look::Plain, Msg::CancelConnect, ON)],
    );
}

fn trouble(app: &App, ui: &mut Ui, target: &Target, problem: &Problem) {
    titlebar(app, ui, false);
    let area = rect(0.0, TITLE, ui.w, ui.h - TITLE);
    let (name, address) = (&target.name, target.address.clone());
    let known = app.store.find(&address).is_some();
    let back: Act = (id("problem.back"), "Back", Look::Plain, Msg::Back, ON);
    let again = |label: &'static str| -> Act { (id("problem.again"), label, Look::Accent, Msg::Connect(address.clone()), ON) };
    match problem {
        Problem::Unreachable(detail) => {
            let words = format!("{detail} at {}. It may be off, on another network, or its firewall may be closed to remoteman.", short(&address));
            let mut acts = vec![again("Try again")];
            if known {
                acts.push((id("problem.address"), "Edit address…", Look::Plain, Msg::EditAddress(address.clone()), ON));
            }
            acts.push(back);
            status(ui, area, Sign::Icon(Icon::NoAnswer), &format!("Can't reach {name}"), &words, None, &acts);
        }
        Problem::Asleep => {
            let used = app.store.find(&address).map_or(0, |c| c.used);
            let seen = if used > 0 { format!("It was last used from this PC {}. ", when(used)) } else { String::new() };
            let wake: Act = (id("problem.wake"), "Wake and connect", Look::Accent, Msg::Connect(address.clone()), gate(FEATURES.wake));
            status(ui, area, Sign::Icon(Icon::Moon), &format!("{name} is asleep"), &format!("{seen}Waking it over the network takes a few seconds."), None, &[wake, back]);
        }
        Problem::NotPaired => {
            let pair: Act = (id("problem.pair"), "Pair again…", Look::Accent, Msg::Pair(Some(address.clone())), ON);
            let words = "It answered, but this PC is no longer on its list of paired devices. Put it in pairing mode and pair again.";
            status(ui, area, Sign::Icon(Icon::Lock), &format!("{name} doesn't know this PC"), words, None, &[pair, back]);
        }
        Problem::Changed => {
            // Not in the design, which has no page for it: the same shape, in the Linux
            // viewer's words.
            let words = format!("What answered at {} is not the computer this PC was paired with. If it has been set up again, forget it and pair it anew.", short(&address));
            let mut acts = Vec::new();
            if known {
                acts.push((id("problem.settings"), "Settings for this computer…", Look::Plain, Msg::OpenComputer(address.clone()), ON));
            }
            acts.push(back);
            status(ui, area, Sign::Icon(Icon::Info), &format!("{name} has a different key"), &words, None, &acts);
        }
        Problem::Refused(said) => {
            status(ui, area, Sign::Icon(Icon::Info), &format!("{name} closed the connection"), &format!("It answered, and said: {said}"), None, &[again("Connect again"), back]);
        }
        Problem::LoggingIn(said) => {
            // The computer's own words, as a sentence. It is tried again by itself while the
            // ring turns; after that it is the person's turn.
            let mut words = said.chars();
            let said: String = words.next().map(|first| first.to_uppercase().chain(words).collect()).unwrap_or_default();
            let words = format!("It was locked or logged out. {said}{}", if said.ends_with('.') { "" } else { "." });
            let title = format!("{name} is logging in for you");
            if app.retry_at.is_some() {
                status(ui, area, Sign::Ring, &title, &words, None, &[back]);
            } else {
                status(ui, area, Sign::Icon(Icon::Clock), &title, &words, None, &[again("Try again"), back]);
            }
        }
        Problem::Locked(said) => {
            if app.retry_at.is_some() {
                return status(ui, area, Sign::Ring, &format!("Unlocking {name}"), "With the password that was given.", None, &[back]);
            }
            // The password goes to the computer with the next try and is kept nowhere.
            let t = ui.t;
            let words = format!("{said} It locks again when you leave.");
            let title = format!("{name} is locked");
            let title_w = (area.w - 48.0).min(560.0);
            let (title_h, words_h) = (ui.fonts.height(&title, Font::Subtitle, title_w), ui.fonts.height(&words, Font::Caption, 300.0));
            let total = 40.0 + 10.0 + title_h + 10.0 + words_h + 18.0 + 32.0 + 12.0 + 32.0;
            let cx = area.x + (area.w / 2.0).round();
            let mut y = area.y + ((area.h - total) / 2.0).round().max(8.0);
            ui.ops.push(Op::Icon { icon: Icon::Lock, x: cx - 20.0, y, size: 40.0, color: t.t2, stroke: 1.1 });
            y += 50.0;
            ui.para(cx - title_w / 2.0, y, title_w, &title, Font::Subtitle, t.tx, Align::Center);
            y += title_h + 10.0;
            ui.para(cx - 150.0, y, 300.0, &words, Font::Caption, t.t2, Align::Center);
            y += words_h + 18.0;
            if let Some(field) = app.fields.get(&id("locked.password")) {
                ui.secret(id("locked.password"), rect(cx - 140.0, y, 280.0, 32.0), field, ON);
            }
            y += 44.0;
            let (unlock, back_w) = (ui.button_width("Unlock", Look::Accent, false), ui.button_width("Back", Look::Plain, false));
            let x = cx - ((unlock + 8.0 + back_w) / 2.0).round();
            ui.button(id("locked.unlock"), rect(x, y, unlock, 32.0), "Unlock", Look::Accent, None, Msg::Unlock, ON);
            ui.button(id("problem.back"), rect(x + unlock + 8.0, y, back_w, 32.0), "Back", Look::Plain, None, Msg::Back, ON);
        }
        Problem::Ended(reason) => {
            status(ui, area, Sign::Icon(Icon::Info), &format!("{name} closed the connection"), reason, None, &[again("Connect again"), back]);
        }
    }
}

// The settings pages.

enum Ctl {
    Toggle(bool, Option<Msg>),
    Drop(String, Option<Choice>),
    Button(&'static str, Look, Option<Msg>),
    /// A text field 260 wide.
    Field,
    None,
}

struct Row {
    name: &'static str,
    title: String,
    caption: String,
    ctl: Ctl,
    en: En,
    mono: bool,
}

fn row(name: &'static str, title: &str, caption: &str, ctl: Ctl, en: En) -> Row {
    Row { name, title: title.to_owned(), caption: caption.to_owned(), ctl, en, mono: false }
}

/// A heading and a card of rows under it; says where the next thing goes.
fn group(app: &App, ui: &mut Ui, x: f32, mut y: f32, w: f32, heading: &str, rows: &[Row]) -> f32 {
    let t = ui.t;
    ui.label(x, y, heading, Font::Strong, t.tx);
    y += 28.0;
    // Measure first: the card is drawn before what is in it.
    let sizes: Vec<(f32, f32, f32, f32)> = rows
        .iter()
        .map(|r| {
            let control = match &r.ctl {
                Ctl::Toggle(..) => 40.0,
                Ctl::Drop(label, _) => (ui.fonts.width(label, Font::Body) + 46.0).max(190.0),
                Ctl::Button(label, look, _) => ui.button_width(label, *look, false),
                Ctl::Field => 260.0,
                Ctl::None => 0.0,
            };
            let text_w = w - 32.0 - if control > 0.0 { control + 14.0 } else { 0.0 };
            let title_h = ui.fonts.height(&r.title, Font::Body, text_w);
            let caption_h = if r.caption.is_empty() { 0.0 } else { ui.fonts.height(&r.caption, Font::Caption, text_w) };
            (control, text_w, title_h, caption_h)
        })
        .collect();
    let total: f32 = sizes.iter().map(|s| (s.2 + s.3 + 16.0).max(52.0)).sum();
    ui.card(rect(x, y, w, total));
    for (i, (r, (control, text_w, title_h, caption_h))) in rows.iter().zip(sizes).enumerate() {
        let h = (title_h + caption_h + 16.0).max(52.0);
        if i > 0 {
            ui.hline(x + 1.0, y, w - 2.0);
        }
        let dim = if r.en.on { 1.0 } else { 0.45 };
        if !r.en.on {
            // The whole row says why it can't be used.
            ui.region(id(&format!("{}.row", r.name)), rect(x, y, w, h), Kind::Block, None, r.en, "", false);
        }
        let text_y = y + ((h - title_h - caption_h) / 2.0).round();
        ui.para(x + 16.0, text_y, text_w, &r.title, Font::Body, t.tx.fade(dim), Align::Left);
        if !r.caption.is_empty() {
            ui.para(x + 16.0, text_y + title_h, text_w, &r.caption, if r.mono { Font::Mono } else { Font::Caption }, t.t2.fade(dim), Align::Left);
        }
        let at = rect(x + w - 16.0 - control, y + ((h - 32.0) / 2.0).round(), control, 32.0);
        let rid = id(r.name);
        match &r.ctl {
            Ctl::Toggle(on, msg) => ui.toggle(rid, at.x, at.y + 6.0, *on, &r.title, msg.clone().unwrap_or(Msg::CloseMenu), r.en),
            Ctl::Drop(label, choice) => {
                let msg = choice.clone().map_or(Msg::CloseMenu, |c| Msg::OpenMenu(MenuKind::Choice(c), at));
                ui.dropdown(rid, at, label, msg, r.en);
            }
            Ctl::Button(label, look, msg) => ui.button(rid, at, label, *look, None, msg.clone().unwrap_or(Msg::CloseMenu), r.en),
            Ctl::Field => {
                if let Some(field) = app.fields.get(&rid) {
                    ui.field(rid, at, field, r.en);
                }
            }
            Ctl::None => {}
        }
        y += h;
    }
    y + 18.0
}

fn chosen(app: &App, choice: Choice) -> Ctl {
    let (options, at) = app.options(&choice);
    Ctl::Drop(options.get(at).map(|o| o.0.clone()).unwrap_or_default(), Some(choice))
}

fn fixed(label: &str) -> Ctl {
    Ctl::Drop(label.to_owned(), None)
}

fn settings(app: &App, ui: &mut Ui) {
    let (w, h, t) = (ui.w, ui.h, ui.t);
    let s = &app.store.settings;
    let area = rect(0.0, TITLE, w, h - TITLE);
    ui.clip(area);
    let top = TITLE + 8.0 - app.scroll.min(app.scroll_room.get());
    let (x, room) = (40.0, w - 80.0);
    let mut y = top;
    ui.label(x, y, "Settings", Font::Title, t.tx);
    y += 36.0;
    y += ui.para(x, y, room, "For every computer. One computer can differ: open its own settings from the list.", Font::Caption, t.t2, Align::Left) + 18.0;

    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Session window",
        &[
            row("set.controls", "Controls", "A title bar, or nothing but the picture with a bar that appears at the top edge", chosen(app, Choice::Controls), ON),
            row("set.fullscreen", "Start in full screen", "", Ctl::Toggle(s.fullscreen, Some(Msg::StartFullscreen)), ON),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Picture",
        &[
            row("set.quality", "Default quality", "Automatic picks the best stream this PC decodes on its graphics card", chosen(app, Choice::DefaultQuality), ON),
            row("set.refresh", "Match this monitor's refresh rate", "Otherwise 60 Hz", Ctl::Toggle(s.match_refresh, Some(Msg::Toggle(Flag::MatchRefresh, None))), gate(FEATURES.refresh_choice)),
            row("set.hdr", "HDR", "Used when the computer offers it and this monitor is in HDR", Ctl::Toggle(false, None), gate(FEATURES.hdr)),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Keyboard",
        &[
            row(
                "set.shortcuts",
                "Send system shortcuts to the computer",
                "Alt+Tab, the Windows key and others, while a session is in front. Ctrl+Alt+Delete is always in Send keys.",
                Ctl::Toggle(FEATURES.system_shortcuts && s.shortcuts, Some(Msg::Toggle(Flag::Shortcuts, None))),
                gate(FEATURES.system_shortcuts),
            ),
            row("set.release", "Release the keyboard", "Ctrl+Alt+Shift, pressed together, gives shortcuts back to this PC, and takes them again", Ctl::None, gate(FEATURES.release_shortcut)),
            row("set.command", "On a Mac, Ctrl acts as Command", "Ctrl+C on this keyboard copies on a Mac, as it does here", Ctl::Toggle(s.ctrl_as_command, Some(Msg::Toggle(Flag::CtrlAsCommand, None))), gate(FEATURES.ctrl_as_command)),
            row("set.winkey", "Windows key", "Opens the overview on a Linux computer, Spotlight's key on a Mac", fixed("Super / Command"), gate(FEATURES.key_mapping)),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Pointer and touch",
        &[
            row("set.pointer", "Pointer", "Captured sends raw movement, which games and 3D apps need", fixed("Follows this pointer"), gate(FEATURES.pointer_capture)),
            row("set.scroll", "Scroll direction", "", chosen(app, Choice::Scroll), gate(FEATURES.scroll_direction)),
            row("set.touch", "Send touchscreen touches as touch", "Fingers arrive as fingers, so gestures on the computer work", Ctl::Toggle(false, None), gate(FEATURES.touch)),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Sound, microphone and camera",
        &[
            row("set.sound", "Play the computer's sound here", "For computers paired from now on; each has its own switch", Ctl::Toggle(s.sound, Some(Msg::Toggle(Flag::Sound, None))), gate(FEATURES.sound)),
            row("set.mic", "Let computers use this microphone", "Still switched on by you in each session", Ctl::Toggle(false, None), gate(FEATURES.microphone)),
            row("set.camera", "Let computers use this camera", "For calls made on the other computer", Ctl::Toggle(false, None), gate(FEATURES.camera)),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Sharing",
        &[
            row("set.clipboard", "Share the clipboard", "Text copied on either side can be pasted on the other. For computers paired from now on", Ctl::Toggle(s.clipboard, Some(Msg::Toggle(Flag::Clipboard, None))), gate(FEATURES.clipboard)),
            row("set.folder", "Save received files to", &app.store.receive_folder().display().to_string(), Ctl::Button("Change…", Look::Plain, None), gate(FEATURES.folder_choice)),
        ],
    );
    let version = format!("remoteman {} · ", env!("CARGO_PKG_VERSION"));
    let about = x + ui.label(x, y, &version, Font::Caption, t.t2);
    let link = rect(about, y, ui.fonts.width("About", Font::Caption), 16.0);
    let state = ui.region(id("set.about"), link, Kind::Button, Some(Msg::About), ON, "", true);
    ui.label(about, y, "About", Font::Caption, if state.hot { t.at } else { t.t2 });
    ui.focus_ring(link, 2.0, state);
    y += 16.0 + 28.0;
    ui.unclip();
    scrolled(app, ui, area, y - top + 8.0);
    titlebar(app, ui, true);
}

fn computer(app: &App, ui: &mut Ui, address: &str) {
    let (w, h, t) = (ui.w, ui.h, ui.t);
    let Some(c) = app.store.find(address) else { return titlebar(app, ui, true) };
    let area = rect(0.0, TITLE, w, h - TITLE);
    ui.clip(area);
    let top = TITLE + 8.0 - app.scroll.min(app.scroll_room.get());
    let (x, room) = (40.0, w - 80.0);
    let mut y = top;
    let crumb = rect(x, y, ui.fonts.width("Computers ›", Font::Caption), 16.0);
    let state = ui.region(id("computer.crumb"), crumb, Kind::Button, Some(Msg::Back), ON, "", false);
    ui.label(x, y, "Computers ›", Font::Caption, if state.hot { t.at } else { t.t2 });
    y += 16.0;
    ui.label_in(rect(x, y, room, 36.0), &c.name, Font::Title, t.tx, Align::Left);
    y += 36.0;
    let mut about = Vec::new();
    if !c.os.is_empty() {
        about.push(os_name(&c.os));
    }
    if c.paired > 0 {
        about.push(format!("paired {}", when(c.paired)));
    }
    about.push(if c.used > 0 { format!("last used {}", when(c.used)) } else { "not used yet".into() });
    ui.label(x, y, &about.join(" · "), Font::Caption, t.t2);
    y += 16.0 + 18.0;

    let a = || address.to_owned();
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Connection",
        &[
            row("computer.name", "Name", "", Ctl::Field, ON),
            row("computer.address", "Address", short(address), Ctl::Button("Edit address…", Look::Plain, Some(Msg::EditAddress(a()))), ON),
            row("computer.wake", "Wake it over the network", "When it is asleep and on the same network", Ctl::Toggle(false, None), gate(FEATURES.wake)),
            row("computer.way", "Getting in", "Joins your session there if you are logged in, otherwise shows its login screen", fixed("The computer decides"), gate(FEATURES.way_in)),
        ],
    );
    let sits = PLACEMENT_NAMES.iter().find(|p| p.0 == c.placement).map_or("", |p| p.2);
    let size = match c.max_height {
        0 => "The picture has this screen's own pixels",
        crate::store::WINDOW => "The computer is asked for a display as large as the window, again when the window changes size",
        _ => "Fewer pixels, drawn larger here: quicker for a computer that can't keep up",
    };
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Display",
        &[
            row("computer.placement", "Where this display sits", sits, chosen(app, Choice::Placement(a())), ON),
            row("computer.size", "Picture size", size, chosen(app, Choice::Size(a())), gate(FEATURES.follow_window)),
            row("computer.quality", "Picture quality", "", chosen(app, Choice::Quality(a())), ON),
            row("computer.bitrate", "Bitrate limit", "", chosen(app, Choice::Bitrate(a())), ON),
        ],
    );
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Sound and sharing",
        &[
            row("computer.sound", "Play its sound here", "When a session starts; the bar's button is for the session", Ctl::Toggle(c.sound, Some(Msg::Toggle(Flag::Sound, Some(a())))), gate(FEATURES.sound)),
            row("computer.clipboard", "Share the clipboard with it", "When a session starts, if the computer allows it", Ctl::Toggle(c.clipboard, Some(Msg::Toggle(Flag::Clipboard, Some(a())))), gate(FEATURES.clipboard)),
            row("computer.files", "Files it sends", &format!("Go into {}, never over a file that is there", app.store.receive_folder().display()), Ctl::None, gate(FEATURES.files)),
        ],
    );
    let key = match app.store.key(address) {
        Some(key) => format!("{} · the same is shown on {}", crate::store::key_words(&key, 4), c.name),
        None => "This PC has no key for it: it is not paired".into(),
    };
    let mut its_key = row("computer.key", "Its key", &key, Ctl::None, ON);
    its_key.mono = true;
    y = group(
        app,
        ui,
        x,
        y,
        room,
        "Pairing",
        &[its_key, row("computer.forget", "Forget this computer", "It has to be paired again before this PC can connect", Ctl::Button("Forget…", Look::Bad, Some(Msg::AskForget(a()))), ON)],
    );
    y += 10.0;
    ui.unclip();
    scrolled(app, ui, area, y - top + 8.0);
    titlebar(app, ui, true);
}

// Dialogs.

/// A column that is gone down twice: once to find how tall it is, once to draw it.
struct Col<'u, 'a> {
    ui: &'u mut Ui<'a>,
    x: f32,
    y: f32,
    w: f32,
    draw: bool,
}

impl Col<'_, '_> {
    fn para(&mut self, text: &str, font: Font, color: Color) {
        let h = self.ui.fonts.height(text, font, self.w);
        if self.draw {
            self.ui.ops.push(Op::Text { r: rect(self.x, self.y, self.w, h), text: text.to_owned(), font, color, align: Align::Left, wrap: true });
        }
        self.y += h;
    }

    /// Room `h` tall for something else; `None` while only measuring.
    fn room(&mut self, h: f32) -> Option<Rect> {
        let r = rect(self.x, self.y, self.w, h);
        self.y += h;
        self.draw.then_some(r)
    }

    fn gap(&mut self, h: f32) {
        self.y += h;
    }
}

/// A dialog over the window: what is behind it greyed and out of reach, a body, and a foot
/// with one or two buttons side by side.
fn sheet(ui: &mut Ui, ok: Option<(&str, Look, En)>, cancel: &str, body: &dyn Fn(&mut Col)) {
    let t = ui.t;
    let whole = ui.whole();
    ui.region(id("dialog.behind"), whole, Kind::Block, None, ON, "", false);
    ui.fill(whole, 0.0, rgba(0x000000, if t.dark { 0.5 } else { 0.3 }));
    let w = 448.0f32.min(whole.w - 16.0);
    let mut measure = Col { ui, x: 0.0, y: 0.0, w: w - 48.0, draw: false };
    body(&mut measure);
    let body_h = measure.y + 48.0;
    let r = rect(((whole.w - w) / 2.0).round(), ((whole.h - body_h - 81.0) / 2.0).round().max(4.0), w, body_h + 81.0);
    ui.panel(r, 8.0, t.pane);
    ui.region(id("dialog"), r, Kind::Block, None, ON, "", false);
    // The body is the card colour down to the foot's line.
    ui.clip(rect(r.x + 1.0, r.y + 1.0, r.w - 2.0, body_h - 1.0));
    ui.fill(r.inset(1.0), 7.0, t.card);
    ui.unclip();
    ui.hline(r.x + 1.0, r.y + body_h, r.w - 2.0);
    let mut col = Col { ui, x: r.x + 24.0, y: r.y + 24.0, w: w - 48.0, draw: true };
    body(&mut col);
    let half = ((w - 48.0 - 8.0) / 2.0).floor();
    let foot = r.y + body_h + 1.0 + 24.0;
    match ok {
        Some((label, look, en)) => {
            ui.button(id("dialog.ok"), rect(r.x + 24.0, foot, half, 32.0), label, look, None, Msg::DialogOk, en);
            ui.button(id("dialog.cancel"), rect(r.x + 32.0 + half, foot, half, 32.0), cancel, Look::Plain, None, Msg::DialogCancel, ON);
        }
        None => ui.button(id("dialog.cancel"), rect(r.x + 32.0 + half, foot, half, 32.0), cancel, Look::Plain, None, Msg::DialogCancel, ON),
    }
}

pub fn dialog(app: &App, ui: &mut Ui, dialog: &Dialog) {
    let t = ui.t;
    match dialog {
        Dialog::Pair { waiting, problem } => {
            let busy = waiting.is_some();
            sheet(ui, Some(("Pair", Look::Accent, if busy { off("") } else { ON })), "Cancel", &|c| {
                c.para("Pair a computer", Font::Subtitle, t.tx);
                c.gap(16.0);
                c.para("Put the other computer in pairing mode, then enter its address here.", Font::Body, t.tx);
                c.gap(16.0);
                c.para("Address", Font::Body, t.tx);
                c.gap(6.0);
                if let (Some(r), Some(field)) = (c.room(32.0), app.fields.get(&id("pair.address"))) {
                    c.ui.field(id("pair.address"), r, field, if busy { off("") } else { ON });
                }
                c.gap(6.0);
                c.para(&format!("A name or an IP address. Add a colon and the port if it is not {}.", remoteman_core::PORT), Font::Caption, t.t2);
                c.gap(16.0);
                c.para("Nearby, in pairing mode", Font::Body, if FEATURES.discovery { t.tx } else { t.tx.fade(0.45) });
                c.gap(6.0);
                if let Some(r) = c.room(52.0) {
                    // Where the computers found on the network will be listed.
                    c.ui.card(r);
                    c.ui.region(id("pair.nearby"), r, Kind::Block, None, off(NOT_YET), "", false);
                    c.ui.icon(Icon::Computer, r.x + 16.0, r.y + 18.0, t.tx.fade(0.4));
                    c.ui.label(r.x + 46.0, r.y + 8.0, "Looking for computers on this network", Font::Body, t.tx.fade(0.4));
                    c.ui.label(r.x + 46.0, r.y + 28.0, NOT_YET, Font::Caption, t.t2.fade(0.6));
                }
                if let Some(host) = waiting {
                    c.gap(16.0);
                    if let Some(r) = c.room(20.0) {
                        c.ui.ring(r.x + 10.0, r.y + 10.0, 20.0);
                        c.ui.label(r.x + 30.0, r.y, &format!("Waiting for {host}…"), Font::Body, t.tx);
                    }
                }
                if let Some(problem) = problem {
                    c.gap(16.0);
                    c.para(problem, Font::Body, t.bad);
                }
            });
        }
        Dialog::Code { name, code, until } => {
            let left = until.saturating_duration_since(std::time::Instant::now()).as_secs() as u32;
            sheet(ui, None, "Cancel", &|c| {
                c.para(&format!("Pair with {name}?"), Font::Subtitle, t.tx);
                c.gap(16.0);
                c.para(&format!("{name} is asking its owner about this PC. Check that it shows the same code, and confirm there. If it shows a different one, cancel: something else answered at that address."), Font::Body, t.tx);
                c.gap(16.0);
                if let Some(r) = c.room(88.0) {
                    c.ui.fill(r, 6.0, t.sub);
                    // Two groups of three, as it is read out.
                    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
                    let (a, b) = digits.split_at(digits.len() / 2);
                    let (wa, wb) = (c.ui.fonts.width(a, Font::Code), c.ui.fonts.width(b, Font::Code));
                    let x = r.x + ((r.w - wa - wb - 20.0) / 2.0).round();
                    c.ui.label(x, r.y + 20.0, a, Font::Code, t.tx);
                    c.ui.label(x + wa + 20.0, r.y + 20.0, b, Font::Code, t.tx);
                }
                c.gap(16.0);
                if let Some(r) = c.room(16.0) {
                    c.ui.icon(Icon::Clock, r.x, r.y, t.t2);
                    c.ui.label(r.x + 22.0, r.y, &format!("Pairing closes in {}:{:02}", left / 60, left % 60), Font::Caption, t.t2);
                }
                c.gap(16.0);
                c.para(&format!("After pairing, this PC and {name} keep each other's key. Nothing else has to be typed again."), Font::Caption, t.t2);
            });
        }
        Dialog::Paired { target, key } => {
            let name = &target.name;
            sheet(ui, Some(("Connect", Look::Accent, ON)), "Close", &|c| {
                c.para(&format!("{name} is paired"), Font::Subtitle, t.tx);
                c.gap(16.0);
                c.para(&format!("This PC and {name} keep each other's key. Nothing else has to be typed again."), Font::Body, t.tx);
                c.gap(16.0);
                c.para("Its key", Font::Body, t.tx);
                c.para(&format!("{key} · the same is shown on {name}"), Font::Mono, t.t2);
            });
        }
        Dialog::Forget(address) => {
            let name = app.name_of(address);
            sheet(ui, Some(("Forget", Look::Bad, ON)), "Cancel", &|c| {
                c.para(&format!("Forget {name}?"), Font::Subtitle, t.tx);
                c.gap(16.0);
                c.para(&format!("It has to be paired again before this PC can connect to {name}."), Font::Body, t.tx);
            });
        }
        Dialog::Address { address, problem } => {
            let name = app.name_of(address);
            sheet(ui, Some(("Save", Look::Accent, ON)), "Cancel", &|c| {
                c.para(&format!("Address of {name}"), Font::Subtitle, t.tx);
                c.gap(16.0);
                c.para("Where this PC looks for the computer. Its key stays with it.", Font::Body, t.tx);
                c.gap(16.0);
                c.para("Address", Font::Body, t.tx);
                c.gap(6.0);
                if let (Some(r), Some(field)) = (c.room(32.0), app.fields.get(&id("address.new"))) {
                    c.ui.field(id("address.new"), r, field, ON);
                }
                c.gap(6.0);
                c.para(&format!("A name or an IP address. Add a colon and the port if it is not {}.", remoteman_core::PORT), Font::Caption, t.t2);
                if let Some(problem) = problem {
                    c.gap(16.0);
                    c.para(problem, Font::Body, t.bad);
                }
            });
        }
        Dialog::About => {
            sheet(ui, None, "Close", &|c| {
                if let Some(r) = c.room(48.0) {
                    c.ui.ops.push(Op::Logo { x: r.x, y: r.y, size: 48.0 });
                }
                c.gap(16.0);
                c.para("remoteman", Font::Subtitle, t.tx);
                c.para(&format!("Version {}", env!("CARGO_PKG_VERSION")), Font::Caption, t.t2);
                c.gap(16.0);
                c.para("See and use another computer's screen from this one.", Font::Body, t.tx);
            });
        }
    }
}

/// What a control has to say, in a small box under it (over it, if there is no room under).
pub fn tip(ui: &mut Ui, text: &str, by: Rect) {
    let t = ui.t;
    let w = ui.fonts.width(text, Font::Caption) + 16.0;
    let x = (by.x + (by.w - w) / 2.0).round().clamp(4.0, (ui.w - w - 4.0).max(4.0));
    let mut y = by.bottom() + 6.0;
    if y + 28.0 > ui.h {
        y = by.y - 34.0;
    }
    let r = rect(x, y, w, 28.0);
    ui.fill(r, 4.0, t.pane);
    ui.stroke(r, 4.0, t.line.fade(1.6));
    ui.label_in(r, text, Font::Caption, t.tx, Align::Center);
}
