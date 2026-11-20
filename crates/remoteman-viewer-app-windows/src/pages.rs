//! The pages: the computers (paired and nearby), connecting and the pages that say why there is
//! no session, Settings, and one computer's settings. The words are the design's
//! (docs/design/windows/Viewer*.dc.html), as the C# view had them.

use serde_json::{Value, json};
use windows_reactor::*;

use crate::app::{Msg, Viewer};
use crate::kit::{self, J, act, caption, cmd, glyph, group, row, strong};

fn scroll(content: impl Into<View>) -> View {
    ScrollViewer::new().horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled).content(content).into()
}

/// The thumbnail's colour for a computer's system.
fn thumb(os: &str) -> Color {
    match os.to_lowercase().as_str() {
        "linux" => Color::rgb(0x1E, 0x2A, 0x44),
        "macos" | "mac" | "darwin" => Color::rgb(0x3A, 0x35, 0x50),
        "windows" => Color::rgb(0x24, 0x3B, 0x3A),
        _ => Color::rgb(0x33, 0x33, 0x33),
    }
}

// ---- The computers ----

pub fn computers(v: &Viewer, context: &ViewContext<Viewer>) -> View {
    let list = v.state.a("computers");
    let found = v.state.a("nearby");
    let settings = kit::square(glyph::SETTINGS, "Settings", Some(act(context, "settings", Value::Null))).tooltip("Settings");
    if list.is_empty() && found.is_empty() {
        let head = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((kit::title("Computers"), Border::new().grid_column(1).content(settings)));
        let empty = StackPanel::new()
            .spacing(12.0)
            .max_width(420.0)
            .vertical_alignment(VerticalAlignment::Center)
            .horizontal_alignment(HorizontalAlignment::Center)
            .children((
                kit::icon(glyph::DISPLAY, 40.0).foreground(ThemeBrush::SecondaryText),
                kit::subtitle("No computers yet").horizontal_alignment(HorizontalAlignment::Center),
                TextBlock::new()
                    .text("See and use another computer's screen from this one. Pair a Linux PC, a Mac or another Windows PC that runs the remoteman host.")
                    .text_wrapping(TextWrapping::Wrap)
                    .text_alignment(TextAlignment::Center)
                    .foreground(ThemeBrush::SecondaryText),
                kit::button("Pair a computer", act(context, "pair", Value::Null), true, Some(glyph::ADD)).horizontal_alignment(HorizontalAlignment::Center),
            ));
        return Grid::new()
            .children((
                Border::new().padding(Thickness::new(32.0, 12.0, 32.0, 0.0)).vertical_alignment(VerticalAlignment::Top).content(head),
                empty,
            ))
            .into();
    }
    let head = Grid::new()
        .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
        .column_spacing(8.0)
        .children((
            kit::title("Computers"),
            Border::new().grid_column(1).content(kit::button("Pair a computer", act(context, "pair", Value::Null), true, Some(glyph::ADD))),
            Border::new().grid_column(2).content(settings),
        ));
    let mut cards: Vec<View> = Vec::new();
    for (n, c) in list.iter().enumerate() {
        cards.push(card(c, context).grid_row((n / 3) as i32).grid_column((n % 3) as i32).into());
    }
    let rows = list.len().div_ceil(3).max(1);
    let cards = Grid::new()
        .columns([GridLength::STAR, GridLength::STAR, GridLength::STAR])
        .rows(vec![GridLength::Auto; rows])
        .column_spacing(12.0)
        .row_spacing(12.0)
        .children(cards);
    // Computers running remoteman on this network that this PC isn't paired with.
    let mut nearby: Vec<View> = Vec::new();
    for near in found {
        let address = near.s("address").to_owned();
        let about = [near.s("os_name"), "on this network", "not paired"].iter().filter(|x| !x.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
        let pair = Button::new()
            .content("Pair…")
            .automation_name(format!("Pair {}", near.s("name")))
            .on_click(act(context, "pair", json!({"address": address})))
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(2);
        nearby.push(
            Border::new()
                .padding(Thickness::new(16.0, 10.0, 16.0, 10.0))
                .min_height(60.0)
                .content(
                    Grid::new()
                        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                        .column_spacing(14.0)
                        .children((
                            kit::icon(glyph::DISPLAY, 16.0).vertical_alignment(VerticalAlignment::Center),
                            StackPanel::new().grid_column(1).vertical_alignment(VerticalAlignment::Center).children((TextBlock::new().text(near.s("name")), caption(&about))),
                            pair,
                        )),
                )
                .into(),
        );
    }
    if found.is_empty() {
        nearby.push(caption("None found. A computer shows here while its remoteman host is on and it is on this network.").margin(Thickness::new(16.0, 12.0, 16.0, 12.0)).into());
    }
    scroll(
        StackPanel::new()
            .spacing(20.0)
            .margin(Thickness::new(32.0, 12.0, 32.0, 28.0))
            .children((
                head,
                StackPanel::new().spacing(8.0).children((strong("Paired computers"), cards)),
                StackPanel::new().spacing(8.0).children((strong("Nearby"), kit::card(StackPanel::new().children(nearby), Thickness::uniform(0.0)))),
                caption("A computer that is not listed can be paired by its address."),
            )),
    )
}

fn card(c: &Value, context: &ViewContext<Viewer>) -> Border {
    let address = c.s("address").to_owned();
    let reach = c.s("reach");
    let tag = Border::new()
        .background(Color::argb(115, 0, 0, 0))
        .corner_radius(4.0)
        .padding(Thickness::new(8.0, 2.0, 8.0, 2.0))
        .horizontal_alignment(HorizontalAlignment::Left)
        .vertical_alignment(VerticalAlignment::Bottom)
        .content(TextBlock::new().text(c.s("os_name")).font_size(12.0).foreground(Color::rgb(255, 255, 255)));
    let thumbnail = Border::new().height(112.0).corner_radius(4.0).background(thumb(c.s("os"))).padding(10.0).content(tag);
    let state = match reach {
        "unreachable" => "Not reachable",
        "asleep" => "Asleep",
        "online" => "Online",
        _ => "",
    };
    let used = if c.s("last").is_empty() { "never used".to_owned() } else { format!("used {}", c.s("last")) };
    let line = if c.b("connected") {
        "Connected now".to_owned()
    } else if !state.is_empty() {
        format!("{state} · {used}")
    } else {
        let mut u = used.clone();
        u[..1].make_ascii_uppercase();
        u
    };
    let dot: Brush = match reach {
        "online" => ThemeBrush::SystemSuccess.into(),
        "unreachable" => ThemeBrush::SystemCritical.into(),
        "asleep" => ThemeBrush::SystemCaution.into(),
        _ => ThemeBrush::SecondaryText.into(),
    };
    let words = StackPanel::new().children((
        kit::strong(c.s("name")).text_trimming(TextTrimming::CharacterEllipsis),
        StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children((
            Ellipse::new().width(8.0).height(8.0).fill(dot).vertical_alignment(VerticalAlignment::Center),
            caption(&line),
        )),
    ));
    // One that didn't answer and can be sent the waking packet is offered that.
    let wake = reach == "unreachable" && c.b("wakeable");
    let connect = if wake {
        kit::button("Wake and connect", act(context, "wake", json!({"address": address})), false, None)
    } else {
        kit::button("Connect", act(context, "connect", json!({"address": address})), true, None)
    }
    .horizontal_alignment(HorizontalAlignment::Stretch);
    let key = |what: &str, more: Value| cmd(what, more).to_string();
    let menu = Menu::new(
        [
            MenuItem::item(key("computer", json!({"address": address})), "Settings for this computer…").glyph(glyph::SETTINGS),
            MenuItem::item(key("edit-address", json!({"address": address})), "Edit address…").glyph(glyph::EDIT),
            MenuItem::separator("sep"),
            MenuItem::item(key("forget", json!({"address": address})), "Forget…").glyph(glyph::DELETE),
        ],
        context.callback(Msg::Menu),
    );
    let more = kit::square(glyph::MORE, &format!("More for {}", c.s("name")), None).grid_column(1).menu(menu);
    let acts = Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((connect, more));
    kit::card(StackPanel::new().spacing(12.0).children((thumbnail, words, acts)), Thickness::uniform(12.0)).on_double_tapped(act(context, "connect", json!({"address": address})))
}

// ---- Connecting, and why there is no session ----

pub fn status(v: &Viewer, context: &ViewContext<Viewer>) -> View {
    let s = v.state.o("screen");
    let name = if s.s("name").is_empty() { s.s("address") } else { s.s("name") }.to_owned();
    let address = s.s("address").to_owned();
    let host = address.split(':').next().unwrap_or("").to_owned();
    let mut acts: Vec<View> = Vec::new();
    let mut add = |text: &str, on: Callback<()>, accent: bool| acts.push(kit::button(text, on, accent, None).into());
    let ring = || -> View { ProgressRing::new().is_active(true).width(40.0).height(40.0).into() };
    let sign = |g: &str, brush: Option<ThemeBrush>| -> View {
        let i = kit::icon(g, 40.0).horizontal_alignment(HorizontalAlignment::Center);
        match brush {
            Some(b) => i.foreground(b).into(),
            None => i.foreground(ThemeBrush::SecondaryText).into(),
        }
    };
    let (top, title, words): (View, String, String);
    let mut locked: Option<View> = None;
    if s.s("kind") == "connecting" {
        let m = v.state.a("monitor");
        top = ring();
        title = format!("Connecting to {name}");
        words = if m.len() == 3 { format!("Asking {name} for a {} × {} display at {} Hz…", m[0], m[1], m[2]) } else { String::new() };
        add("Cancel", act(context, "cancel-connect", Value::Null), false);
    } else {
        let p = s.o("problem");
        let said = p.s("said").to_owned();
        let connect = || act(context, "connect", json!({"address": address}));
        match p.s("kind") {
            "unreachable" => {
                top = sign(glyph::UNREACHABLE, Some(ThemeBrush::SystemCritical));
                title = format!("Can't reach {name}");
                words = format!("{said} at {host}. It may be off, on another network, or its firewall may be closed to remoteman.");
                add("Try again", connect(), true);
                if v.state.a("computers").iter().any(|c| c.s("address") == address && c.b("wakeable")) {
                    add("Wake and connect", act(context, "wake", json!({"address": address})), false);
                }
                add("Edit address…", act(context, "edit-address", json!({"address": address})), false);
                add("Back", act(context, "back", Value::Null), false);
            }
            "asleep" => {
                top = sign(glyph::ASLEEP, Some(ThemeBrush::SystemCaution));
                title = format!("{name} is asleep");
                words = "Waking it over the network takes a few seconds.".into();
                add("Wake and connect", act(context, "wake", json!({"address": address})), true);
                add("Back", act(context, "back", Value::Null), false);
            }
            "not-paired" => {
                top = sign(glyph::LOCK, Some(ThemeBrush::SystemCaution));
                title = format!("{name} doesn't know this PC");
                words = "It answered, but this PC is no longer on its list of paired devices. Put it in pairing mode and pair again.".into();
                add("Pair again…", act(context, "pair", json!({"address": address})), true);
                add("Back", act(context, "back", Value::Null), false);
            }
            "changed" => {
                top = sign(glyph::WARNING, Some(ThemeBrush::SystemCritical));
                title = format!("This isn't the {name} this PC paired with");
                words = format!("Something else answered at {host}, with another key. If {name} was set up again, forget it and pair it again.");
                add("Forget…", act(context, "forget", json!({"address": address})), false);
                add("Back", act(context, "back", Value::Null), true);
            }
            "refused" => {
                top = sign(glyph::CLOSE, Some(ThemeBrush::SystemCritical));
                title = format!("{name} closed the connection");
                words = format!("It answered, and said: {said}");
                add("Connect again", connect(), true);
                add("Back", act(context, "back", Value::Null), false);
            }
            "logging-in" => {
                top = ring();
                title = format!("{name} is logging in");
                words = if said.is_empty() { "This PC tries again by itself.".into() } else { format!("It said: {said}. This PC tries again by itself.") };
                add("Try now", connect(), true);
                add("Back", act(context, "back", Value::Null), false);
            }
            "locked" => {
                top = sign(glyph::LOCK, None);
                title = format!("{name} is locked");
                words = said.clone();
                // Enter in the box unlocks too.
                let enter = context.sender();
                locked = Some(
                    Border::new()
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .on_preview_key_down(RoutedCallback::new(move |k: KeyEventInfo| {
                            if k.key == VirtualKey::ENTER {
                                let _ = enter.send(Msg::Unlock);
                                return true;
                            }
                            false
                        }))
                        .content(StackPanel::new().spacing(8.0).children((
                            PasswordBox::new().width(300.0).placeholder_text("Password").automation_name("Password").password(v.password.as_str()).on_password_changed(context.callback(Msg::Password)),
                            CheckBox::new().content("Remember it on this PC").is_checked(v.remember.unwrap_or_else(|| v.state.b("remember"))).on_click(context.message(Msg::Remember)),
                        )))
                        .into(),
                );
                add("Unlock", context.message(Msg::Unlock), true);
                add("Back", act(context, "back", Value::Null), false);
            }
            _ => {
                top = sign(glyph::CLOSE, None);
                title = format!("The session with {name} ended");
                words = said.clone();
                add("Connect again", connect(), true);
                add("Back", act(context, "back", Value::Null), false);
            }
        }
    }
    let mut col: Vec<View> = vec![
        top,
        kit::subtitle(&title).text_alignment(TextAlignment::Center).text_wrapping(TextWrapping::Wrap).horizontal_alignment(HorizontalAlignment::Center).into(),
    ];
    if !words.is_empty() {
        col.push(TextBlock::new().text(words).text_alignment(TextAlignment::Center).text_wrapping(TextWrapping::Wrap).foreground(ThemeBrush::SecondaryText).max_width(460.0).horizontal_alignment(HorizontalAlignment::Center).into());
    }
    if let Some(l) = locked {
        col.push(l);
    }
    col.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).horizontal_alignment(HorizontalAlignment::Center).children(acts).into());
    StackPanel::new().spacing(16.0).max_width(520.0).vertical_alignment(VerticalAlignment::Center).horizontal_alignment(HorizontalAlignment::Center).children(col).into()
}

