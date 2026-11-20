//! The window: the title bar, the navigation (Sharing, Devices, About), the page, a line of news
//! at the bottom, and the dialogs (the pairing question, Revoke…). Built from WinUI's own
//! controls through Reactor, with the design's words (docs/design/windows/Host*.dc.html).
//!
//! The window holds no state of the app's: closing it drops this component, and the app (in
//! app.rs) keeps asking the host, keeps the icon, and opens a new window when it is wanted. What
//! the window shows comes in as its input; what the owner does goes back through callbacks.

use std::cell::Cell;
use std::rc::Rc;

use windows_reactor::*;

use crate::app::Page;
use crate::host::{self, Device, Pending, Status, Unreached};

/// The mark from the design boards (docs/design/windows/Host*.dc.html).
const LOGO_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect x=".5" y=".5" width="15" height="15" rx="3.5" fill="#0B1437" stroke="#2458F5"/><rect x="3" y="5" width="7" height="5" rx="1.2" fill="#2458F5"/><path d="M11.5 6h2M11.5 7.8h2.5M11.5 9.5h1.5" stroke="#fff" stroke-width="1.1" stroke-linecap="round"/></svg>"##;

thread_local! {
    /// The window's handle while it is open, for the file dialog to belong to.
    pub static HWND: Cell<isize> = const { Cell::new(0) };
}

/// What the window shows, and where what the owner does goes.
#[derive(Clone, PartialEq)]
pub struct Shown {
    pub status: Result<Status, Unreached>,
    /// This PC's name, as devices are told it.
    pub pc: String,
    pub port: u16,
    pub page: Page,
    pub news: Option<String>,
    /// The device whose pairing question is showing.
    pub asking: Option<Pending>,
    /// The second factor being turned on (the secret to show, and the first code typed).
    pub second_factor: Option<SecondFactorSetup>,
    pub theme: WindowTheme,
    pub icon: Rc<str>,
    pub on: Callbacks,
}

#[derive(Clone, PartialEq)]
pub struct Callbacks {
    pub page: Callback<Option<Rc<str>>>,
    /// Words for the host, as they are.
    pub tell: Callback<String>,
    pub share: Callback<bool>,
    pub clipboard: Callback<bool>,
    pub files: Callback<bool>,
    pub lock_on_leave: Callback<bool>,
    pub speakers: Callback<bool>,
    pub second_device: Callback<Option<usize>>,
    pub notify: Callback<bool>,
    pub second_factor: Callback<bool>,
    pub second_factor_code: Callback<Rc<str>>,
    pub second_factor_confirm: Callback<()>,
    /// The pairing question was answered (or taken away).
    pub answer: Callback<ContentDialogResult>,
    pub news_closed: Callback<()>,
    pub send_file: Callback<()>,
}

/// What the owner needs to put into an authenticator app, and the code typed to confirm it.
#[derive(Clone, PartialEq)]
pub struct SecondFactorSetup {
    /// The key in groups of four, for typing in (no QR code: the app has no encoder for one).
    pub grouped: String,
    /// The `otpauth://` link; some apps open it from the clipboard.
    pub uri: String,
    pub code: String,
}

pub struct HostWindow {
    /// The device whose Revoke… dialog is showing.
    revoking: Option<Device>,
    hwnd: isize,
}

#[derive(Clone)]
pub enum Message {
    Hwnd(isize),
    Revoke(Device),
    Revoked(ContentDialogResult),
}

impl Component for HostWindow {
    type Input = Shown;
    type Message = Message;

