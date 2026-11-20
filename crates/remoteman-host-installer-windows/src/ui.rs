//! The pages, WinUI 3 through windows-reactor, in the look of the design boards
//! (docs/design/windows/HostFirstRun.dc.html above all: a card of numbered rows, a title and a
//! line under it, the buttons at the bottom right, Mica, light or dark as Windows is):
//!
//! - installing: what will be installed and where (Install), then each step as it goes, then
//!   done (Open Remoteman Host) or what failed and where the log is;
//! - uninstalling: what will be removed (Remove), each step, done or failed.
//!
//! The steps run on a thread of their own (src/engine.rs); their events come back through a
//! chain of Reactor background tasks, each waiting for the next event. The window can't be
//! closed while they run (Reactor has no closing event, so its window procedure is wrapped).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{CallWindowProcW, GWLP_WNDPROC, SetWindowLongPtrW, WM_CLOSE, WNDPROC};
use windows_reactor::*;

use crate::archive::Archive;
use crate::engine::{self, Ctx, Event, Facts, Mode, Outcome, StepId, StepInfo};
use crate::{log, system};

/// The mark from the design boards.
const LOGO_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect x=".5" y=".5" width="15" height="15" rx="3.5" fill="#0B1437" stroke="#2458F5"/><rect x="3" y="5" width="7" height="5" rx="1.2" fill="#2458F5"/><path d="M11.5 6h2M11.5 7.8h2.5M11.5 9.5h1.5" stroke="#fff" stroke-width="1.1" stroke-linecap="round"/></svg>"##;
const ICON: &[u8] = include_bytes!("../assets/setup.ico");

/// Exit codes the starter acts on (src/launch.rs).
pub const EXIT_DONE: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_CANCELLED: i32 = 2;
/// Done, and open Remoteman Host (its window).
pub const EXIT_OPEN_APP: i32 = 10;
/// Done, and start Remoteman Host again in the background (it was running before).
pub const EXIT_RESTART_APP: i32 = 11;

static EXIT: AtomicI32 = AtomicI32::new(EXIT_CANCELLED);
/// The steps are running: the window stays open.
static BUSY: AtomicBool = AtomicBool::new(false);
static OLD_PROC: AtomicIsize = AtomicIsize::new(0);
static ENGINE: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

thread_local! {
    /// Where the wrapped window procedure says a close was refused.
    static SENDER: RefCell<Option<LocalSender<Message>>> = const { RefCell::new(None) };
}

#[derive(Clone, PartialEq)]
pub struct Start {
    ctx: Ctx,
    facts: Facts,
    plan: Vec<StepInfo>,
    theme: WindowTheme,
    icon: Rc<str>,
}

pub fn run(args: &crate::Args) -> i32 {
    let source = args.source.clone().unwrap_or_else(|| std::env::current_exe().expect("this exe"));
    let ctx = Ctx::new(args, source);
    let archive = Archive::open(&ctx.source);
    if ctx.mode == Mode::Install && archive.is_err() {
        let e = archive.err().map(|e| e.to_string()).unwrap_or_default();
        log::say(&format!("pages: {e}"));
        system::message(&format!("remoteman host can't be installed from this file: {e}"));
        return EXIT_FAILED;
    }
    let facts = engine::facts(&ctx, archive.as_ref().ok());
    let plan = engine::plan(&ctx, &facts);
    let theme = match args.theme.as_deref() {
        Some("dark") => WindowTheme::Dark,
        Some("light") => WindowTheme::Light,
        _ => WindowTheme::System,
    };
    // The window's icon is a file: beside this exe (a folder of this run's own).
    let icon_path = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("setup.ico"))).unwrap_or_else(|| std::env::temp_dir().join("remoteman-host-setup.ico"));
    if std::fs::read(&icon_path).ok().as_deref() != Some(ICON) {
        let _ = std::fs::write(&icon_path, ICON);
    }
    log::say(&format!("pages: {:?}{}, {} steps, installed now: {:?}", ctx.mode, if ctx.dry_run { " (dry run)" } else { "" }, plan.len(), facts.installed));
    let start = Start { ctx, facts, plan, theme, icon: Rc::from(icon_path.to_string_lossy().as_ref()) };
    if let Err(e) = App::run_component::<Installer>(start) {
        log::say(&format!("pages: the window ended with an error: {e}"));
        return EXIT_FAILED;
    }
    // The window can't close while the steps run; should it have gone some other way, they
    // are let finish.
    if let Some(engine) = ENGINE.lock().unwrap().take() {
        log::say("pages: waiting for the steps to finish");
        let _ = engine.join();
    }
    let code = EXIT.load(Ordering::SeqCst);
    log::say(&format!("pages: closed ({code})"));
    code
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Working,
    Done,
    Failed,
}

