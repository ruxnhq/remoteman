//! The application: what lives while the app runs, whether or not its window is open. It keeps
//! the host's last answer (asked once a second on another thread, bridge.rs), the icon in the
//! notification area with its tooltip and menu, the notifications, and the pairing question; the
//! window (window.rs) is declared here and opened when it is wanted. Closing the window keeps the
//! icon; only Quit ends the app, and the host runs on either way.

use std::rc::Rc;
use std::sync::mpsc::Sender;

use windows_reactor::*;

use crate::bridge::{self, Answer, Job};
use crate::host::{self, Pending, Status, Unreached};
use crate::system::{self, Icons};
use crate::window::{Callbacks, HostWindow, Shown};

const WINDOW: &str = "main";
const TRAY: &str = "tray";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Sharing,
    Devices,
    About,
}

impl Page {
    pub fn tag(self) -> &'static str {
        match self {
            Page::Sharing => "sharing",
            Page::Devices => "devices",
            Page::About => "about",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Page> {
        match tag {
            "sharing" => Some(Page::Sharing),
            "devices" => Some(Page::Devices),
            "about" => Some(Page::About),
            _ => None,
        }
    }
}

/// How the app was started.
pub struct Start {
    pub port: u16,
    pub page: Page,
    /// The icon only, as at sign-in: no window until it is wanted.
    pub background: bool,
    pub theme: WindowTheme,
    /// The host's first answer, asked before anything is shown.
    pub status: Result<Status, Unreached>,
}

pub struct HostApp {
    port: u16,
    status: Result<Status, Unreached>,
    page: Page,
    /// A line at the bottom of the window.
    news: Option<String>,
    /// The device whose pairing question is showing.
    asking: Option<Pending>,
    /// The fingerprint of the device asked about last, so it is asked once.
    asked: Option<String>,
    /// How many files the device had answered for when last seen.
    answered: Option<u64>,
    /// Whether a device was connected when last seen.
    had_client: Option<bool>,
    jobs: Sender<Job>,
    icons: Icons,
    theme: WindowTheme,
}

#[derive(Clone)]
pub enum AppMessage {
    /// The host answered.
    Host(Answer),
    /// Show the window, at this page.
    Open(Option<Page>),
    Page(Option<Rc<str>>),
    Tell(String),
    Share(bool),
    Clipboard(bool),
    Files(bool),
    LockOnLeave(bool),
    Speakers(bool),
    Answer(ContentDialogResult),
    NewsClosed,
    SendFile,
    Menu(Key),
}

impl HostApp {
    /// This PC's name as devices are told it.
    fn pc(&self) -> String {
        match &self.status {
            Ok(s) if !s.name.is_empty() => s.name.clone(),
            _ => system::computer_name(),
        }
    }

    fn tell(&self, words: impl Into<String>) {
        let words = words.into();
        crate::log::say(&format!("telling the host: {words}"));
        let _ = self.jobs.send(Job::Tell(words));
    }

    fn open(&mut self, page: Option<Page>, context: &ApplicationContext<Self>) {
        if let Some(page) = page {
            self.page = page;
        }
        context.show_window(WINDOW);
    }

    /// A new answer to `status`: what it means beyond what is shown.
    fn take(&mut self, status: Result<Status, Unreached>, context: &ApplicationContext<Self>) {
        if status.is_ok() != self.status.is_ok() || matches!((&status, &self.status), (Err(a), Err(b)) if a != b) {
            crate::log::say(match &status {
                Ok(_) => "the host answers",
                Err(Unreached::NoHost) => "no host answers",
                Err(Unreached::Refused) => "the host's pipe refused this user",
            });
        }
        let pc = match &status {
            Ok(s) if !s.name.is_empty() => s.name.clone(),
            _ => system::computer_name(),
        };
        if let Ok(s) = &status {
            // The device answered for a file sent to it.
            if self.answered.is_some_and(|before| s.answered > before) {
                let (head, text) = if s.sent_ok {
                    ("The device has the file", format!("The device has the file: {}", s.said))
                } else {
                    ("The file didn't reach the device", format!("The file didn't reach the device: {}", s.said))
                };
                self.news = Some(text);
                self.notify(context, head, &s.said);
            }
            self.answered = Some(s.answered);
        }
        // A device that just connected (not one already connected when the app started).
        let client = status.as_ref().ok().and_then(|s| s.client.clone());
        if self.had_client == Some(false)
            && let Some(c) = &client
        {
            self.notify(context, &format!("{} connected", c.name), &format!("It has {} on {pc}.", host::placement_phrase(&c.placement)));
        }
        self.had_client = Some(client.is_some());
        // A device that wants to pair is put to the owner, once.
        match status.as_ref().ok().and_then(|s| s.pending.clone()) {
            Some(p) if self.asked.as_deref() != Some(p.fingerprint.as_str()) => {
                self.asked = Some(p.fingerprint.clone());
                self.notify(context, &format!("{} wants to pair", p.name), &format!("Its code should be {}. Answer in Remoteman Host.", host::code(&p.code)));
                crate::log::say(&format!("asking: pair with {}? code {}", p.name, p.code));
                self.asking = Some(p);
                self.open(Some(Page::Devices), context);
            }
            Some(_) => {}
            None => {
                if self.asking.take().is_some() {
                    crate::log::say("the question was taken away: answered elsewhere, or its minute is up");
                }
                self.asked = None;
            }
        }
        self.status = status;
    }