    fn create(_: &Shown, context: &ComponentContext<Self>) -> Self {
        let _ = context.run_window(|window| Message::Hwnd(window.as_raw() as isize));
        crate::log::say("window: open");
        Self { revoking: None, hwnd: 0 }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::Hwnd(hwnd) => {
                self.hwnd = hwnd;
                HWND.with(|h| h.set(hwnd));
            }
            Message::Revoke(device) => self.revoking = Some(device),
            Message::Revoked(result) => {
                if let Some(device) = self.revoking.take()
                    && result == ContentDialogResult::Primary
                {
                    context.input().on.tell.call(format!("revoke {}", device.fingerprint));
                }
            }
        }
    }

    fn view(&self, input: &Shown, context: &mut ViewContext<Self>) -> View {
        context.window_title("Remoteman Host");
        context.window_visuals(WindowVisuals::new().client_size(900.0, 680.0).backdrop(WindowBackdrop::Mica).theme(input.theme).icon(input.icon.clone()));
        let title_bar = TitleBar::new().title("remoteman host").icon(Icon::image_svg(EncodedImage::new(LOGO_SVG.to_vec()))).grid_row(0);

        let s = input.status.as_ref().ok();
        let mut page: Vec<View> = Vec::new();
        match input.page {
            Page::Sharing => self.sharing(input, s, &mut page),
            Page::Devices => self.devices(input, s, &mut page, context),
            Page::About => self.about(input, s, &mut page),
        }
        let body = Grid::new()
            .children((
                ScrollViewer::new().content(
                    Border::new()
                        .padding(Thickness::new(32.0, 12.0, 32.0, 24.0))
                        .content(StackPanel::new().spacing(4.0).max_width(880.0).horizontal_alignment(HorizontalAlignment::Stretch).children(page)),
                ),
                InfoBar::new()
                    .is_open(input.news.is_some())
                    .is_closable(true)
                    .severity(InfoBarSeverity::Informational)
                    .message(input.news.clone().unwrap_or_default())
                    .vertical_alignment(VerticalAlignment::Bottom)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .margin(16.0)
                    .max_width(560.0)
                    .on_closed(input.on.news_closed.clone()),
            ));
        // The dialogs are there only while they are open, one at a time: the pairing question
        // goes first. Closing the window with one open doesn't answer it (the app asks again in
        // the next window, until the host's minute is up).
        let revoking = self.revoking.is_some() && input.asking.is_none();
        let body: View = if revoking { body.content_dialog(self.revoke_dialog(context)) } else { body.into() };

        let item = |page: Page, title: &str, symbol: Symbol| NavigationViewItem::new().tag(page.tag()).content(title).icon(Icon::symbol(symbol)).is_selected(input.page == page);
        let nav = NavigationView::new()
            .pane_display_mode(NavigationViewPaneDisplayMode::Left)
            .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
            .is_settings_visible(false)
            .is_pane_toggle_button_visible(false)
            .open_pane_length(200.0)
            .menu_items((item(Page::Sharing, "Sharing", Symbol::Share), item(Page::Devices, "Devices", Symbol::AllApps), item(Page::About, "About", Symbol::Help)))
            .on_selected_tag_changed(input.on.page.clone())
            .content(body)
            .grid_row(1);
        let nav: View = match &input.asking {
            Some(pending) => nav.content_dialog(pair_dialog(pending, input.on.answer.clone())),
            None => nav.into(),
        };

        Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((title_bar, nav)).into()
    }
}

impl Drop for HostWindow {
    fn drop(&mut self) {
        HWND.with(|h| {
            if h.get() == self.hwnd {
                h.set(0);
            }
        });
        crate::log::say("window: closed (the icon stays)");
    }
}

// ---- The pages ----