struct Row {
    state: State,
    /// The latest line while it runs, or what it came to.
    detail: Option<String>,
    word: Option<String>,
}

pub struct Installer {
    page: Page,
    rows: Vec<Row>,
    outcome: Option<Outcome>,
    events: Option<Arc<Mutex<Receiver<Event>>>>,
    close_refused: bool,
}

#[derive(Clone)]
pub enum Message {
    Hwnd(isize),
    Go,
    Cancel,
    Event(Option<Event>),
    Close,
    OpenApp,
    OpenLog,
    CloseRefused,
}

impl Component for Installer {
    type Input = Start;
    type Message = Message;

    fn create(start: &Start, context: &ComponentContext<Self>) -> Self {
        let _ = context.run_window(|window| Message::Hwnd(window.as_raw() as isize));
        SENDER.with(|s| *s.borrow_mut() = Some(context.sender()));
        let rows = start.plan.iter().map(|_| Row { state: State::Waiting, detail: None, word: None }).collect();
        Installer { page: Page::Welcome, rows, outcome: None, events: None, close_refused: false }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        let start = context.input().clone();
        match message {
            Message::Hwnd(hwnd) => wrap_window(hwnd),
            Message::Go => {
                if self.page != Page::Welcome {
                    return;
                }
                log::say("pages: go");
                self.page = Page::Running;
                BUSY.store(true, Ordering::SeqCst);
                let (send, receive) = mpsc::channel();
                let (ctx, facts) = (start.ctx.clone(), start.facts.clone());
                let spawned = std::thread::Builder::new().name("steps".into()).spawn(move || {
                    let mut emit = |e: Event| {
                        let _ = send.send(e);
                    };
                    engine::run(&ctx, &facts, &mut emit);
                });
                match spawned {
                    Ok(handle) => *ENGINE.lock().unwrap() = Some(handle),
                    Err(e) => {
                        BUSY.store(false, Ordering::SeqCst);
                        self.page = Page::Failed;
                        self.outcome = Some(Outcome { ok: false, failed: None, log: log::path(), app_was_running: false, at_restart: 0 });
                        log::say(&format!("pages: the steps couldn't start: {e}"));
                        return;
                    }
                }
                let events = Arc::new(Mutex::new(receive));
                self.events = Some(events.clone());
                next_event(events, context);
            }
            Message::Event(event) => {
                let Some(event) = event else {
                    // The steps' thread ended without saying it had finished.
                    if BUSY.swap(false, Ordering::SeqCst) {
                        self.page = Page::Failed;
                        EXIT.store(EXIT_FAILED, Ordering::SeqCst);
                    }
                    return;
                };
                let index = |id: StepId| start.plan.iter().position(|s| s.id == id);
                match event {
                    Event::Started(id) => {
                        if let Some(i) = index(id) {
                            self.rows[i].state = State::Working;
                        }
                    }
                    Event::Detail(id, line) => {
                        if let Some(i) = index(id) {
                            self.rows[i].detail = Some(line);
                        }
                    }
                    Event::Done(id, word, detail) => {
                        if let Some(i) = index(id) {
                            self.rows[i].state = State::Done;
                            self.rows[i].word = Some(word);
                            self.rows[i].detail = detail;
                        }
                    }
                    Event::Failed(id, reason) => {
                        if let Some(i) = index(id) {
                            self.rows[i].state = State::Failed;
                            self.rows[i].word = Some("Failed".into());
                            self.rows[i].detail = Some(reason);
                        }
                    }
                    Event::Finished(outcome) => {
                        BUSY.store(false, Ordering::SeqCst);
                        self.close_refused = false;
                        self.page = if outcome.ok { Page::Done } else { Page::Failed };
                        let code = match (outcome.ok, outcome.app_was_running && start.ctx.mode == Mode::Install && start.facts.has_app) {
                            (true, true) => EXIT_RESTART_APP,
                            (true, false) => EXIT_DONE,
                            (false, _) => EXIT_FAILED,
                        };
                        EXIT.store(code, Ordering::SeqCst);
                        self.outcome = Some(outcome);
                        let _ = context.activate_window();
                        return;
                    }
                }
                if let Some(events) = self.events.clone() {
                    next_event(events, context);
                }
            }
            Message::Cancel | Message::Close => {
                if !BUSY.load(Ordering::SeqCst) {
                    let _ = context.close_window();
                }
            }
            Message::OpenApp => {
                EXIT.store(EXIT_OPEN_APP, Ordering::SeqCst);
                let _ = context.close_window();
            }
            Message::OpenLog => {
                if let Some(outcome) = &self.outcome {
                    let notepad = system::windows_dir().join(r"System32\notepad.exe");
                    let _ = std::process::Command::new(notepad).arg(&outcome.log).spawn();
                }
            }
            Message::CloseRefused => self.close_refused = true,
        }
    }

