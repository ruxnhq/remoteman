//! The computer's picture: a `SwapChainPanel` the engine draws into, inside a `Border` that takes
//! the keyboard, so every key (Tab, the arrows, Alt, F10, Space included) goes to the computer
//! rather than to XAML's focus and menus. The pointer, its buttons, the wheel and fingers go to
//! the engine in the panel's pixels, as the C# view's `PictureView` sent them.
//!
//! The handlers are routed (they run while WinUI delivers the input, and say it was handled) and
//! made once, so a render doesn't subscribe them again.

use std::cell::Cell;
use std::rc::Rc;

use windows_reactor::*;

use crate::app::Msg;

pub struct Picture {
    pub hwnd: Rc<Cell<isize>>,
    /// The panel's composition scale (the app's units to pixels).
    pub scale: Rc<Cell<(f32, f32)>>,
    border: ElementRef<Border>,
    pressed: RoutedCallback<PointerEventInfo>,
    moved: RoutedCallback<PointerEventInfo>,
    released: RoutedCallback<PointerEventInfo>,
    wheel: RoutedCallback<PointerEventInfo>,
    lost: Callback<()>,
    key_down: RoutedCallback<KeyEventInfo>,
    key_up: RoutedCallback<KeyEventInfo>,
    got_focus: Callback<FocusEventInfo>,
    lost_focus: Callback<FocusEventInfo>,
}

/// What the handlers share.
struct Shared {
    scale: Rc<Cell<(f32, f32)>>,
    buttons: Cell<u32>,
    /// The pointer was at the top edge when last seen (the floating pill comes out there).
    top: Cell<bool>,
    sender: LocalSender<Msg>,
    border: ElementRef<Border>,
}

impl Shared {
    fn pixels(&self, info: &PointerEventInfo) -> (i32, i32) {
        let (sx, sy) = self.scale.get();
        ((info.x * sx as f64).round() as i32, (info.y * sy as f64).round() as i32)
    }

    /// `phase`: 0 down, 1 move, 2 up, as the engine counts a finger's.
    fn pointer(&self, info: PointerEventInfo, phase: i32) -> bool {
        let (x, y) = self.pixels(&info);
        // For tests: REMOTEMAN_DEBUG_POINTER=1 puts each pointer event in the log.
        static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *DEBUG.get_or_init(|| std::env::var("REMOTEMAN_DEBUG_POINTER").is_ok()) {
            remoteman_engine::say(&format!("pointer: phase {phase} at {:.1},{:.1} device {:?} buttons {}{}{}", info.x, info.y, info.device, info.is_left_button_pressed as u8, info.is_right_button_pressed as u8, info.is_middle_button_pressed as u8));
        }
        // A finger: the engine sends it as a touch if that is wanted, otherwise it is the pointer.
        if info.device == PointerDevice::Touch && remoteman_engine::touch(info.pointer_id, phase, x, y) {
            return true;
        }
        let top = info.y <= 3.0;
        if top && !self.top.get() {
            let _ = self.sender.send(Msg::BarPeek);
        }
        self.top.set(top);
        remoteman_engine::pointer(x, y);
        let now = info.is_left_button_pressed as u32 | (info.is_right_button_pressed as u32) << 1 | (info.is_middle_button_pressed as u32) << 2;
        let before = self.buttons.replace(now);
        for b in 0..3 {
            let bit = 1 << b;
            if now & bit != before & bit {
                remoteman_engine::button(b, now & bit != 0, x, y);
            }
        }
        true
    }

    /// Every button the computer was told is down goes up (the pointer's capture was lost).
    fn let_go(&self) {
        let before = self.buttons.replace(0);
        for b in 0..3 {
            if before & (1 << b) != 0 {
                remoteman_engine::button(b, false, 0, 0);
            }
        }
    }

    fn key(&self, info: KeyEventInfo, down: bool) -> bool {
        let mut scancode = info.status.scan_code;
        if scancode == 0 {
            // Keys made up by software may only name the key's meaning.
            scancode = crate::system::scancode_of(info.key.0 as u32);
        }
        // A repeat: the key was down already.
        let repeat = down && info.status.was_down;
        remoteman_engine::key(scancode, info.status.is_extended, down, repeat)
    }
}

impl Picture {
    pub fn new(sender: LocalSender<Msg>) -> Self {
        let scale = Rc::new(Cell::new((1.0, 1.0)));
        let border = ElementRef::<Border>::new();
        let s = Rc::new(Shared { scale: scale.clone(), buttons: Cell::new(0), top: Cell::new(false), sender, border: border.clone() });
        let (a, b, c, d, e, f, g) = (s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone(), s.clone());
        Self {
            hwnd: Rc::new(Cell::new(0)),
            scale,
            border,
            pressed: RoutedCallback::new(move |info| {
                let _ = a.border.request_focus();
                a.pointer(info, 0)
            }),
            moved: RoutedCallback::new(move |info| b.pointer(info, 1)),
            released: RoutedCallback::new(move |info| c.pointer(info, 2)),
            wheel: RoutedCallback::new(move |info: PointerEventInfo| {
                remoteman_engine::wheel(info.wheel_delta as f32 / 120.0, info.is_horizontal_wheel);
                true
            }),
            lost: Callback::new(move |()| d.let_go()),
            key_down: RoutedCallback::new(move |info| e.key(info, true)),
            key_up: RoutedCallback::new(move |info| f.key(info, false)),
            got_focus: Callback::new(move |_| remoteman_engine::focus(true)),
            lost_focus: Callback::new(move |_| {
                g.let_go();
                remoteman_engine::focus(false);
            }),
        }
    }

    /// The panel inside its border; seen, and taking the keyboard, only during a session. While
    /// the pointer is captured this PC's cursor isn't seen over it (the engine draws the computer's
    /// into the picture): WinUI sets its own cursor over its content, whatever the window answers
    /// to WM_SETCURSOR, so the border wears a blank one (the exe's cursor resource 1).
    pub fn view(&self, panel: &ElementRef<SwapChainPanel>, session: bool, captured: bool) -> Border {
        let border = Border::new()
            .element_ref(&self.border)
            .is_tab_stop(session)
            .allow_focus_on_interaction(session)
            .capture_pointer_on_press(true)
            .opacity(if session { 1.0 } else { 0.0 })
            .automation_name("The computer's picture")
            .on_pointer_pressed(self.pressed.clone())
            .on_pointer_moved(self.moved.clone())
            .on_pointer_released(self.released.clone())
            .on_pointer_wheel_changed(self.wheel.clone())
            .on_pointer_capture_lost(self.lost.clone())
            .on_pointer_canceled(self.lost.clone())
            .on_preview_key_down(self.key_down.clone())
            .on_key_up(self.key_up.clone())
            .on_got_focus(self.got_focus.clone())
            .on_lost_focus(self.lost_focus.clone())
            .content(SwapChainPanel::new().element_ref(panel));
        let border = if captured { border.pointer_cursor(InputSystemCursorShape::Hidden) } else { border };
        // Black behind the picture (the bands that keep its shape are black too), and nothing
        // to hit when there is no session.
        if session { border.background(Color::rgb(0, 0, 0)) } else { border }
    }

    /// Give the picture the keyboard (a session has started, or a menu has closed).
    pub fn focus(&self) {
        let _ = self.border.request_focus();
    }
}