impl HostWindow {
    fn sharing(&self, input: &Shown, s: Option<&Status>, page: &mut Vec<View>) {
        page.push(title("Sharing"));
        let Some(s) = s else {
            page.push(match input.status {
                Err(Unreached::Refused) => row("Sharing couldn't be read", Some("The host (remoteman-host.exe) is running but didn't let this app ask it how it stands, so nothing here can be shown or changed."), None),
                _ => row("Sharing couldn't start", Some("The host (remoteman-host.exe) isn't running, so devices can't connect. It runs as the service \"remoteman host\", which starts with Windows; start it in Services, or restart the PC, and this page follows."), None),
            });
            return;
        };
        let on = !s.paused;
        let words = if on { format!("Sharing {}. Paired devices can connect while it is on.", input.pc) } else { "Off: no device can connect.".to_string() };
        page.push(row("Share this PC", Some(&words), Some(switch(on, true, "Share this PC", input.on.share.clone()))));
        let (state, lines) = s.words(&input.pc);
        let mut buttons: Vec<View> = Vec::new();
        if s.client.is_some() {
            if s.files {
                buttons.push(push("Send a file…", input.on.send_file.clone(), false).into());
            }
            buttons.push(push("Disconnect", tell(input, "drop"), false).into());
        }
        let right = (!buttons.is_empty()).then(|| StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(buttons).into());
        page.push(row(&state, Some(&lines.join("\n")), right));
        if on {
            if s.pairing {
                page.push(row("Pairing is open", Some("It accepts one new device, then closes. You are asked here before it is let in."), Some(push("Cancel", tell(input, "pair off"), false).into())));
            } else {
                page.push(row("Pair a device", Some("On the device, choose Pair a computer and pick this PC, or enter its address. You compare a six-digit code here."), Some(push("Pair a device…", tell(input, "pair on"), true).into())));
            }
        }
        page.push(heading("Getting in"));
        page.push(row(
            "Locked, or nobody signed in",
            Some("The host runs from the moment Windows starts. A paired device that comes while this PC is locked, or before anyone has signed in, is asked for the password or PIN, which the host types into Windows' own screen; it locks again when the device leaves."),
            Some(TextBlock::new().text("On").into()),
        ));
        page.push(heading("Picture"));
        page.push(row("Encoder", Some("On the graphics card: AV1 10-bit and HEVC 10-bit; HEVC 4:4:4 and HDR where the card's NVENC has them"), Some(TextBlock::new().text("Automatic").into())));
    }

