//! The one window: the title bar (the pill during a session), the page the engine's state calls
//! for, its dialogs, and a line of news at the bottom. The words are the design's
//! (docs/design/windows); what each control does is the engine's to decide, through its
//! commands (`kit::cmd`) and the picture's typed calls (`picture.rs`).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use windows_reactor::*;

use crate::Options;
use crate::kit::{J, cmd};

/// What the process ends with, set when the window closes and the engine stops.
static CODE: AtomicI32 = AtomicI32::new(0);

pub fn exit_code() -> i32 {
    CODE.load(Ordering::Relaxed)
}

/// The mark from the design boards.
const LOGO_SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><rect width="16" height="16" rx="3.5" fill="#2458F5"/><rect x="6" y="5" width="7" height="5" rx="1.2" fill="none" stroke="#fff" stroke-width="1.2"/><path d="M2.5 6h2M2 7.8h2.5M3 9.5h1.5" stroke="#fff" stroke-width="1.1" stroke-linecap="round"/></svg>"##;

pub struct Viewer {
    /// Why the engine didn't start, if it didn't.
    failed: Option<String>,
    pub state: Value,
    pub hwnd: isize,
    /// The window's title as the engine last said it.
    title: String,
    pub panel: ElementRef<SwapChainPanel>,
    _surface: ElementObservation,
    binding: Option<SwapChainPanelBinding>,
    /// The panel's size and scale as last told to the engine.
    metrics: Option<(f64, f64, f32, f32)>,
    attached: bool,
    pub picture: crate::picture::Picture,
    /// What is being typed: the pair dialog's address, the edit dialog's, the locked page's
    /// password and its "Remember it", a computer's new name (its address and the text).
    pub pair_text: Option<String>,
    pub address_text: Option<String>,
    pub password: String,
    pub remember: Option<bool>,
    pub name_text: Option<(String, String)>,
    /// The dialog shown last, and one whose primary button was pressed: kept away until the
    /// engine's dialog changes, as the C# view kept it open instead.
    dialog_key: String,
    dialog_hidden: Option<String>,
    /// The pill floating over the picture: brought out at the top edge, gone again a little
    /// after the pointer leaves it (unless a menu of it is open).
    pub bar_peek: bool,
    bar_timer: Option<ComponentTimer>,
    pub over_bar: bool,
    /// The window's own title bar is taken away during a session in a window.
    captionless: bool,
    pub session_view: bool,
    pending: Rc<Cell<bool>>,
    theme: WindowTheme,
    /// For tests: a file whose words are the next walk step for the engine, and the last step
    /// taken from it, looked at every 300 ms.
    walk: Option<(std::path::PathBuf, String, Option<ComponentTimer>)>,
    window_size: (f64, f64),
    pos: Option<(i32, i32)>,
}

#[derive(Clone)]
pub enum Msg {
    /// The engine says its state has changed.
    Changed,
    Hwnd(isize),
    Surface(SwapChainPanelEvent),
    /// A command for the engine.
    Do(Value),
    /// A menu item: its key is the engine's command, as JSON (or the view's own word).
    Menu(Key),
    PairText(Rc<str>),
    AddressText(Rc<str>),
    Password(Rc<str>),
    Remember,
    Unlock,
    NameText(Rc<str>),
    NameCommit,
    Dialog(ContentDialogResult),
    /// The pill's window buttons, and a press on its words.
    Window(&'static str),
    Drag,
    /// The pointer at the picture's top edge; the pill's timer; the pointer on the pill.
    BarPeek,
    BarAway,
    /// For tests: look at the walk file again (`REMOTEMAN_WALK_FILE`).
    WalkTick,
    OverBar(bool),
    Scheme(ColorScheme),
    Bitrate(f64),
}

impl Component for Viewer {
    type Input = Options;
    type Message = Msg;

