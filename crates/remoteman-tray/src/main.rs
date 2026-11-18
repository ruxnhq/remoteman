//! `remoteman-tray`: the Linux host's face for its owner.
//!
//!     remoteman-tray [--background] [--page sharing|devices|about] [--shot FILE.png [--after SECONDS]]
//!
//! An icon in the top bar (where the desktop shows such icons) with a menu that says how
//! things stand and holds the few things done often, and a window with the paired devices and
//! the switches. The host itself is a user service; this program starts and stops it and talks
//! to it (see host.rs), so the computer stays shared when this program is closed or fails, and
//! opening it never starts sharing by itself.
//!
//! `--background` starts with the icon only (for starting at login). `--shot` saves a picture
//! of the window and leaves, for checking it without a person; `REMOTEMAN_HOST_SOCKET` and
//! `REMOTEMAN_HOST_UNIT` point it at a host other than the everyday one.

mod host;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gtk::{glib, graphene};
use host::{Device, State};
use ksni::blocking::TrayMethods;

/// Where this program keeps its own two choices (the host's are the host's).
fn state_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state"));
    base.join("remoteman")
}

/// The entry that opens this program, icon only, when its owner signs in.
fn autostart() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("autostart/dev.remoteman.Host.desktop")
}

fn set_autostart(on: bool) -> std::io::Result<()> {
    let path = autostart();
    if !on {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let program = std::env::current_exe()?;
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("/")))?;
    std::fs::write(path, format!("[Desktop Entry]\nType=Application\nName=Remoteman Host\nComment=The icon in the top bar for this computer's sharing\nExec=\"{}\" --background\nIcon=dev.remoteman.Host\nX-GNOME-Autostart-enabled=true\n", program.display()))
}

/// Whether its owner wants to be told when a device connects. Off until asked for.
fn notifies() -> bool {
    state_dir().join("tray-notify").exists()
}