    fn devices(&self, input: &Shown, s: Option<&Status>, page: &mut Vec<View>, context: &ViewContext<Self>) {
        page.push(title("Devices"));
        let live = s.is_some_and(|s| !s.paused);
        if s.is_some_and(|s| s.pairing) {
            page.push(row("Pairing is open", Some("It accepts one new device, then closes. The device's code is shown here for you to compare."), Some(push("Cancel", tell(input, "pair off"), false).into())));
        } else {
            let pair = push("Pair a device…", tell(input, "pair on"), true).is_enabled(live);
            page.push(row("Pair a device", Some(if live { "Open pairing for one new device." } else { "Shown while sharing is on" }), Some(pair.into())));
        }

        page.push(heading("Paired devices"));
        match s {
            None => page.push(row("Shown while the host is running", None, None)),
            Some(s) if s.devices.is_empty() => page.push(row("No device is paired yet", None, None)),
            Some(s) => {
                for d in &s.devices {
                    let mut actions: Vec<View> = Vec::new();
                    if d.connected {
                        actions.push(push("Disconnect", tell(input, "drop"), false).into());
                    }
                    let device = d.clone();
                    actions.push(push("Revoke…", context.callback(move |()| Message::Revoke(device.clone())), false).automation_name(format!("Revoke {}", d.name)).into());
                    let state = if !d.permissions.input { "View only" } else if d.connected { "Connected now" } else { "Paired" };
                    let state = if d.connected && !d.permissions.input { "Connected now · view only".to_string() } else { state.to_string() };
                    let state = if d.joined { format!("{state} · joined") } else { state };
                    page.push(row(&d.name, Some(&state), Some(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(actions).into())));
                    page.push(permissions_card(input, d));
                }
            }
        }
        page.push(caption("A revoked device has to be paired again before it can connect.").margin(Thickness::new(1.0, 4.0, 0.0, 0.0)).into());

        page.push(heading("Security"));
        let on = s.is_some_and(|s| s.second_factor.is_some());
        page.push(row(
            "Ask for a code from an authenticator app",
            Some("Besides its key, a paired device has to give the six-digit code from an app such as Microsoft Authenticator or Google Authenticator"),
            Some(switch(on || input.second_factor.is_some(), s.is_some(), "Ask for a code from an authenticator app", input.on.second_factor.clone())),
        ));
        if let Some(setup) = &input.second_factor {
            page.push(second_factor_card(input, setup));
        }
        if let Some(remembered) = s.and_then(|s| s.second_factor) {
            let words = if remembered == 0 { "No device has been told not to ask again".to_string() } else { format!("{remembered} device(s) are not asked for a code for 30 days") };
            page.push(row("Forget remembered devices", Some(&words), Some(push("Forget", tell(input, "second-factor forget"), false).is_enabled(remembered > 0).into())));
        }

        page.push(heading("Clipboard and files"));
        page.push(row(
            "Share the clipboard with devices",
            Some("Text and pictures copied on either side can be pasted on the other"),
            Some(switch(s.is_some_and(|s| s.clipboard), s.is_some(), "Share the clipboard with devices", input.on.clipboard.clone())),
        ));
        page.push(row("Receive files from devices", Some("Into Downloads, never over an existing file"), Some(switch(s.is_some_and(|s| s.files), s.is_some(), "Receive files from devices", input.on.files.clone()))));

        page.push(heading("While a device is connected"));
        let second = match s.map(|s| s.second_device.as_str()) {
            Some("watch") => 1,
            Some("control") => 2,
            _ => 0,
        };
        page.push(row(
            "When a second device connects",
            Some("While one device is using this PC. A device that watches or controls too sees the same screen; when the first leaves, the second's session ends with it"),
            Some(ComboBox::new().min_width(200.0).items_source(["It takes over", "It watches", "It can control too"]).selected_index(second).is_enabled(s.is_some()).automation_name("When a second device connects").on_selection_changed(input.on.second_device.clone()).into()),
        ));
        page.push(row(
            "Lock this PC when a device disconnects",
            Some("The PC is kept awake while a device is connected, and locked once the last one leaves"),
            Some(switch(s.is_some_and(|s| s.lock_on_leave), s.is_some(), "Lock this PC when a device disconnects", input.on.lock_on_leave.clone())),
        ));
        page.push(row(
            "Silence this PC's speakers while a device listens",
            Some("The PC's output is muted meanwhile; the sound still goes to the device"),
            Some(switch(s.is_some_and(|s| s.speakers_follow), s.is_some(), "Silence this PC's speakers while a device listens", input.on.speakers.clone())),
        ));

        page.push(row(
            "Tell me when a device connects",
            Some("A notification when a device connects, is paired, or is turned away"),
            Some(switch(s.is_some_and(|s| s.notify), s.is_some(), "Tell me when a device connects", input.on.notify.clone())),
        ));

        page.push(heading("Recent connections"));
        match s {
            Some(s) if !s.connections.is_empty() => {
                for c in s.connections.iter().rev().take(10) {
                    let detail = if c.reason.is_empty() { format!("{} · {}", c.when(), c.address) } else { format!("{} · {} · {}", c.when(), c.address, c.reason) };
                    page.push(row(&c.words(), Some(&detail), None));
                }
            }
            Some(_) => page.push(row("Nobody has connected yet", None, None)),
            None => page.push(row("Shown while the host is running", None, None)),
        }

        page.push(heading("Address and key"));
        let reach = s.map(|s| crate::system::reach(&s.address));
        page.push(row("Address", Some(reach.as_deref().unwrap_or("Shown while the host is running")), None));
        let key = s.map(|s| format!("{}\nA device shows the same fingerprint once it is paired", host::short_key(&s.fingerprint)));
        page.push(row("Key fingerprint", Some(key.as_deref().unwrap_or("Shown while the host is running")), None));
    }

    fn about(&self, input: &Shown, s: Option<&Status>, page: &mut Vec<View>) {
        page.push(title("About"));
        page.push(row("remoteman host", Some("This app shows and changes what the host on this PC does. The host (remoteman-host.exe) runs on its own: closing this window, or quitting, leaves it sharing as it is."), None));
        let host = match (s, &input.status) {
            (Some(s), _) => format!("Running, listening on {}", s.address),
            (None, Err(Unreached::Refused)) => format!("Running on port {}, but its pipe refused this user", input.port),
            (None, _) => format!("Not running (nothing answers on port {})", input.port),
        };
        page.push(row("Host", Some(&host), None));
        page.push(row(
            "Microphone of a device",
            Some(if s.is_some_and(|s| s.microphone) { "Played into Steam Streaming Microphone" } else { "Needs Steam's Streaming Microphone, which isn't installed" }),
            None,
        ));
    }