    fn create(options: &Options, context: &ComponentContext<Self>) -> Self {
        let pending = Rc::new(Cell::new(false));
        let sender = context.sender();
        let flag = pending.clone();
        // Not now: this may be in the middle of a call into the engine. Read the state later.
        let notify: remoteman_engine::Notify = Box::new(move || {
            if !flag.replace(true) {
                let _ = sender.send(Msg::Changed);
            }
        });
        let failed = remoteman_engine::start(options.engine.clone(), notify).err().map(|e| format!("{e:#}"));
        let _ = context.run_window(|window| Msg::Hwnd(window.as_raw() as isize));
        let panel = ElementRef::<SwapChainPanel>::new();
        let sender = context.sender();
        let surface = panel.observe_surface_metrics(move |event| {
            let _ = sender.send(Msg::Surface(event));
        });
        let picture = crate::picture::Picture::new(context.sender());
        let state = remoteman_engine::state().unwrap_or(Value::Null);
        remoteman_engine::say(&match &failed {
            Some(why) => format!("window: the engine didn't start: {why}"),
            None => "window: made, engine started".into(),
        });
        let mut viewer = Self {
            failed,
            state: Value::Null,
            hwnd: 0,
            title: "remoteman".into(),
            panel,
            _surface: surface,
            binding: None,
            metrics: None,
            attached: false,
            picture,
            pair_text: None,
            address_text: None,
            password: String::new(),
            remember: None,
            name_text: None,
            dialog_key: String::new(),
            dialog_hidden: None,
            bar_peek: false,
            bar_timer: None,
            over_bar: false,
            captionless: false,
            session_view: false,
            pending,
            theme: options.theme,
            walk: std::env::var_os("REMOTEMAN_WALK_FILE").map(|p| (p.into(), String::new(), Some(context.set_local_timeout(Duration::from_millis(300), || Msg::WalkTick)))),
            window_size: options.window.unwrap_or((984.0, 644.0)),
            pos: options.pos,
        };
        viewer.take(state, context);
        viewer
    }

    fn update(&mut self, message: Msg, context: &ComponentContext<Self>) {
        match message {
            Msg::Changed => {
                self.pending.set(false);
                self.refresh(context);
            }
            Msg::Hwnd(hwnd) => {
                self.hwnd = hwnd;
                self.picture.hwnd.set(hwnd);
                self.try_attach();
            }
            Msg::Surface(SwapChainPanelEvent::Metrics { binding, width, height, scale_x, scale_y }) => {
                let rebound = self.binding != Some(binding);
                self.binding = Some(binding);
                self.picture.scale.set((scale_x, scale_y));
                let m = (width, height, scale_x, scale_y);
                if self.metrics != Some(m) {
                    self.metrics = Some(m);
                    // The panel fills the window during a session: it starts at its corner.
                    remoteman_engine::panel(0, 0, (width * scale_x as f64).round() as i32, (height * scale_y as f64).round() as i32, scale_x, scale_y);
                }
                if rebound && self.attached {
                    // A new panel (or the same one bound again): it is given the swap chain again.
                    self.attached = false;
                }
                self.try_attach();
            }
            Msg::Surface(SwapChainPanelEvent::Rendering) => {}
            Msg::Do(v) => self.command(&v, context),
            Msg::Menu(key) => {
                if let Some(text) = key.as_str() {
                    match serde_json::from_str::<Value>(text) {
                        Ok(v) => self.command(&v, context),
                        Err(_) => remoteman_engine::say(&format!("a menu item nobody knows: {text}")),
                    }
                }
            }
            Msg::PairText(t) => self.pair_text = Some(t.to_string()),
            Msg::AddressText(t) => self.address_text = Some(t.to_string()),
            Msg::Password(t) => self.password = t.to_string(),
            Msg::Remember => self.remember = Some(!self.remember.unwrap_or_else(|| self.state.b("remember"))),
            Msg::Unlock => {
                let v = cmd("unlock", json!({"password": self.password, "remember": self.remember.unwrap_or_else(|| self.state.b("remember"))}));
                self.command(&v, context);
            }
            Msg::NameText(t) => {
                let address = self.state.o("screen").s("address").to_owned();
                self.name_text = Some((address, t.to_string()));
            }
            Msg::NameCommit => {
                if let Some((address, name)) = self.name_text.take() {
                    self.command(&cmd("rename", json!({"address": address, "name": name})), context);
                }
            }
            Msg::Dialog(result) => self.dialog_closed(result, context),
            Msg::Window(what) => match what {
                "minimise" => crate::system::minimise(self.hwnd),
                "maximise" => crate::system::maximise(self.hwnd),
                _ => {
                    let _ = context.close_window();
                }
            },
            Msg::Drag => crate::system::drag(self.hwnd),
            Msg::BarPeek => {
                if !self.bar_peek {
                    self.bar_peek = true;
                    self.bar_timer = Some(context.set_local_timeout(Duration::from_secs(2), || Msg::BarAway));
                }
            }
            Msg::BarAway => {
                // Not while the pointer is on it.
                if self.over_bar {
                    self.bar_timer = Some(context.set_local_timeout(Duration::from_secs(2), || Msg::BarAway));
                } else {
                    self.bar_peek = false;
                    self.bar_timer = None;
                }
            }
            Msg::OverBar(on) => {
                self.over_bar = on;
                if !on && self.bar_peek {
                    self.bar_timer = Some(context.set_local_timeout(Duration::from_secs(2), || Msg::BarAway));
                }
            }
            Msg::WalkTick => {
                let step = self.walk.as_ref().and_then(|w| std::fs::read_to_string(&w.0).ok()).map(|s| s.trim().to_owned()).unwrap_or_default();
                if let Some(w) = &mut self.walk {
                    if !step.is_empty() && step != w.1 {
                        w.1 = step.clone();
                        self.command(&cmd("walk", json!({"step": step})), context);
                    }
                }
                if let Some(w) = &mut self.walk {
                    w.2 = Some(context.set_local_timeout(Duration::from_millis(300), || Msg::WalkTick));
                }
            }
            Msg::Scheme(scheme) => {
                remoteman_engine::command(&cmd("dark", json!({"on": scheme == ColorScheme::Dark})));
            }
            Msg::Bitrate(step) => {
                let step = step.round() as i64;
                if self.state.o("session").n("bitrate_step") as i64 != step {
                    self.command(&cmd("bitrate", json!({"step": step})), context);
                }
            }
        }
    }

