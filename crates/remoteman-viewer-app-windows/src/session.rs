//! The session's bar: a pill floating over the top of the picture, as wide as its controls, in
//! in-app acrylic: always there ("Title bar"), or brought out at the top edge ("Show the bar on
//! hover"). Its words are what the window is moved by, and it carries the window's own
//! buttons, since the window has no title bar of Windows' own during a session. Its menus are
//! WinUI menus with check and radio items (the vendored Reactor's patch 6); Picture is a flyout,
//! for the bitrate's slider. And the figures over the picture.

use serde_json::{Value, json};
use windows_reactor::*;

use crate::app::{Msg, Viewer};
use crate::kit::{self, J, act, caption, cmd, glyph};

fn offers(state: &Value, feature: &str) -> bool {
    state.o("session").a("features").iter().any(|f| f.as_str() == Some(feature))
}

/// A menu item's key: the engine's command as JSON.
fn key(what: &str, more: Value) -> String {
    cmd(what, more).to_string()
}

fn drop_down(g: &str, text: &str) -> DropDownButton {
    DropDownButton::new()
        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((kit::icon(g, 16.0), TextBlock::new().text(text))))
        .automation_name(text)
}

pub fn pill(v: &Viewer, context: &ViewContext<Viewer>) -> View {
    let st = &v.state;
    let s = st.o("session");
    let f = st.o("features");
    let floating = st.b("fullscreen") || st.o("settings").b("hover_bar");
    let shown = !floating || v.bar_peek;
    let format = match s.a("format") {
        [w, h, hz] => format!("{w} × {h} · {hz} Hz"),
        _ => String::new(),
    };
    // The words: where the window is moved from (and maximised, twice).
    let words = Border::new()
        .background(Color::transparent())
        .padding(Thickness::new(10.0, 0.0, 6.0, 0.0))
        .max_width(340.0)
        .on_pointer_pressed(context.callback(|_| Msg::Drag))
        .on_double_tapped(context.message(Msg::Window("maximise")))
        .content(
            StackPanel::new().orientation(Orientation::Horizontal).spacing(10.0).vertical_alignment(VerticalAlignment::Center).children((
                kit::strong(s.s("name")).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center),
                caption(&format).vertical_alignment(VerticalAlignment::Center),
            )),
        );
    let menus = context.callback(Msg::Menu);
    let toggle_button = |g: &str, name: &str, on: bool, enabled: bool, tip: &str, changed: Callback<Option<bool>>| -> View {
        ToggleButton::new()
            .content(kit::icon(g, 16.0))
            .min_width(36.0)
            .height(32.0)
            .is_checked(on)
            .is_enabled(enabled)
            .automation_name(name)
            .on_is_checked_changed(changed)
            .tooltip(tip)
    };
    let sound = toggle_button(glyph::SOUND, "Play the computer's sound here", s.b("sound"), offers(st, "sound") && f.b("sound"), "Play the computer's sound here", context.callback(|_| Msg::Do(cmd("sound", Value::Null))));
    // The engine says whether each may be lent and, if not, why (the tip).
    let mic_tip = if !s.s("mic_why").is_empty() {
        s.s("mic_why").to_owned()
    } else if s.b("mic") {
        format!("Microphone, on: {}", s.s("mic_device"))
    } else {
        "Microphone, off".into()
    };
    let mic = toggle_button(glyph::MIC, "Microphone", s.b("mic"), s.b("mic_can") || s.b("mic"), &mic_tip, context.callback(|on: Option<bool>| Msg::Do(cmd("mic", json!({"on": on.unwrap_or(false)})))));
    let cam_tip = if !s.s("camera_why").is_empty() {
        s.s("camera_why").to_owned()
    } else if s.b("camera") {
        "Camera, on".into()
    } else {
        "Camera, off".into()
    };
    let camera = toggle_button(glyph::CAMERA, "Camera", s.b("camera"), s.b("camera_can") || s.b("camera"), &cam_tip, context.callback(|on: Option<bool>| Msg::Do(cmd("camera", json!({"on": on.unwrap_or(false)})))));
    let share = kit::square(glyph::SHARE, "Clipboard and files", None).is_enabled(offers(st, "clipboard") || offers(st, "files")).menu(Menu::new(share_menu(st), menus.clone()));
    let more = kit::square(glyph::MORE, "More", None).menu(Menu::new(more_menu(st), menus.clone()));
    let full = kit::square(glyph::FULL_SCREEN, "Full screen (F11)", Some(act(context, "fullscreen", Value::Null))).tooltip("Full screen (F11)");
    let off = kit::button("Disconnect", act(context, "disconnect", Value::Null), false, Some(glyph::CLOSE));
    let keys = drop_down(glyph::KEYBOARD, "Send keys").menu(Menu::new(keys_menu(st), menus.clone()));
    let display = drop_down(glyph::DISPLAY, "Display").menu(Menu::new(display_menu(st), menus.clone()));
    let picture = drop_down(glyph::PICTURE, "Picture").flyout_with(Flyout::rich(picture_flyout(st, context)).placement(FlyoutPlacement::Bottom));
    let window_button = |g: &str, name: &'static str, what: &'static str| -> View {
        kit::square(g, name, Some(context.message(Msg::Window(what)))).background(Color::transparent()).tooltip(name)
    };
    let maximised = crate::system::maximised(v.hwnd);
    let window_buttons = StackPanel::new().orientation(Orientation::Horizontal).children((
        window_button(glyph::MINIMISE, "Minimise", "minimise"),
        window_button(if maximised { glyph::RESTORE } else { glyph::MAXIMISE }, if maximised { "Restore" } else { "Maximise" }, "maximise"),
        window_button(glyph::WINDOW_CLOSE, "Close", "close"),
    ));
    let tools = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(4.0)
        .vertical_alignment(VerticalAlignment::Center)
        .children((off, keys, display, picture, sound, mic, camera, share, more, full, window_buttons));
    let bar = Border::new()
        .vertical_alignment(VerticalAlignment::Top)
        .horizontal_alignment(HorizontalAlignment::Center)
        .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        .height(44.0)
        .padding(Thickness::new(4.0, 0.0, 6.0, 0.0))
        .corner_radius(22.0)
        .border_thickness(1.0)
        .background(ThemeBrush::AcrylicInApp)
        .border_brush(ThemeBrush::CardStroke)
        .automation_name("Session bar")
        .on_pointer_entered(context.callback(|_| Msg::OverBar(true)))
        .on_pointer_exited(context.callback(|_| Msg::OverBar(false)))
        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children((words, tools)));
    // Hidden, it stays (a menu of it may be open) but out of sight above the window's top.
    if shown { bar.into() } else { bar.opacity(0.0).translation(0.0, -80.0).into() }
}