    fn revoke_dialog(&self, context: &ViewContext<Self>) -> ContentDialog {
        let name = self.revoking.as_ref().map(|d| d.name.as_str()).unwrap_or_default();
        ContentDialog::new()
            .title(format!("Revoke {name}?"))
            .content(TextBlock::new().text("It is disconnected if it is connected now, and has to be paired again before it can connect.").text_wrapping(TextWrapping::Wrap))
            .primary_button_text("Revoke")
            .close_button_text("Cancel")
            .default_button(ContentDialogButton::Close)
            .is_open(true)
            .on_closed(context.callback(Message::Revoked))
    }
}

/// The question put to the owner when a new device wants to pair.
fn pair_dialog(pending: &Pending, answer: Callback<ContentDialogResult>) -> ContentDialog {
    let (name, code, key) = (&pending.name, host::code(&pending.code), host::short_key(&pending.fingerprint));
    ContentDialog::new()
        .title(format!("Pair with {name}?"))
        .content(StackPanel::new().spacing(12.0).children((
            TextBlock::new().text(format!("It shows {code} if it is the device in front of you.")).text_wrapping(TextWrapping::Wrap),
            TextBlock::new().text(code.clone()).font_size(40.0).font_weight(FontWeight::SEMI_BOLD).character_spacing(120).horizontal_alignment(HorizontalAlignment::Center),
            caption(&format!("Its key: {key}")),
        )))
        .primary_button_text("Codes match")
        .close_button_text("Refuse")
        .default_button(ContentDialogButton::Close)
        .is_open(true)
        .on_closed(answer)
}

// ---- The pieces every page is made of ----

/// Words for the host from a button.
fn tell(input: &Shown, words: &'static str) -> Callback<()> {
    let tell = input.on.tell.clone();
    Callback::new(move |()| tell.call(words.to_string()))
}

/// A page's title: WinUI's Title style.
fn title(text: &str) -> View {
    TextBlock::new().text(text).font_family("Segoe UI Variable Display").font_size(28.0).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 8.0, 0.0, 8.0)).into()
}

/// A section's heading: WinUI's BodyStrong style.
fn heading(text: &str) -> View {
    TextBlock::new().text(text).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(1.0, 24.0, 0.0, 6.0)).into()
}

/// WinUI's Caption style in the secondary text colour.
fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(12.0).foreground(ThemeBrush::SecondaryText).text_wrapping(TextWrapping::Wrap)
}

/// A row: a title and a line under it on the left, something on the right, in a card (WinUI's
/// settings-row look: a thin layer over Mica, a hairline, rounded corners).
fn row(heading: &str, subtitle: Option<&str>, right: Option<View>) -> View {
    let mut words: Vec<View> = vec![TextBlock::new().text(heading).text_wrapping(TextWrapping::Wrap).into()];
    if let Some(subtitle) = subtitle.filter(|s| !s.is_empty()) {
        words.push(caption(subtitle).into());
    }
    let mut cells: Vec<View> = vec![StackPanel::new().vertical_alignment(VerticalAlignment::Center).children(words).into()];
    if let Some(right) = right {
        cells.push(Border::new().grid_column(1).vertical_alignment(VerticalAlignment::Center).content(right).into());
    }
    Border::new()
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .corner_radius(6.0)
        .padding(Thickness::new(16.0, 12.0, 16.0, 12.0))
        .margin(Thickness::new(0.0, 0.0, 0.0, 2.0))
        .min_height(64.0)
        .content(Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(16.0).children(cells))
        .into()
}