    fn notify(&self, context: &ApplicationContext<Self>, title: &str, text: &str) {
        crate::log::say(&format!("notified: {title}: {text}"));
        context.notify(TRAY, title, text);
    }

    /// The icon's menu: what the state is called and a line or two, then what can be done.
    fn menu(&self) -> Vec<MenuItem> {
        let mut items = Vec::new();
        let line = |items: &mut Vec<MenuItem>, text: &str| {
            let key = format!("line{}", items.len());
            items.push(MenuItem::disabled(key, menu_text(text)));
        };
        match &self.status {
            Err(why) => {
                line(&mut items, "Sharing couldn't start");
                line(&mut items, if *why == Unreached::Refused { "The host's pipe refused this user." } else { "The host isn't running." });
            }
            Ok(s) => {
                let (title, lines) = s.words(&self.pc());
                line(&mut items, &title);
                for l in &lines {
                    line(&mut items, l);
                }
                items.push(MenuItem::separator("sep1"));
                if s.client.is_some() {
                    if s.files {
                        items.push(MenuItem::item("send", "Send a file to the device…"));
                    }
                    items.push(MenuItem::item("drop", "Disconnect"));
                }
                items.push(if s.paused { MenuItem::item("resume", "Resume sharing") } else { MenuItem::item("pause", "Pause sharing") });
                if !s.paused {
                    items.push(MenuItem::item("pair", "Pair a device…"));
                }
            }
        }
        items.push(MenuItem::item("settings", "Settings…"));
        items.push(MenuItem::separator("sep2"));
        items.push(MenuItem::item("quit", "Quit remoteman"));
        items
    }

    /// Which icon, and what its tooltip says.
    fn tray(&self) -> (&str, String) {
        match &self.status {
            Err(Unreached::NoHost) => (&self.icons.paused, "remoteman: the host isn't running".into()),
            Err(Unreached::Refused) => (&self.icons.paused, "remoteman: the host's pipe refused this user".into()),
            Ok(s) => {
                let (title, lines) = s.words(&self.pc());
                let icon = if s.paused {
                    &self.icons.paused
                } else if s.client.is_some() {
                    &self.icons.connected
                } else {
                    &self.icons.sharing
                };
                (icon, tooltip(&format!("remoteman: {title}\n{}", lines.join("\n"))))
            }
        }
    }
}

impl Application for HostApp {
    type Input = Start;
    type Message = AppMessage;

    fn create(start: &Start, context: &ApplicationContext<Self>) -> Self {
        let jobs = bridge::start(start.port, context.sender());
        let mut app = HostApp {
            port: start.port,
            status: Err(Unreached::NoHost),
            page: start.page,
            news: None,
            asking: None,
            asked: None,
            answered: None,
            had_client: None,
            jobs,
            icons: system::icons(),
            theme: start.theme,
        };
        app.take(start.status.clone(), context);
        if !start.background {
            app.open(None, context);
        }
        crate::log::say(&format!("started{}", if start.background { " in the background" } else { "" }));
        app
    }