fn keys_menu(st: &Value) -> Vec<MenuItem> {
    let chords = [
        ("Ctrl+Alt+Delete", "Windows can't pass this one on", "ctrl-alt-del"),
        ("Win", "Super on Linux, Command on a Mac", "win"),
        ("Win+L", "Locks the other computer", "win-l"),
        ("Alt+Tab", "", "alt-tab"),
        ("Alt+F4", "", "alt-f4"),
        ("Ctrl+Shift+Esc", "", "ctrl-shift-esc"),
        ("Print Screen", "", "print"),
    ];
    let mut items: Vec<MenuItem> = chords
        .iter()
        .map(|(label, hint, keys)| {
            let item = MenuItem::item(key("keys", json!({"keys": keys})), *label);
            if hint.is_empty() { item } else { item.tooltip(*hint) }
        })
        .collect();
    items.push(MenuItem::separator("sep"));
    items.push(MenuItem::check(key("toggle", json!({"flag": "shortcuts"})), "Send system shortcuts to the computer", st.o("settings").b("shortcuts")).enabled(st.o("features").b("system_shortcuts")));
    items.push(MenuItem::disabled("release", "Release the keyboard: Ctrl+Alt+Shift"));
    items
}

fn display_menu(st: &Value) -> Vec<MenuItem> {
    let s = st.o("session");
    let f = st.o("features");
    let now = s.s("placement");
    let mut items = vec![MenuItem::disabled("where", "Where this display sits")];
    for (place, text, hint) in [
        ("extend", "Extra display", "Beside the computer's own displays, which stay on"),
        ("main", "Main display", "Its own displays stay on as secondary ones"),
        ("only", "Only display", "Its own screens turn off while you are connected"),
    ] {
        items.push(MenuItem::radio(key("place", json!({"placement": place})), text, "place", now == place).enabled(offers(st, "placement") || place == "extend").tooltip(hint));
    }
    items.push(MenuItem::radio(key("place", json!({"placement": "mirror"})), format!("Mirror a monitor of {}", s.s("name")), "place", now == "mirror").enabled(offers(st, "mirror") && f.b("mirror")));
    items.push(MenuItem::separator("sep1"));
    let opts = st.o("computer").o("size");
    let at = opts.n("at") as usize;
    let sizes: Vec<MenuItem> = opts
        .a("items")
        .iter()
        .enumerate()
        .map(|(index, item)| MenuItem::radio(key("choose", json!({"choice": "size", "address": s.s("address"), "index": index})), item.s("text"), "size", index == at).enabled(f.b("follow_window")))
        .collect();
    items.push(MenuItem::submenu("size", "Size", sizes));
    let mon = st.a("monitor");
    let hz = mon.get(2).and_then(Value::as_u64).unwrap_or(60);
    items.push(MenuItem::disabled("rate", format!("Refresh rate: this monitor's, {hz} Hz")));
    // The engine says what each comes to now ("picture"); these only show it and flip it.
    let pic = st.o("picture");
    items.push(
        MenuItem::check(key("toggle", json!({"flag": "hdr"})), "HDR", st.o("settings").b("hdr"))
            .enabled(f.b("hdr") && offers(st, "hdr") && pic.o("hdr").b("screen"))
            .tooltip(format!("When the computer offers it and this monitor is in HDR. Now: {}", pic.o("hdr").s("why"))),
    );
    let vrr = pic.o("variable_refresh");
    items.push(
        MenuItem::check(key("toggle", json!({"flag": "variable-refresh"})), if f.b("variable_refresh") { "Variable refresh rate" } else { "Variable refresh rate (not available yet)" }, f.b("variable_refresh") && st.o("settings").b("variable_refresh"))
            .enabled(f.b("variable_refresh") && vrr.get("range").is_some_and(Value::is_array) && vrr.b("tearing_allowed"))
            .tooltip(format!("Shows each frame when it arrives, in full screen. Now: {}", vrr.s("why"))),
    );
    items.push(MenuItem::separator("sep2"));
    let second = s.b("second");
    items.push(MenuItem::item(key("second", Value::Null), if second { "Remove the second display" } else { "Add a second display…" }).glyph(glyph::DISPLAY).enabled(offers(st, "displays") && f.b("second_display")));
    items
}