// ---- Settings ----

fn toggle(on: bool, name: &str, flip: Callback<bool>) -> View {
    ToggleSwitch::new().is_on(on).on_content("").off_content("").min_width(0.0).automation_name(name).on_toggled(flip).into()
}

/// A drop-down of the engine's options: `{"items": [{"text", "on"}], "at"}`.
fn choice(options: &Value, name: &str, chose: Callback<Option<usize>>) -> View {
    let items: Vec<String> = options.a("items").iter().map(|i| i.s("text").to_owned()).collect();
    ComboBox::new().min_width(200.0).items_source(items).selected_index(options.n("at") as usize).automation_name(name).on_selection_changed(chose).into()
}

/// A change of a drop-down, as the engine's `choose`.
fn chose(context: &ViewContext<Viewer>, choice: &'static str, address: Option<String>) -> Callback<Option<usize>> {
    context.callback(move |index: Option<usize>| match index {
        Some(index) => Msg::Do(cmd("choose", json!({"choice": choice, "index": index, "address": address}))),
        None => Msg::Do(Value::Null),
    })
}

fn flip(context: &ViewContext<Viewer>, v: Value) -> Callback<bool> {
    context.callback(move |_: bool| Msg::Do(v.clone()))
}

fn rows_page(head: View, groups: Vec<View>) -> View {
    let mut col = vec![head];
    col.extend(groups);
    scroll(StackPanel::new().spacing(18.0).margin(Thickness::new(40.0, 8.0, 40.0, 28.0)).max_width(1000.0).children(col))
}