fn set_notifies(on: bool) -> std::io::Result<()> {
    let path = state_dir().join("tray-notify");
    if on {
        std::fs::create_dir_all(state_dir())?;
        std::fs::write(path, "on\n")
    } else {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// What the icon's menu asks of the program.
enum Cmd {
    Open,
    Pair,
    Disconnect,
    Pause,
    Resume,
    Quit,
}

/// The icon in the top bar and its menu. The desktop draws the menu, so it opens where the
/// icon is.
struct Icon {
    state: State,
    name: String,
    tx: mpsc::Sender<Cmd>,
}

/// What a state is called, and a line or two more: the menu's first lines and the window's
/// status row.
fn words(state: &State, name: &str) -> (String, Vec<String>) {
    match state {
        State::Sharing { client: Some(c), .. } => {
            let place = match c.placement.as_str() {
                "main" => "Main display",
                "only" => "Only display",
                _ => "Extra display",
            };
            (
                "A device is connected".into(),
                vec![
                    format!("{} · connected {}", c.name, host::ago(c.since)),
                    format!("{} × {} at {} Hz · {place}", c.width, c.height, c.refresh),
                    format!("{}{}", c.picture, if c.sound { " · sound on" } else { "" }),
                ],
            )
        }
        State::Sharing { pairing: true, .. } => ("Pairing is open".into(), vec!["The next new device to connect is paired, then pairing closes.".into()]),
        State::Sharing { .. } => ("No device is connected".into(), vec![format!("Paired devices can connect while {name} is on.")]),
        State::Paused => (format!("Not sharing {name}"), vec!["No device can connect until you turn it on. Paired devices stay paired.".into()]),
        State::Failed(why) => ("Sharing couldn't start".into(), vec![why.clone()]),
    }
}

impl ksni::Tray for Icon {
    fn id(&self) -> String {
        "dev.remoteman.Host".into()
    }

    fn title(&self) -> String {
        "remoteman".into()
    }

    /// One shape for each way things can stand: the desktop recolours these icons, so colour
    /// can't tell them apart.
    fn icon_name(&self) -> String {
        match &self.state {
            State::Sharing { client: Some(_), .. } => "dev.remoteman.Host-connected-symbolic",
            State::Sharing { .. } => "dev.remoteman.Host-symbolic",
            State::Paused => "dev.remoteman.Host-paused-symbolic",
            State::Failed(_) => "dev.remoteman.Host-failed-symbolic",
        }
        .into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let (title, lines) = words(&self.state, &self.name);
        ksni::ToolTip { title: format!("remoteman: {title}"), description: lines.join("\n"), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Cmd::Open);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let line = |text: String| MenuItem::Standard(StandardItem { label: text.replace('_', "__"), enabled: false, ..Default::default() });
        let act = |text: &str, cmd: fn() -> Cmd| MenuItem::Standard(StandardItem { label: text.into(), activate: Box::new(move |icon: &mut Self| drop(icon.tx.send(cmd()))), ..Default::default() });
        let (title, lines) = words(&self.state, &self.name);
        let mut menu = vec![line(title)];
        menu.extend(lines.into_iter().map(line));
        menu.push(MenuItem::Separator);
        let sharing = matches!(self.state, State::Sharing { .. });
        if matches!(self.state, State::Sharing { client: Some(_), .. }) {
            menu.push(act("Disconnect", || Cmd::Disconnect));
        }
        menu.push(if sharing { act("Pause Sharing", || Cmd::Pause) } else { act("Resume Sharing", || Cmd::Resume) });
        if sharing {
            menu.push(act("Pair a Device…", || Cmd::Pair));
        }
        menu.push(act("Settings…", || Cmd::Open));
        menu.push(MenuItem::Separator);
        // Said as what it does: with the icon gone nothing would show that the computer is
        // shared, so leaving ends the sharing too.
        menu.push(act(if sharing { "Stop Sharing and Quit" } else { "Quit" }, || Cmd::Quit));
        menu
    }
}

struct Ui {
    app: adw::Application,
    window: adw::ApplicationWindow,
    stack: adw::ViewStack,
    toasts: adw::ToastOverlay,
    name: String,
    share: adw::SwitchRow,
    status: adw::ActionRow,
    disconnect: gtk::Button,
    pairing: adw::Banner,
    pair: adw::ButtonRow,
    devices: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    clipboard: adw::SwitchRow,
    files: adw::SwitchRow,
    address: adw::ActionRow,
    key: adw::ActionRow,
    /// The widgets are being set from the host's state, not by the person.
    setting: Cell<bool>,
    shown: RefCell<Option<State>>,
    icon: RefCell<Option<ksni::blocking::Handle<Icon>>>,
}

impl Ui {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(text).use_markup(false).build());
    }

    /// Ask the running host for something and show the new state at once.
    fn tell(self: &Rc<Self>, line: &str) {
        match host::ask(line).as_deref() {
            Some("ok") => {}
            Some(other) => self.toast(other),
            None => self.toast("The host isn't running"),
        }
        self.refresh();
    }

    fn open(&self, page: &str) {
        self.stack.set_visible_child_name(page);
        self.window.present();
    }

    /// Read how things stand and show it, in the window and in the icon's menu.
    fn refresh(self: &Rc<Self>) {
        let state = host::state();
        if self.shown.borrow().as_ref() == Some(&state) {
            return;
        }
        // A device that wasn't connected a moment ago is now. Not at this program's start: a
        // device already connected then is no news.
        let device = |s: &State| if let State::Sharing { client: Some(c), .. } = s { Some((c.name.clone(), c.since)) } else { None };
        if let (Some(before), Some((name, since))) = (self.shown.borrow().as_ref(), device(&state)) {
            if device(before).is_none_or(|b| b.1 != since) && notifies() {
                let note = gtk::gio::Notification::new(&format!("{name} connected"));
                note.set_body(Some(&format!("It is using {} now.", self.name)));
                self.app.send_notification(Some("device"), &note);
                println!("notified: {name} connected");
            }
        }
        if let Some(icon) = self.icon.borrow().as_ref() {
            let for_icon = state.clone();
            icon.update(move |i| i.state = for_icon);
        }
        self.setting.set(true);
        let (title, lines) = words(&state, &self.name);
        self.status.set_title(&title);
        self.status.set_subtitle(&lines.join("\n"));
        let sharing = matches!(state, State::Sharing { .. });
        self.share.set_active(sharing);
        self.share.set_subtitle(&if sharing { format!("Sharing {}. Paired devices can connect while it is on.", self.name) } else { "Off: no device can connect.".to_owned() });
        self.disconnect.set_visible(matches!(state, State::Sharing { client: Some(_), .. }));
        for row in self.rows.borrow_mut().drain(..) {
            self.devices.remove(&row);
        }
        let none = |text: &str| adw::ActionRow::builder().title(text).sensitive(false).build();
        match &state {
            State::Sharing { fingerprint, port, pairing, clipboard, files, devices, .. } => {
                self.pairing.set_revealed(*pairing);
                self.pair.set_sensitive(!pairing);
                self.clipboard.set_sensitive(true);
                self.files.set_sensitive(true);
                self.clipboard.set_active(*clipboard);
                self.files.set_active(*files);
                let (name, ip) = host::reach(*port);
                self.address.set_subtitle(&if ip.is_empty() { name } else { format!("{name}  ·  also {ip}") });
                self.key.set_subtitle(&host::short_key(fingerprint));
                if devices.is_empty() {
                    let row = none("No device is paired yet");
                    self.devices.add(&row);
                    self.rows.borrow_mut().push(row.upcast());
                }
                for device in devices {
                    let row = self.device_row(device);
                    self.devices.add(&row);
                    self.rows.borrow_mut().push(row.upcast());
                }
            }
            _ => {
                self.pairing.set_revealed(false);
                self.pair.set_sensitive(false);
                self.clipboard.set_sensitive(false);
                self.files.set_sensitive(false);
                self.address.set_subtitle("Shown while sharing is on");
                self.key.set_subtitle("Shown while sharing is on");
                let row = none("Shown while sharing is on");
                self.devices.add(&row);
                self.rows.borrow_mut().push(row.upcast());
            }
        }
        self.setting.set(false);
        *self.shown.borrow_mut() = Some(state);
    }

    fn device_row(self: &Rc<Self>, device: &Device) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title(&device.name).subtitle(if device.connected { "Connected now" } else { "Paired" }).use_markup(false).build();
        if device.connected {
            let disconnect = gtk::Button::builder().label("Disconnect").valign(gtk::Align::Center).build();
            let ui = self.clone();
            disconnect.connect_clicked(move |_| ui.tell("drop"));
            row.add_suffix(&disconnect);
        }
        let revoke = gtk::Button::builder().label("Revoke…").valign(gtk::Align::Center).css_classes(["destructive-action"]).build();
        let (ui, device) = (self.clone(), device.clone());
        revoke.connect_clicked(move |_| {
            let alert = adw::AlertDialog::new(Some(&format!("Revoke {}?", device.name)), Some("It is disconnected if it is connected now, and has to be paired again before it can connect."));
            alert.add_responses(&[("cancel", "Cancel"), ("revoke", "Revoke")]);
            alert.set_response_appearance("revoke", adw::ResponseAppearance::Destructive);
            let (ui2, fingerprint) = (ui.clone(), device.fingerprint.clone());
            alert.connect_response(Some("revoke"), move |_, _| ui2.tell(&format!("revoke {fingerprint}")));
            alert.present(Some(&ui.window));
        });
        row.add_suffix(&revoke);
        row
    }

    /// Start or stop the host service, and wait a moment for it to say so.
    fn share(self: &Rc<Self>, on: bool) {
        if on { host::start() } else { host::stop() }
        let ui = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || ui.refresh());
    }

    fn run(self: &Rc<Self>, cmd: Cmd) {
        match cmd {
            Cmd::Open => self.open("sharing"),
            Cmd::Pair => {
                self.tell("pair on");
                self.open("devices");
            }
            Cmd::Disconnect => self.tell("drop"),
            Cmd::Pause => self.share(false),
            Cmd::Resume => self.share(true),
            Cmd::Quit => {
                host::stop();
                if let Some(icon) = self.icon.borrow_mut().take() {
                    icon.shutdown().wait();
                }
                self.app.quit();
            }
        }
    }
}