fn share_menu(st: &Value) -> Vec<MenuItem> {
    let s = st.o("session");
    vec![
        MenuItem::check(key("clipboard", Value::Null), "Share the clipboard", s.b("clipboard")).enabled(offers(st, "clipboard")).tooltip("Text and images copied on either side can be pasted on the other"),
        MenuItem::item(key("send-file", Value::Null), "Send a file…").glyph(glyph::SEND).enabled(offers(st, "files")),
        MenuItem::disabled("received", format!("Received files: saved to {}, never over an existing file", st.o("settings").s("folder"))),
    ]
}

fn more_menu(st: &Value) -> Vec<MenuItem> {
    let address = st.o("session").s("address");
    let hover = st.o("settings").b("hover_bar");
    vec![
        MenuItem::item(key("computer", json!({"address": address})), "Settings for this computer…").glyph(glyph::SETTINGS),
        MenuItem::item(key("settings", Value::Null), "Settings"),
        MenuItem::separator("sep1"),
        MenuItem::radio(key("bar-mode", json!({"hover": false})), "Title bar", "bar", !hover),
        MenuItem::radio(key("bar-mode", json!({"hover": true})), "Show the bar on hover", "bar", hover),
        MenuItem::separator("sep2"),
        MenuItem::item(key("about", Value::Null), "About remoteman"),
    ]
}