pub fn settings(v: &Viewer, context: &ViewContext<Viewer>) -> View {
    let s = v.state.o("settings");
    let f = v.state.o("features");
    let flag = |name: &str| flip(context, cmd("toggle", json!({"flag": name})));
    let set = |what: &str| flip(context, cmd("set", json!({"what": what})));
    let head = StackPanel::new().children((kit::title("Settings"), caption("For every computer. One computer can differ: open its own settings from the list."))).into();
    let groups = vec![
        group(
            "Session window",
            vec![
                row("Controls", "A title bar, or nothing but the picture with a bar that appears at the top edge", Some(choice(s.o("controls"), "Controls", chose(context, "controls", None))), true, None),
                row("Start in full screen", "", Some(toggle(s.b("fullscreen"), "Start in full screen", flip(context, cmd("start-fullscreen", Value::Null)))), true, None),
            ],
        ),
        group(
            "Picture",
            vec![
                row("Default quality", "Automatic picks the best stream this PC decodes on its graphics card", Some(choice(s.o("default_quality"), "Default quality", chose(context, "default-quality", None))), true, None),
                row("Match this monitor's refresh rate", "Otherwise 60 Hz", Some(toggle(s.b("match_refresh"), "Match this monitor's refresh rate", flag("match-refresh"))), f.b("refresh_choice"), None),
                row("HDR", "Used when the computer offers it and this monitor is in HDR", Some(toggle(s.b("hdr"), "HDR", flag("hdr"))), f.b("hdr"), None),
            ],
        ),
        group(
            "Keyboard",
            vec![
                row("Send system shortcuts to the computer", "Alt+Tab, the Windows key and others, while a session is in front. Ctrl+Alt+Delete is always in Send keys.", Some(toggle(f.b("system_shortcuts") && s.b("shortcuts"), "Send system shortcuts to the computer", flag("shortcuts"))), f.b("system_shortcuts"), None),
                row("Release the keyboard", "Gives shortcuts back to this PC, and takes them again", Some(kit::fixed("Ctrl+Alt+Shift")), f.b("release_shortcut"), None),
                row("On a Mac, Ctrl acts as Command", "Ctrl+C on this keyboard copies on a Mac, as it does here", Some(toggle(s.b("ctrl_as_command"), "On a Mac, Ctrl acts as Command", flag("ctrl-as-command"))), f.b("ctrl_as_command"), None),
                row("Windows key", "Opens the overview on a Linux computer, Spotlight's key on a Mac", Some(kit::fixed("Super / Command")), f.b("key_mapping"), None),
            ],
        ),
        group(
            "Pointer and touch",
            vec![
                row("Pointer", "Captured sends raw movement, which games and 3D apps need. Ctrl+Alt+Shift lets it go", Some(choice(s.o("pointer"), "Pointer", chose(context, "pointer", None))), f.b("pointer_capture"), None),
                row("Scroll direction", "", Some(choice(s.o("scroll"), "Scroll direction", chose(context, "scroll", None))), f.b("scroll_direction"), None),
                row("Send touchscreen touches as touch", "Fingers arrive as fingers, so gestures on the computer work", Some(toggle(s.b("touch"), "Send touchscreen touches as touch", set("touch"))), f.b("touch"), None),
            ],
        ),
        group(
            "Sound, microphone and camera",
            vec![
                row("Play the computer's sound here", "For computers paired from now on; each has its own switch", Some(toggle(s.b("sound"), "Play the computer's sound here", flag("sound"))), f.b("sound"), None),
                row("Let computers use this microphone", "Still switched on by you in each session", Some(toggle(s.b("allow_microphone"), "Let computers use this microphone", set("allow-microphone"))), f.b("microphone"), None),
                row("Let computers use this camera", "For calls made on the other computer", Some(toggle(s.b("allow_camera"), "Let computers use this camera", set("allow-camera"))), f.b("camera"), None),
            ],
        ),
        group(
            "Sharing",
            vec![
                row("Share the clipboard", "Text and images copied on either side can be pasted on the other. For computers paired from now on", Some(toggle(s.b("clipboard"), "Share the clipboard", flag("clipboard"))), f.b("clipboard"), None),
                row("Save received files to", s.s("folder"), Some(Button::new().content("Change…").automation_name("Save received files to").on_click(act(context, "pick-folder", Value::Null)).into()), f.b("folder_choice"), None),
            ],
        ),
        HyperlinkButton::new().content("remoteman 0.1 · About").horizontal_alignment(HorizontalAlignment::Left).on_click(act(context, "about", Value::Null)).into(),
    ];
    rows_page(head, groups)
}