    fn view(&self, start: &Start, context: &mut ViewContext<Self>) -> View {
        let uninstall = start.ctx.mode == Mode::Uninstall;
        context.window_title(if uninstall { "Remove remoteman host" } else { "remoteman host setup" });
        context.window_visuals(
            WindowVisuals::new()
                .client_size(720.0, 680.0)
                .constraints(WindowConstraints { min_width: Some(560.0), min_height: Some(520.0), max_width: None, max_height: None })
                .backdrop(WindowBackdrop::Mica)
                .theme(start.theme)
                .icon(start.icon.clone()),
        );
        let title_bar = TitleBar::new()
            .title(if uninstall { "remoteman host: remove" } else { "remoteman host setup" })
            .icon(Icon::image_svg(EncodedImage::new(LOGO_SVG.to_vec())))
            .grid_row(0);

        let mut parts: Vec<View> = Vec::new();
        if start.ctx.dry_run {
            parts.push(
                InfoBar::new()
                    .is_open(true)
                    .is_closable(false)
                    .severity(InfoBarSeverity::Warning)
                    .title("Dry run")
                    .message(if uninstall {
                        format!("Nothing on this PC is changed. What would be done is written to {}; the folder deleted is the dry run's own, {}.", log::path().display(), start.ctx.dir.display())
                    } else {
                        format!("Nothing on this PC is changed. What would be done is written to {}; the files are unpacked into {}.", log::path().display(), start.ctx.dir.display())
                    })
                    .into(),
            );
        }
        let mut buttons: Vec<View> = Vec::new();
        match (self.page, uninstall) {
            (Page::Welcome, false) => self.welcome(start, &mut parts, &mut buttons, context),
            (Page::Welcome, true) => self.confirm(start, &mut parts, &mut buttons, context),
            (Page::Running, _) => {
                parts.push(heading(
                    if uninstall { "Removing remoteman host" } else { "Installing remoteman host" },
                    "This takes a minute or so. The window can be closed when it is done.",
                ));
                if self.close_refused {
                    parts.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Informational).message("It can't be stopped half way: the window closes when it is done.").into());
                }
                parts.push(self.steps(start));
                buttons.push(Button::new().content("Close").is_enabled(false).min_width(96.0).into());
            }
            (Page::Done, _) => self.done(start, &mut parts, &mut buttons, context),
            (Page::Failed, _) => self.failed(start, &mut parts, &mut buttons, context),
        }

        let body = ScrollViewer::new()
            .content(Border::new().padding(Thickness::new(32.0, 4.0, 32.0, 16.0)).content(StackPanel::new().spacing(16.0).children(parts)))
            .grid_row(1);
        let footer = Border::new()
            .padding(Thickness::new(32.0, 12.0, 32.0, 24.0))
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).horizontal_alignment(HorizontalAlignment::Right).children(buttons))
            .grid_row(2);
        Grid::new().rows([GridLength::Auto, GridLength::STAR, GridLength::Auto]).children((title_bar, body, footer)).into()
    }
}