fn build(app: &adw::Application) -> Rc<Ui> {
    let name = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "this computer".into());

    // Sharing.
    let share = adw::SwitchRow::builder().title("Share This Computer").build();
    let status = adw::ActionRow::builder().title("").subtitle_lines(4).use_markup(false).build();
    let disconnect = gtk::Button::builder().label("Disconnect").valign(gtk::Align::Center).visible(false).build();
    status.add_suffix(&disconnect);
    let sharing_group = adw::PreferencesGroup::builder().title("Sharing").build();
    sharing_group.add(&share);
    sharing_group.add(&status);
    let later = adw::PreferencesGroup::new();
    let at_sign_in = adw::SwitchRow::builder().title("Open remoteman when I sign in").subtitle("The icon in the top bar. Sharing itself starts at sign-in once it has been turned on").active(autostart().exists()).build();
    let notify = adw::SwitchRow::builder().title("Notify me when a device connects").active(notifies()).build();
    later.add(&at_sign_in);
    later.add(&notify);
    let sharing_page = adw::PreferencesPage::new();
    sharing_page.add(&sharing_group);
    sharing_page.add(&later);

    // Devices.
    let pair = adw::ButtonRow::builder().title("Pair a Device…").build();
    let pair_group = adw::PreferencesGroup::new();
    pair_group.add(&pair);
    let devices = adw::PreferencesGroup::builder().title("Paired devices").description("A revoked device has to be paired again before it can connect.").build();
    let clipboard = adw::SwitchRow::builder().title("Share the clipboard with devices").subtitle("Text and pictures copied on either side can be pasted on the other").build();
    let files = adw::SwitchRow::builder().title("Receive files from devices").subtitle("Into Downloads, never over an existing file").build();
    let sharing_what = adw::PreferencesGroup::builder().title("Clipboard and files").build();
    sharing_what.add(&clipboard);
    sharing_what.add(&files);
    let address = adw::ActionRow::builder().title("Address").subtitle("").subtitle_selectable(true).css_classes(["property"]).build();
    let key = adw::ActionRow::builder().title("Key fingerprint").subtitle("").subtitle_selectable(true).css_classes(["property"]).build();
    let reach = adw::PreferencesGroup::builder().title("Address and key").description("What to enter on a device to reach this computer. A device shows the same fingerprint once it is paired.").build();
    reach.add(&address);
    reach.add(&key);
    let devices_page = adw::PreferencesPage::new();
    devices_page.add(&pair_group);
    devices_page.add(&devices);
    devices_page.add(&sharing_what);
    devices_page.add(&reach);

    // About.
    let about = adw::StatusPage::builder().icon_name("dev.remoteman.Host").title("remoteman host").description(format!("Version {}\nShares this computer's screen with your paired devices.", env!("CARGO_PKG_VERSION"))).build();

    let stack = adw::ViewStack::new();
    stack.add_titled_with_icon(&sharing_page, Some("sharing"), "Sharing", "network-transmit-receive-symbolic");
    stack.add_titled_with_icon(&devices_page, Some("devices"), "Devices", "computer-symbolic");
    stack.add_titled_with_icon(&about, Some("about"), "About", "help-about-symbolic");
    let header = adw::HeaderBar::builder().title_widget(&adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build()).build();
    let pairing = adw::Banner::builder().title("Pairing is open: the next new device to connect is paired, then pairing closes").button_label("Close Pairing").build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&pairing);
    content.append(&stack);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&toasts));
    // Closing the window leaves the icon and the sharing as they are.
    let window = adw::ApplicationWindow::builder().application(app).title("remoteman host").default_width(640).default_height(720).hide_on_close(true).content(&toolbar).build();

    let ui = Rc::new(Ui {
        app: app.clone(),
        window,
        stack,
        toasts,
        name,
        share,
        status,
        disconnect,
        pairing,
        pair,
        devices,
        rows: RefCell::new(Vec::new()),
        clipboard,
        files,
        address,
        key,
        setting: Cell::new(false),
        shown: RefCell::new(None),
        icon: RefCell::new(None),
    });

    at_sign_in.connect_active_notify({
        let ui = ui.clone();
        move |row| {
            if let Err(e) = set_autostart(row.is_active()) {
                ui.toast(&format!("That couldn't be changed: {e}"));
            }
        }
    });
    notify.connect_active_notify({
        let ui = ui.clone();
        move |row| {
            if let Err(e) = set_notifies(row.is_active()) {
                ui.toast(&format!("That couldn't be changed: {e}"));
            }
        }
    });

    ui.share.connect_active_notify({
        let ui = ui.clone();
        move |row| {
            if !ui.setting.get() {
                ui.share(row.is_active());
            }
        }
    });
    ui.disconnect.connect_clicked({
        let ui = ui.clone();
        move |_| ui.tell("drop")
    });
    ui.pair.connect_activated({
        let ui = ui.clone();
        move |_| ui.tell("pair on")
    });
    ui.pairing.connect_button_clicked({
        let ui = ui.clone();
        move |_| ui.tell("pair off")
    });
    for (row, what) in [(&ui.clipboard, "clipboard"), (&ui.files, "files")] {
        let ui = ui.clone();
        row.connect_active_notify(move |row| {
            if !ui.setting.get() {
                ui.tell(&format!("allow {what} {}", if row.is_active() { "on" } else { "off" }));
            }
        });
    }
    ui
}