// ---- One computer ----

pub fn computer(v: &Viewer, context: &ViewContext<Viewer>) -> View {
    let address = v.state.o("screen").s("address").to_owned();
    let Some(c) = v.state.a("computers").iter().find(|c| c.s("address") == address) else {
        return Grid::new().into();
    };
    let opts = v.state.o("computer");
    let f = v.state.o("features");
    let folder = v.state.o("settings").s("folder");
    let a = Some(address.clone());
    let head = StackPanel::new()
        .children((
            HyperlinkButton::new().content(TextBlock::new().text("Computers ›").font_size(12.0)).on_click(act(context, "back", Value::Null)),
            kit::title(c.s("name")),
            caption(&format!("{} · paired {} · last used {}", c.s("os_name"), c.s("paired"), c.s("last"))),
        ))
        .into();
    let typed = v.name_text.as_ref().filter(|(at, _)| *at == address).map(|(_, t)| t.clone()).unwrap_or_else(|| c.s("name").to_owned());
    let commit = context.sender();
    let name_box = Border::new()
        .on_lost_focus(context.callback(|_| Msg::NameCommit))
        .on_preview_key_down(RoutedCallback::new(move |k: KeyEventInfo| {
            if k.key == VirtualKey::ENTER {
                let _ = commit.send(Msg::NameCommit);
                return true;
            }
            false
        }))
        .content(TextBox::new().text(typed).width(260.0).automation_name("Name").on_text_changed(context.callback(Msg::NameText)))
        .into();
    let sits = match c.s("placement") {
        "main" => "Its own displays stay on as secondary ones",
        "only" => "Its own screens turn off while you are connected",
        "mirror" => "Shows one of its own monitors as it is",
        _ => "Beside the computer's own displays, which stay on",
    };
    let forget = Button::new()
        .content(TextBlock::new().text("Forget…").foreground(ThemeBrush::SystemCritical))
        .automation_name("Forget this computer")
        .on_click(act(context, "forget", json!({"address": address})))
        .into();
    let key_row: View = Border::new()
        .padding(Thickness::new(16.0, 10.0, 16.0, 10.0))
        .min_height(60.0)
        .content(StackPanel::new().vertical_alignment(VerticalAlignment::Center).children((
            TextBlock::new().text("Its key"),
            caption(&format!("{} · the same is shown on {}", c.s("key"), c.s("name"))).font_family("Consolas"),
        )))
        .into();
    let groups = vec![
        group(
            "Connection",
            vec![
                row("Name", "", Some(name_box), true, None),
                row("Address", c.s("short"), Some(Button::new().content("Edit address…").automation_name("Edit address…").on_click(act(context, "edit-address", json!({"address": address}))).into()), true, None),
                row("Wake it over the network", "When it is asleep and on the same network", Some(toggle(c.b("wake"), "Wake it over the network", flip(context, cmd("set", json!({"what": "wake", "address": address}))))), f.b("wake"), None),
                row("Getting in", "Joins your session there if you are logged in, otherwise shows its login screen", Some(kit::fixed("The computer decides")), f.b("way_in"), None),
            ],
        ),
        group(
            "Display",
            vec![
                row("Where this display sits", sits, Some(choice(opts.o("placement"), "Where this display sits", chose(context, "placement", a.clone()))), true, None),
                row("Picture size", "How many pixels to ask the computer for", Some(choice(opts.o("size"), "Picture size", chose(context, "size", a.clone()))), f.b("follow_window"), None),
                row("A second display goes", "Where the computer puts a second display you add, among its other displays", Some(choice(opts.o("side"), "A second display goes", chose(context, "side", a.clone()))), f.b("second_display"), None),
                row("Picture quality", "", Some(choice(opts.o("quality"), "Picture quality", chose(context, "quality", a.clone()))), true, None),
                row("Bitrate limit", "", Some(choice(opts.o("bitrate"), "Bitrate limit", chose(context, "bitrate", a.clone()))), true, None),
            ],
        ),
        group(
            "Sound and sharing",
            vec![
                row("Play its sound here", "When a session starts; the bar's button is for the session", Some(toggle(c.b("sound"), "Play its sound here", flip(context, cmd("toggle", json!({"flag": "sound", "address": address}))))), f.b("sound"), None),
                row("Share the clipboard with it", "When a session starts, if the computer allows it", Some(toggle(c.b("clipboard"), "Share the clipboard with it", flip(context, cmd("toggle", json!({"flag": "clipboard", "address": address}))))), f.b("clipboard"), None),
                row("Files it sends", &format!("Go into {folder}, never over a file that is there"), None, f.b("files"), None),
            ],
        ),
        group("Pairing", vec![key_row, row("Forget this computer", "It has to be paired again before this PC can connect", Some(forget), true, None)]),
    ];
    rows_page(head, groups)
}