impl Drop for Installer {
    fn drop(&mut self) {
        SENDER.with(|s| *s.borrow_mut() = None);
    }
}

impl Installer {
    fn welcome(&self, start: &Start, parts: &mut Vec<View>, buttons: &mut Vec<View>, context: &ViewContext<Self>) {
        let version = &start.facts.version;
        parts.push(heading(
            "Install remoteman host",
            &format!("It lets your paired devices see and control this PC. Version {version}, into {}.", start.ctx.real_dir.display()),
        ));
        if let Some(installed) = &start.facts.installed {
            let what = match (installed.version.as_str(), installed.inno) {
                ("", _) => "remoteman host is installed here already".to_string(),
                (v, true) => format!("remoteman host {v} is installed here already, by its earlier installer"),
                (v, false) => format!("remoteman host {v} is installed here already"),
            };
            parts.push(
                InfoBar::new()
                    .is_open(true)
                    .is_closable(false)
                    .severity(InfoBarSeverity::Informational)
                    .message(format!("{what}. It is replaced; this PC's key and the paired devices are kept."))
                    .into(),
            );
        }
        let mut items = vec![
            ("The host", "A service that starts with Windows, so a paired device can reach this PC from the sign-in screen on."),
            ("Display driver", "remoteman's own: a monitor for each device, at its size and refresh rate. Signed with a certificate made on this PC only, whose private key is deleted at once."),
            ("Firewall rule", "Lets paired devices reach the host on UDP port 7791."),
        ];
        if start.facts.has_app {
            items.push(("Remoteman Host", "The app in the notification area, for pairing devices and its settings. It starts when you sign in."));
        }
        parts.push(card(items.iter().enumerate().map(|(i, (title, words))| row(badge(i + 1, State::Waiting), title, Some(words), None)).collect()));
        parts.push(caption("Settings > Apps > remoteman host removes all of it again.").into());
        buttons.push(button("Cancel", context.message(Message::Cancel), false));
        buttons.push(button("Install", context.message(Message::Go), true));
    }

    fn confirm(&self, start: &Start, parts: &mut Vec<View>, buttons: &mut Vec<View>, context: &ViewContext<Self>) {
        let Some(installed) = &start.facts.installed else {
            parts.push(heading("remoteman host isn't installed", &format!("Nothing of it was found on this PC ({} isn't there).", start.ctx.real_dir.display())));
            buttons.push(button("Close", context.message(Message::Close), true));
            return;
        };
        let version = if installed.version.is_empty() { String::new() } else { format!(" {}", installed.version) };
        parts.push(heading("Remove remoteman host?", &format!("remoteman host{version}, in {}. Everything its installer set up is taken away:", start.ctx.real_dir.display())));
        let mut items = vec![
            ("The host and its service", "Paired devices can no longer reach this PC."),
            ("Display driver", "With the certificate made for it on this PC."),
            ("Firewall rule", "For UDP port 7791."),
        ];
        if start.facts.app_installed {
            items.push(("Remoteman Host", "With its start at sign-in and its shortcut in Start."));
        }
        items.push(("This PC's key and the paired devices", "Each device has to pair again after a new install."));
        parts.push(card(items.iter().enumerate().map(|(i, (title, words))| row(badge(i + 1, State::Waiting), title, Some(words), None)).collect()));
        buttons.push(button("Cancel", context.message(Message::Cancel), false));
        buttons.push(button("Remove", context.message(Message::Go), true));
    }