/// Picture: the quality, the bitrate's limit on a slider with what it is now, and the figures.
fn picture_flyout(st: &Value, context: &ViewContext<Viewer>) -> View {
    let s = st.o("session");
    let d = st.o("decodes");
    let now = s.s("quality");
    let mut col: Vec<View> = vec![kit::strong("Picture quality").font_size(12.0).foreground(ThemeBrush::SecondaryText).into()];
    for (quality, text, on) in [
        ("auto", "Automatic", true),
        ("sharp", "Sharp text (4:4:4)", st.o("features").b("sharp_text") && d.b("hevc444")),
        ("hevc", "HEVC", d.b("hevc")),
        ("av1", "AV1", d.b("av1")),
    ] {
        col.push(
            RadioButton::new()
                .group_name("quality")
                .content(text)
                .is_checked(now == quality)
                .is_enabled(on)
                .automation_name(text)
                .on_checked(context.callback(move |_| Msg::Do(cmd("pick", json!({"quality": quality})))))
                .into(),
        );
    }
    let steps: Vec<i64> = st.a("bitrates").iter().filter_map(Value::as_f64).map(|x| x as i64).collect();
    if !steps.is_empty() {
        let step = (s.n("bitrate_step") as usize).min(steps.len() - 1);
        let limit = |i: usize| if steps[i] == 0 { "No limit".to_owned() } else { format!("Up to {} Mb/s", steps[i]) };
        col.push(Border::new().height(1.0).background(ThemeBrush::CardStroke).margin(Thickness::new(0.0, 4.0, 0.0, 4.0)).into());
        col.push(kit::strong("Bitrate limit").font_size(12.0).foreground(ThemeBrush::SecondaryText).into());
        col.push(
            Slider::new()
                .minimum(0.0)
                .maximum((steps.len() - 1) as f64)
                .step_frequency(1.0)
                .value(step as f64)
                .width(240.0)
                .is_enabled(offers(st, "configure"))
                .automation_name(format!("Bitrate limit: {}", limit(step)))
                .on_value_changed(context.callback(Msg::Bitrate))
                .into(),
        );
        col.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((caption(&limit(step)), caption(&format!("Now {:.0} Mb/s", s.o("figures").n("megabits"))).grid_column(1)))
                .into(),
        );
    }
    col.push(Border::new().height(1.0).background(ThemeBrush::CardStroke).margin(Thickness::new(0.0, 4.0, 0.0, 4.0)).into());
    col.push(
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(12.0)
            .children((
                StackPanel::new().vertical_alignment(VerticalAlignment::Center).children((TextBlock::new().text("Show figures"), caption("Ctrl+Alt+Shift+I"))),
                ToggleSwitch::new().is_on(st.o("settings").b("figures")).on_content("").off_content("").min_width(0.0).automation_name("Show figures").on_toggled(context.callback(|_| Msg::Do(cmd("figures", Value::Null)))).grid_column(1),
            ))
            .into(),
    );
    StackPanel::new().spacing(6.0).min_width(280.0).children(col).into()
}

/// The figures over the picture, as the design has them.
pub fn figures(st: &Value) -> View {
    let f = st.o("session").o("figures");
    if !st.o("settings").b("figures") || !f.is_object() {
        return Grid::new().into();
    }
    let lines = [
        ("FRAMES PER SECOND", format!("{}", f.n("shown") as i64)),
        ("DECODED", format!("{} a second", f.n("decoded") as i64)),
        ("PICTURE AGE", format!("{:.0} ms", f.n("age_ms"))),
        ("ROUND TRIP", format!("{:.1} ms", f.n("round_trip_ms"))),
        ("BITRATE", format!("{:.0} Mb/s", f.n("megabits"))),
        ("LOST FRAMES", format!("{}", f.n("lost") as i64)),
        ("REPLACED UNSEEN", format!("{}", f.n("replaced") as i64)),
        ("DECODER", f.s("decoder").to_owned()),
    ];
    let mut cells: Vec<View> = Vec::new();
    for (i, (k, v)) in lines.iter().enumerate() {
        cells.push(TextBlock::new().text(*k).font_size(11.0).foreground(Color::rgb(200, 200, 200)).grid_row(i as i32).into());
        cells.push(TextBlock::new().text(v.as_str()).font_size(12.0).font_weight(FontWeight::SEMI_BOLD).foreground(Color::rgb(255, 255, 255)).grid_row(i as i32).grid_column(1).into());
    }
    Border::new()
        .padding(Thickness::new(14.0, 10.0, 14.0, 10.0))
        .corner_radius(8.0)
        .background(Color::argb(200, 20, 20, 20))
        .horizontal_alignment(HorizontalAlignment::Right)
        .vertical_alignment(VerticalAlignment::Top)
        .margin(Thickness::new(12.0, 60.0, 12.0, 12.0))
        .automation_name("Figures")
        .content(Grid::new().columns([GridLength::Auto, GridLength::Auto]).rows(vec![GridLength::Auto; lines.len()]).column_spacing(16.0).row_spacing(2.0).children(cells))
        .into()
}