    fn view(&self, _: &Options, context: &mut ViewContext<Self>) -> View {
        context.window_title(self.title.clone());
        let mut visuals = WindowVisuals::new().client_size(self.window_size.0, self.window_size.1).backdrop(WindowBackdrop::Mica).theme(self.theme);
        if let Some((x, y)) = self.pos {
            visuals = visuals.initial_position(ScreenPoint::new(x, y));
        }
        context.window_visuals(visuals);
        context.on_color_scheme(context.callback(Msg::Scheme));

        if let Some(why) = &self.failed {
            return Grid::new()
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    TitleBar::new().title("remoteman"),
                    TextBlock::new().text(format!("remoteman can't start: {why}")).margin(32.0).text_wrapping(TextWrapping::Wrap).grid_row(1),
                ))
                .into();
        }

        let screen = self.state.o("screen");
        let kind = screen.s("kind");
        let session = self.session_view;
        let mut layers: Vec<View> = Vec::new();
        // The picture: always there (its panel keeps its swap chain), seen only in a session.
        layers.push(self.picture.view(&self.panel, session).grid_row_span(2).into());
        if session {
            layers.push(Border::new().grid_row_span(2).content(crate::session::figures(&self.state)).into());
            layers.push(Border::new().grid_row_span(2).content(crate::session::pill(self, context)).into());
        } else {
            let back = matches!(kind, "settings" | "computer" | "problem");
            let title = self.title.strip_suffix(" - remoteman").unwrap_or(&self.title).to_owned();
            layers.push(
                TitleBar::new()
                    .title(title)
                    .icon(Icon::image_svg(EncodedImage::new(LOGO_SVG.to_vec())))
                    .is_back_button_visible(back)
                    .on_back_requested(crate::kit::act(context, "back", Value::Null))
                    .preferred_height(WindowTitleBarHeight::Tall)
                    .grid_row(0)
                    .into(),
            );
            let page: View = match kind {
                "connecting" | "problem" => crate::pages::status(self, context),
                "settings" => crate::pages::settings(self, context),
                "computer" => crate::pages::computer(self, context),
                _ => crate::pages::computers(self, context),
            };
            layers.push(Border::new().grid_row(1).content(page).into());
        }
        let toast = self.state.get("toast").and_then(Value::as_str);
        layers.push(
            InfoBar::new()
                .is_open(toast.is_some())
                .is_closable(false)
                .severity(InfoBarSeverity::Informational)
                .message(toast.unwrap_or_default())
                .vertical_alignment(VerticalAlignment::Bottom)
                .horizontal_alignment(HorizontalAlignment::Center)
                .margin(16.0)
                .max_width(560.0)
                .grid_row(1)
                .into(),
        );
        let root = Grid::new().rows([GridLength::Auto, GridLength::STAR]).children(layers);
        match crate::dialogs::dialog(self, context) {
            Some(d) if self.dialog_hidden.as_deref() != Some(self.dialog_key.as_str()) => root.content_dialog(d),
            _ => root.into(),
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let code = remoteman_engine::stop();
        remoteman_engine::say(&format!("window: closed, leaving with {code}"));
        CODE.store(code, Ordering::Relaxed);
    }
}

impl Viewer {
    /// Hand the engine's state to the window: do what only the window can do, and keep it.
    fn refresh(&mut self, context: &ComponentContext<Self>) {
        if let Some(state) = remoteman_engine::state() {
            self.take(state, context);
        }
    }