    fn done(&self, start: &Start, parts: &mut Vec<View>, buttons: &mut Vec<View>, context: &ViewContext<Self>) {
        let outcome = self.outcome.as_ref();
        if start.ctx.mode == Mode::Uninstall {
            let later = outcome.map(|o| o.at_restart).unwrap_or(0);
            let words = if later == 0 { "Nothing of it is left on this PC.".to_string() } else { format!("{later} of its files go when Windows restarts; nothing else of it is left.") };
            parts.push(heading("remoteman host is removed", &words));
            parts.push(self.steps(start));
            buttons.push(button("Close", context.message(Message::Close), true));
        } else {
            let words = if start.facts.has_app {
                "The host is running and starts with Windows. Pair a device in Remoteman Host: on the device, choose Pair a computer and pick this PC."
            } else {
                "The host is running and starts with Windows. On a device, choose Pair a computer and pick this PC."
            };
            parts.push(heading("remoteman host is installed", words));
            parts.push(self.steps(start));
            if start.facts.has_app {
                buttons.push(button("Close", context.message(Message::Close), false));
                buttons.push(button("Open Remoteman Host", context.message(Message::OpenApp), true));
            } else {
                buttons.push(button("Close", context.message(Message::Close), true));
            }
        }
        if start.ctx.dry_run {
            parts.push(caption(&format!("Dry run: nothing was changed. What would have been done is in {}.", log::path().display())).into());
        }
    }

    fn failed(&self, start: &Start, parts: &mut Vec<View>, buttons: &mut Vec<View>, context: &ViewContext<Self>) {
        let uninstall = start.ctx.mode == Mode::Uninstall;
        let outcome = self.outcome.as_ref();
        parts.push(heading(
            if uninstall { "remoteman host couldn't be removed" } else { "remoteman host couldn't be installed" },
            "It stopped at the step that failed; nothing after it was done.",
        ));
        let (title, reason) = match outcome.and_then(|o| o.failed.as_ref()) {
            Some((id, reason)) => (start.plan.iter().find(|s| s.id == *id).map(|s| s.title.clone()).unwrap_or_default(), reason.clone()),
            None => ("The steps".to_string(), "they ended without saying how".to_string()),
        };
        parts.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).title(format!("{title}: failed")).message(reason).into());
        let log = outcome.map(|o| o.log.clone()).unwrap_or_else(log::path);
        let done = |id: StepId| self.rows.iter().zip(&start.plan).any(|(r, s)| s.id == id && r.state == State::Done);
        let next = if uninstall {
            "Settings > Apps > remoteman host tries again.".to_string()
        } else if done(StepId::Register) || start.facts.installed.is_some() {
            "What was installed so far can be removed in Settings > Apps > remoteman host.".to_string()
        } else if done(StepId::Files) {
            format!("Its files were copied into {}; installing again replaces them.", start.ctx.real_dir.display())
        } else {
            "Nothing was installed.".to_string()
        };
        parts.push(TextBlock::new().text(format!("What happened is in {}. {next}", log.display())).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true).into());
        parts.push(self.steps(start));
        buttons.push(button("Open the log", context.message(Message::OpenLog), false));
        buttons.push(button("Close", context.message(Message::Close), true));
    }

    /// The steps, each with how it stands.
    fn steps(&self, start: &Start) -> View {
        card(
            start
                .plan
                .iter()
                .zip(&self.rows)
                .enumerate()
                .map(|(i, (step, row_state))| {
                    let words = row_state.detail.clone().unwrap_or_else(|| step.caption.clone());
                    let word = row_state.word.as_deref().map(|w| {
                        let brush = match row_state.state {
                            State::Failed => ThemeBrush::SystemCritical,
                            _ => ThemeBrush::SystemSuccess,
                        };
                        TextBlock::new().text(w).font_size(12.0).foreground(brush).into()
                    });
                    row(badge(i + 1, row_state.state), &step.title, Some(&words), word)
                })
                .collect(),
        )
    }
}

/// Waits, on a background task, for the steps' next event.
fn next_event(events: Arc<Mutex<Receiver<Event>>>, context: &ComponentContext<Installer>) {
    let _ = context.spawn_background(move |_| Message::Event(events.lock().unwrap().recv().ok()));
}

// ---- The window's close, refused while the steps run ----

fn wrap_window(hwnd: isize) {
    if hwnd == 0 || OLD_PROC.load(Ordering::SeqCst) != 0 {
        return;
    }
    // SAFETY: the window is ours (this thread's); the old procedure is called for everything
    // but a refused close.
    let old = unsafe { SetWindowLongPtrW(HWND(hwnd as *mut _), GWLP_WNDPROC, procedure as *const () as isize) };
    OLD_PROC.store(old, Ordering::SeqCst);
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CLOSE && BUSY.load(Ordering::SeqCst) {
        log::say("pages: the window's close was refused while the steps run");
        SENDER.with(|s| {
            if let Some(s) = s.borrow().as_ref() {
                let _ = s.send(Message::CloseRefused);
            }
        });
        return LRESULT(0);
    }
    // SAFETY: the procedure that was there, as Windows would have called it.
    unsafe {
        let old: WNDPROC = std::mem::transmute::<isize, WNDPROC>(OLD_PROC.load(Ordering::SeqCst));
        CallWindowProcW(old, hwnd, msg, wparam, lparam)
    }
}