/// Save a picture of the window as it is drawn.
fn shoot(window: &adw::ApplicationWindow, path: &str) {
    let (w, h) = (window.width() as f32, window.height() as f32);
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let saved = snapshot.to_node().zip(window.native().and_then(|n| n.renderer())).map(|(node, renderer)| renderer.render_texture(&node, Some(&graphene::Rect::new(0.0, 0.0, w, h))).save_to_png(path));
    match saved {
        Some(Ok(())) => println!("saved {path}"),
        _ => println!("couldn't save {path}"),
    }
}

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let value = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let background = args.iter().any(|a| a == "--background");
    let (page, shot, after) = (value("--page"), value("--shot"), value("--after").and_then(|s| s.parse::<f64>().ok()).unwrap_or(1.5));

    // One of it: opening it again brings the window of the one that runs.
    let app = adw::Application::builder().application_id("dev.remoteman.Host").build();
    let ui: Rc<RefCell<Option<Rc<Ui>>>> = Rc::default();
    app.connect_startup(|_| gtk::Window::set_default_icon_name("dev.remoteman.Host"));
    app.connect_activate(move |app| {
        if let Some(ui) = ui.borrow().as_ref() {
            return ui.open(page.as_deref().unwrap_or("sharing"));
        }
        let built = build(app);
        // The icon lives on while the window is closed.
        std::mem::forget(app.hold());
        let (tx, rx) = mpsc::channel::<Cmd>();
        // Started at sign-in, the top bar's place for icons may not be up yet: wait for it
        // then, where a start by hand says at once that there is none.
        match (Icon { state: State::Paused, name: built.name.clone(), tx }).assume_sni_available(background).spawn() {
            Ok(handle) => *built.icon.borrow_mut() = Some(handle),
            // No place for icons on this desktop: the window is all there is.
            Err(e) => eprintln!("remoteman-tray: no icon in the top bar ({e}); use the window"),
        }
        built.refresh();
        glib::timeout_add_local(std::time::Duration::from_millis(150), {
            let ui = built.clone();
            move || {
                while let Ok(cmd) = rx.try_recv() {
                    ui.run(cmd);
                }
                glib::ControlFlow::Continue
            }
        });
        glib::timeout_add_seconds_local(1, {
            let ui = built.clone();
            move || {
                ui.refresh();
                glib::ControlFlow::Continue
            }
        });
        if !background || built.icon.borrow().is_none() || shot.is_some() {
            built.open(page.as_deref().unwrap_or("sharing"));
        }
        if let Some(path) = shot.clone() {
            let ui = built.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis((after * 1000.0) as u64), move || {
                shoot(&ui.window, &path);
                if let Some(icon) = ui.icon.borrow_mut().take() {
                    icon.shutdown().wait();
                }
                ui.app.quit();
            });
        }
        *ui.borrow_mut() = Some(built);
    });
    app.run_with_args::<&str>(&[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ksni::Tray;

    #[test]
    fn the_icon_has_a_shape_for_each_state() {
        let (tx, _rx) = mpsc::channel();
        let mut icon = Icon { state: State::Paused, name: "desk".into(), tx };
        let sharing = |client| State::Sharing { fingerprint: String::new(), port: 7791, pairing: false, clipboard: false, files: false, client, devices: Vec::new() };
        let device = host::Client { name: "pad".into(), since: 0, width: 1, height: 1, refresh: 60, placement: String::new(), picture: String::new(), sound: false };
        let mut names = Vec::new();
        for state in [State::Paused, State::Failed("why".into()), sharing(None), sharing(Some(device))] {
            icon.state = state;
            names.push(icon.icon_name());
        }
        assert_eq!(names, ["dev.remoteman.Host-paused-symbolic", "dev.remoteman.Host-failed-symbolic", "dev.remoteman.Host-symbolic", "dev.remoteman.Host-connected-symbolic"]);
        // Each has its picture, where the installer takes it from.
        for name in names {
            let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux").join(format!("{name}.svg"));
            assert!(file.exists(), "{} is missing", file.display());
        }
    }

    #[test]
    fn the_two_choices_are_kept_and_taken_back() {
        let dir = std::env::temp_dir().join(format!("remoteman-tray-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: the only test that reads or sets these.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
            std::env::set_var("XDG_STATE_HOME", dir.join("state"));
        }
        assert!(!autostart().exists() && !notifies());
        set_autostart(true).unwrap();
        let entry = std::fs::read_to_string(autostart()).unwrap();
        assert!(autostart().starts_with(dir.join("config/autostart")));
        assert!(entry.contains("--background") && entry.contains("Type=Application"), "{entry}");
        set_autostart(false).unwrap();
        set_autostart(false).unwrap();
        assert!(!autostart().exists());
        set_notifies(true).unwrap();
        assert!(notifies());
        set_notifies(false).unwrap();
        set_notifies(false).unwrap();
        assert!(!notifies());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