/// What one device may do: a switch for each, in a card under its row. A change is told to the
/// host in its words (`device FINGERPRINT view-only on`, `device FINGERPRINT allow files off`);
/// a switch that reports itself as toggled to what it already was says nothing.
fn permissions_card(input: &Shown, d: &Device) -> View {
    let p = d.permissions;
    let line = |label: &str, on: bool, words: String| -> View {
        let tell = input.on.tell.clone();
        let toggled = Callback::new(move |now: bool| {
            if now != on {
                tell.call(format!("{words} {}", if now { "on" } else { "off" }));
            }
        });
        let name = format!("{label} for {}", d.name);
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(16.0)
            .children((TextBlock::new().text(label).vertical_alignment(VerticalAlignment::Center), Border::new().grid_column(1).content(switch(on, true, &name, toggled))))
            .into()
    };
    let fp = &d.fingerprint;
    // "View only" is the opposite of the device's right to control this PC.
    let view_only = {
        let tell = input.on.tell.clone();
        let words = format!("device {fp} view-only");
        let toggled = Callback::new(move |now: bool| {
            if now != !p.input {
                tell.call(format!("{words} {}", if now { "on" } else { "off" }));
            }
        });
        let name = format!("View only for {}", d.name);
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(16.0)
            .children((TextBlock::new().text("View only (it sees the screen but can't control this PC)").vertical_alignment(VerticalAlignment::Center), Border::new().grid_column(1).content(switch(!p.input, true, &name, toggled))))
            .into()
    };
    let lines: Vec<View> = vec![
        view_only,
        line("Clipboard", p.clipboard, format!("device {fp} allow clipboard")),
        line("Files", p.files, format!("device {fp} allow files")),
        line("Sound", p.sound, format!("device {fp} allow sound")),
        line("Microphone", p.mic, format!("device {fp} allow mic")),
        line("Camera", p.camera, format!("device {fp} allow camera")),
    ];
    Border::new()
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .corner_radius(6.0)
        .padding(Thickness::new(16.0, 8.0, 16.0, 8.0))
        .margin(Thickness::new(24.0, 0.0, 0.0, 8.0))
        .content(StackPanel::new().spacing(2.0).children(lines))
        .into()
}

/// Turning the second factor on: the key to put into the app, and the first code from it.
fn second_factor_card(input: &Shown, setup: &SecondFactorSetup) -> View {
    let ready = setup.code.trim().len() == 6;
    Border::new()
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(1.0)
        .corner_radius(6.0)
        .padding(Thickness::new(16.0, 12.0, 16.0, 12.0))
        .margin(Thickness::new(24.0, 0.0, 0.0, 8.0))
        .content(StackPanel::new().spacing(10.0).children((
            TextBlock::new().text("In the authenticator app, add an account and choose to enter a setup key. Give it this key (spaces don't matter; the account type is time-based):").text_wrapping(TextWrapping::Wrap),
            TextBlock::new().text(setup.grouped.clone()).font_family("Consolas").font_size(22.0).font_weight(FontWeight::SEMI_BOLD).is_text_selection_enabled(true),
            caption(&format!("Or, for an app that takes a link: {}", setup.uri)).is_text_selection_enabled(true),
            TextBox::new().header("The six-digit code the app shows now").placeholder_text("123456").width(220.0).text(setup.code.clone()).automation_name("Code from the authenticator app").on_text_changed(input.on.second_factor_code.clone()),
            StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((
                push("Turn on", input.on.second_factor_confirm.clone(), true).is_enabled(ready),
                push("Cancel", { let c = input.on.second_factor.clone(); Callback::new(move |()| c.call(false)) }, false),
            )),
            caption("Devices are asked for a code only after you have typed one that the app shows."),
        )))
        .into()
}

fn switch(on: bool, enabled: bool, name: &str, toggled: Callback<bool>) -> View {
    ToggleSwitch::new().is_on(on).is_enabled(enabled).on_content("On").off_content("Off").min_width(0.0).automation_name(name).on_toggled(toggled).into()
}

fn push(text: &str, clicked: Callback<()>, accent: bool) -> Button {
    let button = Button::new().content(text).on_click(clicked);
    if accent { button.style(ButtonStyle::Accent) } else { button }
}