// ---- The pieces every page is made of ----

/// A page's title (24 px, as the design's first-run window) and the line under it.
fn heading(title: &str, line: &str) -> View {
    StackPanel::new()
        .spacing(4.0)
        .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        .children((
            TextBlock::new().text(title).font_family("Segoe UI Variable Display").font_size(24.0).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::Wrap),
            TextBlock::new().text(line).foreground(ThemeBrush::SecondaryText).text_wrapping(TextWrapping::Wrap),
        ))
        .into()
}

/// WinUI's Caption style in the secondary text colour.
fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(12.0).foreground(ThemeBrush::SecondaryText).text_wrapping(TextWrapping::Wrap)
}

/// Rows in one card, a hairline between them.
fn card(rows: Vec<View>) -> View {
    let children: Vec<View> = rows
        .into_iter()
        .enumerate()
        .map(|(i, r)| Border::new().border_brush(ThemeBrush::CardStroke).border_thickness(Thickness::new(0.0, if i == 0 { 0.0 } else { 1.0 }, 0.0, 0.0)).content(r).into())
        .collect();
    Border::new().background(ThemeBrush::CardBackground).border_brush(ThemeBrush::CardStroke).border_thickness(1.0).corner_radius(6.0).content(StackPanel::new().children(children)).into()
}

/// A row: the badge, a title and a line under it, a word on the right.
fn row(badge: View, title: &str, words: Option<&str>, right: Option<View>) -> View {
    let mut text: Vec<View> = vec![TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::Wrap).into()];
    if let Some(words) = words.filter(|w| !w.is_empty()) {
        text.push(caption(words).into());
    }
    let mut cells: Vec<View> = vec![
        Border::new().vertical_alignment(VerticalAlignment::Center).content(badge).into(),
        StackPanel::new().grid_column(1).vertical_alignment(VerticalAlignment::Center).children(text).into(),
    ];
    if let Some(right) = right {
        cells.push(Border::new().grid_column(2).vertical_alignment(VerticalAlignment::Center).content(right).into());
    }
    Border::new()
        .padding(Thickness::new(16.0, 10.0, 16.0, 10.0))
        .min_height(64.0)
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(14.0).children(cells))
        .into()
}

/// The number in a circle, a check, a cross, or a turning ring.
fn badge(n: usize, state: State) -> View {
    let circle = |brush: ThemeBrush, content: View| -> View {
        Border::new().width(24.0).height(24.0).corner_radius(12.0).border_thickness(1.0).border_brush(brush).content(content).into()
    };
    let centered_icon = |glyph: &str, brush: ThemeBrush| -> View {
        FontIcon::new().glyph(glyph).font_size(12.0).foreground(brush).horizontal_alignment(HorizontalAlignment::Center).vertical_alignment(VerticalAlignment::Center).into()
    };
    match state {
        State::Working => ProgressRing::new().is_active(true).is_indeterminate(true).width(24.0).height(24.0).automation_name("Working").into(),
        State::Done => circle(ThemeBrush::SystemSuccess, centered_icon("\u{E73E}", ThemeBrush::SystemSuccess)),
        State::Failed => circle(ThemeBrush::SystemCritical, centered_icon("\u{E711}", ThemeBrush::SystemCritical)),
        State::Waiting => circle(
            ThemeBrush::SecondaryText,
            TextBlock::new()
                .text(n.to_string())
                .font_size(12.0)
                .font_weight(FontWeight::SEMI_BOLD)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        ),
    }
}

fn button(text: &str, clicked: Callback<()>, accent: bool) -> View {
    let b = Button::new().content(text).min_width(96.0).on_click(clicked);
    if accent { b.style(ButtonStyle::Accent).into() } else { b.into() }
}