    fn update(&mut self, message: AppMessage, context: &ApplicationContext<Self>) {
        match message {
            AppMessage::Host(Answer::Status(status)) => self.take(*status, context),
            AppMessage::Host(Answer::Told { words, said }) => {
                crate::log::say(&format!("told the host: {words} -> {}", match &said {
                    Ok(s) => s.as_str(),
                    Err(Unreached::NoHost) => "(no host)",
                    Err(Unreached::Refused) => "(refused)",
                }));
                if words.starts_with("send ") {
                    self.news = Some(match said {
                        Ok(s) if s == "ok" => "Sending the file".into(),
                        Ok(s) => s,
                        Err(_) => "The host isn't running".into(),
                    });
                } else {
                    match said {
                        Err(Unreached::NoHost) => self.news = Some("The host isn't running, so nothing was changed.".into()),
                        Err(Unreached::Refused) => self.news = Some("The host's pipe refused this user, so nothing was changed.".into()),
                        Ok(s) if s != "ok" => self.news = Some(s),
                        Ok(_) => {}
                    }
                }
            }
            AppMessage::Open(page) => self.open(page, context),
            AppMessage::Page(tag) => {
                if let Some(page) = tag.as_deref().and_then(Page::from_tag) {
                    self.page = page;
                }
            }
            AppMessage::Tell(words) => self.tell(words),
            // The switches: only a change is told (a switch set from the host's answer may
            // report itself as toggled).
            AppMessage::Share(on) => {
                if matches!(&self.status, Ok(s) if s.paused == on) {
                    self.tell(if on { "sharing on" } else { "sharing off" });
                }
            }
            AppMessage::Clipboard(on) => {
                if matches!(&self.status, Ok(s) if s.clipboard != on) {
                    self.tell(format!("allow clipboard {}", if on { "on" } else { "off" }));
                }
            }
            AppMessage::Files(on) => {
                if matches!(&self.status, Ok(s) if s.files != on) {
                    self.tell(format!("allow files {}", if on { "on" } else { "off" }));
                }
            }
            AppMessage::LockOnLeave(on) => {
                if matches!(&self.status, Ok(s) if s.lock_on_leave != on) {
                    self.tell(format!("set lock {}", if on { "on" } else { "off" }));
                }
            }
            AppMessage::Speakers(on) => {
                if matches!(&self.status, Ok(s) if s.speakers_follow != on) {
                    self.tell(format!("set speakers {}", if on { "on" } else { "off" }));
                }
            }
            AppMessage::Answer(result) => {
                // Taken away (answered elsewhere, or its minute is up): nothing to say.
                if let Some(p) = self.asking.take() {
                    let accept = result == ContentDialogResult::Primary;
                    self.tell(if accept { "pair accept" } else { "pair decline" });
                    if accept {
                        self.news = Some(format!("{} was paired. Pairing has closed.", p.name));
                    }
                }
            }
            AppMessage::NewsClosed => self.news = None,
            AppMessage::SendFile => {
                let jobs = self.jobs.clone();
                let owner = crate::window::HWND.with(|h| h.get());
                system::pick_file(owner, move |path| {
                    let _ = jobs.send(Job::Tell(format!("send {path}")));
                });
            }
            AppMessage::Menu(key) => {
                crate::log::say(&format!("tray menu: {}", key.as_str().unwrap_or("?")));
                match key.as_str() {
                    Some("send") => self.update(AppMessage::SendFile, context),
                    Some("drop") => self.tell("drop"),
                    Some("resume") => self.tell("sharing on"),
                    Some("pause") => self.tell("sharing off"),
                    Some("pair") => {
                        self.tell("pair on");
                        self.open(Some(Page::Devices), context);
                    }
                    Some("settings") => self.open(Some(Page::Sharing), context),
                    Some("quit") => {
                        crate::log::say("quitting (the host runs on)");
                        context.exit();
                    }
                    _ => {}
                }
            }
        }
    }

    fn view(&self, _: &Start, context: &ApplicationViewContext<Self>) -> ApplicationView {
        let shown = Shown {
            status: self.status.clone(),
            pc: self.pc(),
            port: self.port,
            page: self.page,
            news: self.news.clone(),
            asking: self.asking.clone(),
            theme: self.theme,
            icon: Rc::from(self.icons.sharing.as_str()),
            on: Callbacks {
                page: context.callback(AppMessage::Page),
                tell: context.callback(AppMessage::Tell),
                share: context.callback(AppMessage::Share),
                clipboard: context.callback(AppMessage::Clipboard),
                files: context.callback(AppMessage::Files),
                lock_on_leave: context.callback(AppMessage::LockOnLeave),
                speakers: context.callback(AppMessage::Speakers),
                answer: context.callback(AppMessage::Answer),
                news_closed: context.callback(|()| AppMessage::NewsClosed),
                send_file: context.callback(|()| AppMessage::SendFile),
            },
        };
        let (icon, tip) = self.tray();
        ApplicationView::new().window::<HostWindow>(WINDOW, shown).notify_icon(
            TRAY,
            NotifyIcon::new(icon)
                .tooltip(tip)
                .on_activate(context.callback(|_| AppMessage::Open(None)))
                .menu(Menu::new(self.menu(), context.callback(AppMessage::Menu))),
        )
    }
}

/// A tooltip Windows takes: at most 127 UTF-16 units, no control characters but line breaks.
fn tooltip(text: &str) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in text.chars().filter(|c| *c == '\n' || !c.is_control()) {
        units += c.len_utf16();
        if units > 127 {
            break;
        }
        out.push(c);
    }
    out
}

/// A menu item's words, without control characters (WinUI shows an ampersand as it is).
fn menu_text(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltips_fit() {
        let long = format!("remoteman: A device is connected\n{}", "x".repeat(300));
        assert_eq!(tooltip(&long).encode_utf16().count(), 127);
        assert_eq!(tooltip("a\u{0}b\nc"), "ab\nc");
        // A character outside the basic plane is not cut in half.
        let emoji = "😀".repeat(70);
        assert_eq!(tooltip(&emoji).encode_utf16().count(), 126);
    }

    #[test]
    fn pages_by_tag() {
        for page in [Page::Sharing, Page::Devices, Page::About] {
            assert_eq!(Page::from_tag(page.tag()), Some(page));
        }
        assert_eq!(Page::from_tag("nope"), None);
    }
}