    fn take(&mut self, mut state: Value, context: &ComponentContext<Self>) {
        if state.is_null() {
            return;
        }
        let effects = state.get_mut("effects").map(Value::take).unwrap_or(Value::Null);
        for effect in effects.as_array().map(Vec::as_slice).unwrap_or(&[]) {
            self.effect(effect, context);
        }
        let kind = state.o("screen").s("kind").to_owned();
        if kind != self.state.o("screen").s("kind") {
            remoteman_engine::say(&format!("screen: {kind}"));
            // What was being typed belongs to the page that was.
            self.name_text = None;
            self.password.clear();
            self.remember = None;
        }
        let session = kind == "session";
        if session && !self.session_view {
            // The picture takes the keyboard when the session's picture comes.
            self.picture.focus();
        }
        self.session_view = session;
        // The dialog: a new one, or the same with something new to say, comes out again.
        let key = crate::dialogs::key(&state);
        if key != self.dialog_key {
            if crate::dialogs::kind_of(&key) != crate::dialogs::kind_of(&self.dialog_key) {
                self.pair_text = None;
                self.address_text = None;
            }
            self.dialog_key = key;
            self.dialog_hidden = None;
        }
        self.state = state;
        // No title bar of Windows' own during a session in a window: the pill has the buttons.
        let captionless = self.session_view;
        if captionless != self.captionless && self.hwnd != 0 {
            self.captionless = captionless;
            crate::system::caption(self.hwnd, !captionless);
        }
        if !self.session_view {
            self.bar_peek = false;
        }
    }

    fn effect(&mut self, e: &Value, context: &ComponentContext<Self>) {
        remoteman_engine::say(&format!("effect: {e}"));
        match e.s("do") {
            "minimise" => crate::system::minimise(self.hwnd),
            "maximise" => crate::system::maximise(self.hwnd),
            "close" | "quit" => {
                let _ = context.close_window();
            }
            "title" => self.title = e.s("text").to_owned(),
            "size" => crate::system::resize_client(self.hwnd, e.n("width"), e.n("height")),
            // Full screen is the engine's own window over the monitor (it hides this one).
            "fullscreen" => {}
            _ => {}
        }
    }

    pub fn command(&mut self, v: &Value, context: &ComponentContext<Self>) {
        if v.is_null() {
            return;
        }
        if !remoteman_engine::command(v) {
            remoteman_engine::say(&format!("the engine didn't take {v}"));
        }
        self.refresh(context);
    }

    /// Give the panel its swap chain once the window and the panel's size are known.
    fn try_attach(&mut self) {
        if self.attached || self.hwnd == 0 || self.metrics.is_none() || self.failed.is_some() {
            return;
        }
        match remoteman_engine::attach(self.hwnd) {
            Ok(chain) => {
                // SAFETY: the engine hands over one reference of an IDXGISwapChain1, which this
                // IUnknown now owns.
                let unknown = unsafe { <windows_core::IUnknown as windows_core::Interface>::from_raw(chain.0) };
                let sent = self.panel.request_set_swap_chain(unknown, |result| {
                    if let Err(e) = result {
                        remoteman_engine::say(&format!("the panel refused its swap chain: {e:?}"));
                    }
                });
                self.attached = sent;
                remoteman_engine::say(if sent { "window: the panel has its swap chain" } else { "window: the panel isn't bound yet" });
            }
            Err(e) => {
                remoteman_engine::say(&format!("the panel: {e:#}"));
                self.attached = true;
            }
        }
    }

    /// A dialog closed by its own button, or by Escape. Closed because the state moved on, it
    /// says nothing (Reactor doesn't report those).
    fn dialog_closed(&mut self, result: ContentDialogResult, context: &ComponentContext<Self>) {
        let d = self.state.o("dialog").clone();
        let kind = d.s("kind").to_owned();
        if kind.is_empty() {
            return;
        }
        match (kind.as_str(), result) {
            ("pair", ContentDialogResult::Primary) => {
                // Kept away until the engine says what came of it (waiting, a problem, the code).
                self.dialog_hidden = Some(self.dialog_key.clone());
                let address = self.pair_text.clone().unwrap_or_else(|| d.s("address").to_owned());
                self.command(&cmd("pair-go", json!({"address": address})), context);
            }
            ("address", ContentDialogResult::Primary) => {
                self.dialog_hidden = Some(self.dialog_key.clone());
                let address = self.address_text.clone().unwrap_or_else(|| d.s("address").trim_end_matches(":7791").to_owned());
                self.command(&cmd("address-go", json!({"address": address})), context);
            }
            ("paired" | "forget", ContentDialogResult::Primary) => {
                self.dialog_hidden = Some(self.dialog_key.clone());
                self.command(&cmd("ok", Value::Null), context);
            }
            _ => {
                self.dialog_hidden = Some(self.dialog_key.clone());
                self.command(&cmd("cancel", Value::Null), context);
            }
        }
    }
}
