//! The interface kit: what the app's screens are built from. There is no toolkit under it, only
//! Direct2D and DirectWrite.
//!
//! How it works: after anything changes, the app describes the whole window again, from its own
//! state, by calling the methods of [`Ui`]. That leaves two lists: what to draw ([`Op`]), which
//! goes to the draw thread as a [`Frame`], and where the pointer and the keyboard can do
//! something ([`Region`]), which stays with the main thread. A region carries the message the
//! app gets when it is pressed. Nothing here keeps widgets alive between two descriptions; what
//! must last (which control is hovered, pressed or focused) is in [`Input`], by the control's
//! [`Id`].
//!
//! All measures are the design's: pixels at 100 % scaling. The draw thread scales.

pub mod icons;
pub mod paint;
pub mod text;
pub mod theme;

use crate::app::Msg;
use icons::Icon;
pub use text::{Align, Font, Fonts};
pub use theme::{Color, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub const fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

impl Rect {
    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Smaller by `d` on every side (larger, if `d` is negative).
    pub fn inset(self, d: f32) -> Rect {
        rect(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }

    pub fn overlap(self, o: Rect) -> Rect {
        let (x, y) = (self.x.max(o.x), self.y.max(o.y));
        rect(x, y, (self.right().min(o.right()) - x).max(0.0), (self.bottom().min(o.bottom()) - y).max(0.0))
    }
}

/// One thing to draw. The draw thread goes through them in order.
#[derive(Clone, Debug)]
pub enum Op {
    Fill { r: Rect, radius: f32, color: Color },
    /// A line `width` thick just inside the edge of `r`.
    Stroke { r: Rect, radius: f32, color: Color, width: f32 },
    /// Text from the top of `r`, in a box as wide as `r`.
    Text { r: Rect, text: String, font: Font, color: Color, align: Align, wrap: bool },
    /// `size`: how wide the icon's 16 units are drawn. `stroke` is in those units.
    Icon { icon: Icon, x: f32, y: f32, size: f32, color: Color, stroke: f32 },
    /// The product's mark.
    Logo { x: f32, y: f32, size: f32 },
    /// A circle of track with a quarter of it coloured, turned by `turn` (0 to 1).
    Ring { cx: f32, cy: f32, r: f32, width: f32, track: Color, color: Color, turn: f32 },
    /// A filled circle, or with `ring` above 0 a circle's outline that thick, inside `r`.
    Dot { cx: f32, cy: f32, r: f32, color: Color, ring: f32 },
    /// The soft shadow under a flyout or a dialog that fills `r`.
    Shadow { r: Rect, radius: f32, strength: f32 },
    Clip(Rect),
    Unclip,
}

/// What the draw thread is handed: the window's interface, whole.
pub struct Frame {
    pub ops: Vec<Op>,
    /// Device pixels to one of the design's.
    pub scale: f32,
    /// Where the other computer's picture goes, in device pixels (left, top, width, height);
    /// `None`: nowhere, the interface covers the window.
    pub video: Option<(i32, i32, i32, i32)>,
    /// Counts up; a picture of the window waits for the frame it asked for.
    pub serial: u64,
}

pub type Id = u64;

/// A control's name as a number.
pub fn id(name: &str) -> Id {
    name.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3)) | 1
}

/// The `n`th of several controls of one name: as `id("name#n")`.
pub fn idn(name: &str, n: usize) -> Id {
    id(&format!("{name}#{n}"))
}

/// A text field's contents, as UTF-16, with the caret and the other end of the selection.
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub text: Vec<u16>,
    pub caret: usize,
    pub anchor: usize,
}

impl Field {
    pub fn with(text: &str) -> Self {
        let text: Vec<u16> = text.encode_utf16().collect();
        Self { caret: text.len(), anchor: 0, text }
    }

    pub fn string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// Put `new` where the selection is (or at the caret).
    pub fn insert(&mut self, new: &[u16]) {
        let (from, to) = self.selection();
        self.text.splice(from..to, new.iter().copied());
        self.caret = from + new.len();
        self.anchor = self.caret;
    }

    pub fn selected(&self) -> String {
        let (from, to) = self.selection();
        String::from_utf16_lossy(&self.text[from..to])
    }

    /// Move the caret; `keep`: the selection stretches with it (Shift is held).
    pub fn go(&mut self, to: usize, keep: bool) {
        self.caret = to.min(self.text.len());
        if !keep {
            self.anchor = self.caret;
        }
    }

    /// Backspace (`forward` false) or Delete.
    pub fn erase(&mut self, forward: bool) {
        if self.caret == self.anchor {
            if forward && self.caret < self.text.len() {
                self.anchor = self.caret + 1;
            } else if !forward && self.caret > 0 {
                self.anchor = self.caret - 1;
            }
        }
        self.insert(&[]);
    }
}

/// What a region does with the pointer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Sends its message when pressed and let go.
    Button,
    /// A text field: a press puts the caret there.
    Field,
    /// Sends `Msg::Slide` with how far along the pointer is, while held.
    Slider,
    /// Takes the pointer and does nothing with it, or sends its message at once when pressed
    /// (what is behind a menu).
    Block,
    /// Part of the window's title bar: dragging it moves the window.
    Drag,
    /// The maximise button, which Windows wants to know about (for the snap layouts).
    Maximise,
}

pub struct Region {
    pub id: Id,
    pub rect: Rect,
    pub kind: Kind,
    pub msg: Option<Msg>,
    /// Which sheet it is on: the page is 0, a dialog or menu over it 1, and so on. Only the
    /// top sheet's controls take the keyboard.
    pub layer: u8,
    /// Tab stops here.
    pub focus: bool,
    pub enabled: bool,
    pub tip: String,
}

/// What lasts between two descriptions of the window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    /// The control under the pointer, the one a button went down on, the one with the keyboard.
    pub hot: Id,
    pub down: Id,
    pub focus: Id,
    /// The focus was moved with the keyboard, so it is shown.
    pub keys: bool,
    /// Whether the caret of a text field is in the shown half of its blink.
    pub caret: bool,
}

/// Whether a control can be used, and what it says if not.
#[derive(Clone, Copy, Debug)]
pub struct En {
    pub on: bool,
    pub why: &'static str,
}

pub const ON: En = En { on: true, why: "" };

pub fn off(why: &'static str) -> En {
    En { on: false, why }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub hot: bool,
    pub down: bool,
    pub focus: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    Plain,
    Accent,
    /// For something that destroys: the label in the warning red.
    Bad,
}

pub struct Ui<'a> {
    pub t: Theme,
    pub fonts: &'a Fonts,
    pub input: Input,
    pub ops: Vec<Op>,
    pub regions: Vec<Region>,
    /// The window's size.
    pub w: f32,
    pub h: f32,
    /// How far round the progress ring is, 0 to 1.
    pub turn: f32,
    layer: u8,
    clips: Vec<Rect>,
}

impl<'a> Ui<'a> {
    pub fn new(t: Theme, fonts: &'a Fonts, input: Input, w: f32, h: f32, turn: f32) -> Self {
        Self { t, fonts, input, ops: Vec::new(), regions: Vec::new(), w, h, turn, layer: 0, clips: Vec::new() }
    }

    pub fn whole(&self) -> Rect {
        rect(0.0, 0.0, self.w, self.h)
    }

    // Drawing.

    pub fn fill(&mut self, r: Rect, radius: f32, color: Color) {
        self.ops.push(Op::Fill { r, radius, color });
    }

    pub fn stroke(&mut self, r: Rect, radius: f32, color: Color) {
        self.ops.push(Op::Stroke { r, radius, color, width: 1.0 });
    }

    /// A hairline across.
    pub fn hline(&mut self, x: f32, y: f32, w: f32) {
        let color = self.t.line;
        self.fill(rect(x, y, w, 1.0), 0.0, color);
    }

    /// One line of text from `x`, `y`; says how wide it is.
    pub fn label(&mut self, x: f32, y: f32, text: &str, font: Font, color: Color) -> f32 {
        let w = self.fonts.width(text, font);
        self.ops.push(Op::Text { r: rect(x, y, w + 2.0, font.line()), text: text.to_owned(), font, color, align: Align::Left, wrap: false });
        w
    }

    /// One line of text in the middle of `r`'s height, cut short if `r` is too narrow.
    pub fn label_in(&mut self, r: Rect, text: &str, font: Font, color: Color, align: Align) {
        let y = r.y + ((r.h - font.line()) / 2.0).round();
        self.ops.push(Op::Text { r: rect(r.x, y, r.w, font.line()), text: text.to_owned(), font, color, align, wrap: false });
    }

    /// Text wrapped to `w`; says how tall it came out.
    pub fn para(&mut self, x: f32, y: f32, w: f32, text: &str, font: Font, color: Color, align: Align) -> f32 {
        let h = self.fonts.height(text, font, w);
        self.ops.push(Op::Text { r: rect(x, y, w, h), text: text.to_owned(), font, color, align, wrap: true });
        h
    }

    /// An icon at its usual size, 16.
    pub fn icon(&mut self, icon: Icon, x: f32, y: f32, color: Color) {
        self.ops.push(Op::Icon { icon, x, y, size: 16.0, color, stroke: if icon == Icon::More { 2.0 } else { 1.3 } });
    }

    pub fn dot(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        self.ops.push(Op::Dot { cx, cy, r, color, ring: 0.0 });
    }

    pub fn clip(&mut self, r: Rect) {
        let r = self.clips.last().map_or(r, |c| c.overlap(r));
        self.clips.push(r);
        self.ops.push(Op::Clip(r));
    }

    pub fn unclip(&mut self) {
        self.clips.pop();
        self.ops.push(Op::Unclip);
    }

    /// What follows is on a sheet over what came before: a dialog or a menu.
    pub fn raise(&mut self) {
        self.layer += 1;
    }

    // Places the pointer and the keyboard act on.

    pub fn region(&mut self, id: Id, r: Rect, kind: Kind, msg: Option<Msg>, en: En, tip: &str, focus: bool) -> State {
        let r = self.clips.last().map_or(r, |c| c.overlap(r));
        if r.w <= 0.0 || r.h <= 0.0 {
            return State { hot: false, down: false, focus: false };
        }
        let tip = if en.on { tip } else { en.why };
        self.regions.push(Region { id, rect: r, kind, msg, layer: self.layer, focus: focus && en.on, enabled: en.on, tip: tip.to_owned() });
        let i = &self.input;
        State { hot: en.on && i.hot == id && (i.down == 0 || i.down == id), down: en.on && i.down == id && i.hot == id, focus: en.on && i.focus == id }
    }

    /// The ring round a control that has the keyboard, if the keyboard put it there.
    pub fn focus_ring(&mut self, r: Rect, radius: f32, state: State) {
        if state.focus && self.input.keys {
            let color = self.t.tx;
            self.ops.push(Op::Stroke { r: r.inset(-3.0), radius: radius + 3.0, color, width: 2.0 });
        }
    }

    /// The fill of a quiet control (one with no fill of its own) under the pointer.
    fn wash(&self, state: State) -> Option<Color> {
        if state.down {
            Some(self.t.sub.fade(1.7))
        } else if state.hot {
            Some(self.t.sub)
        } else {
            None
        }
    }

    // Controls.

    pub fn button_width(&self, label: &str, look: Look, icon: bool) -> f32 {
        let font = if look == Look::Accent { Font::Strong } else { Font::Body };
        28.0 + self.fonts.width(label, font) + if icon { 24.0 } else { 0.0 }
    }

    /// The design's button: 32 tall, with a label and perhaps an icon before it.
    #[allow(clippy::too_many_arguments)]
    pub fn button(&mut self, id: Id, r: Rect, label: &str, look: Look, icon: Option<Icon>, msg: Msg, en: En) {
        let state = self.region(id, r, Kind::Button, Some(msg), en, "", true);
        let t = self.t;
        let (font, mut ink) = match look {
            Look::Accent => (Font::Strong, t.on),
            Look::Bad => (Font::Body, t.bad),
            Look::Plain => (Font::Body, t.tx),
        };
        if look == Look::Accent {
            let fill = if !en.on {
                t.tx.fade(0.2)
            } else if state.down {
                t.ac.fade(0.8)
            } else if state.hot {
                t.ac.fade(0.9)
            } else {
                t.ac
            };
            self.fill(r, 4.0, fill);
            if !en.on {
                ink = t.card;
            }
        } else {
            self.fill(r, 4.0, t.card);
            if let Some(wash) = self.wash(state) {
                self.fill(r, 4.0, wash);
            }
            self.stroke(r, 4.0, t.line);
            if !en.on {
                ink = ink.fade(0.4);
            }
        }
        let text_w = self.fonts.width(label, font);
        let inner = text_w + if icon.is_some() { 24.0 } else { 0.0 };
        let mut x = r.x + ((r.w - inner) / 2.0).round().max(8.0);
        if let Some(icon) = icon {
            self.icon(icon, x, r.y + (r.h - 16.0) / 2.0, ink);
            x += 24.0;
        }
        self.label_in(rect(x, r.y, (r.right() - 8.0 - x).max(0.0), r.h), label, font, ink, Align::Left);
        self.focus_ring(r, 4.0, state);
    }

    /// A square button with only an icon; `tip` says what it is.
    pub fn icon_button(&mut self, id: Id, r: Rect, icon: Icon, tip: &str, msg: Msg, en: En) {
        let state = self.region(id, r, Kind::Button, Some(msg), en, tip, true);
        let t = self.t;
        self.fill(r, 4.0, t.card);
        if let Some(wash) = self.wash(state) {
            self.fill(r, 4.0, wash);
        }
        self.stroke(r, 4.0, t.line);
        let ink = if en.on { t.tx } else { t.tx.fade(0.4) };
        self.icon(icon, r.x + (r.w - 16.0) / 2.0, r.y + (r.h - 16.0) / 2.0, ink);
        self.focus_ring(r, 4.0, state);
    }

    /// The switch: 40 by 20.
    pub fn toggle(&mut self, id: Id, x: f32, y: f32, on: bool, tip: &str, msg: Msg, en: En) {
        let r = rect(x, y, 40.0, 20.0);
        let state = self.region(id, r, Kind::Button, Some(msg), en, tip, true);
        let t = self.t;
        let dim = if en.on { 1.0 } else { 0.4 };
        if on {
            let fill = if state.down { t.ac.fade(0.8) } else if state.hot { t.ac.fade(0.9) } else { t.ac };
            self.fill(r, 10.0, fill.fade(dim));
            self.dot(x + 30.0, y + 10.0, 6.0, t.on);
        } else {
            if let Some(wash) = self.wash(state) {
                self.fill(r, 10.0, wash);
            }
            self.stroke(r, 10.0, t.t2.fade(dim));
            self.dot(x + 10.0, y + 10.0, 6.0, t.t2.fade(dim));
        }
        self.focus_ring(r, 10.0, state);
    }

    /// The round mark of one choice among several: 16 across. Only the picture; the line it is
    /// on is the control.
    pub fn radio(&mut self, x: f32, y: f32, on: bool, dim: f32) {
        let t = self.t;
        if on {
            self.ops.push(Op::Dot { cx: x + 8.0, cy: y + 8.0, r: 8.0, color: t.ac.fade(dim), ring: 4.0 });
        } else {
            self.ops.push(Op::Dot { cx: x + 8.0, cy: y + 8.0, r: 8.0, color: t.t2.fade(dim), ring: 1.0 });
        }
    }

    /// The box that shows one choice and opens the list of them.
    pub fn dropdown(&mut self, id: Id, r: Rect, label: &str, msg: Msg, en: En) {
        let state = self.region(id, r, Kind::Button, Some(msg), en, "", true);
        let t = self.t;
        self.fill(r, 4.0, t.card);
        if let Some(wash) = self.wash(state) {
            self.fill(r, 4.0, wash);
        }
        self.stroke(r, 4.0, t.line);
        let ink = if en.on { t.tx } else { t.tx.fade(0.4) };
        self.label_in(rect(r.x + 10.0, r.y, r.w - 46.0, r.h), label, Font::Body, ink, Align::Left);
        self.icon(Icon::ChevronDown, r.right() - 26.0, r.y + 8.0, ink);
        self.focus_ring(r, 4.0, state);
    }

    pub fn card(&mut self, r: Rect) {
        let t = self.t;
        self.fill(r, 6.0, t.card);
        self.stroke(r, 6.0, t.line);
    }

    /// A small outlined word.
    pub fn tag(&mut self, x: f32, y: f32, text: &str) -> f32 {
        let w = self.fonts.width(text, Font::Tiny) + 18.0;
        let color = self.t.warn;
        self.stroke(rect(x, y, w, 20.0), 9.0, color);
        self.label_in(rect(x, y, w, 20.0), text, Font::Tiny, color, Align::Center);
        w
    }

    /// The ring that turns while something is waited for: 40 across.
    pub fn ring(&mut self, cx: f32, cy: f32, size: f32) {
        let (track, color, turn) = (self.t.line, self.t.ac, self.turn);
        self.ops.push(Op::Ring { cx, cy, r: size * 0.4, width: size * 0.1, track, color, turn });
    }

    /// A one-line text field.
    pub fn field(&mut self, id: Id, r: Rect, field: &Field, en: En) {
        let state = self.region(id, r, Kind::Field, None, en, "", true);
        let t = self.t;
        self.fill(r, 4.0, t.card);
        self.stroke(r, 4.0, t.line);
        let inner = rect(r.x + 10.0, r.y + 1.0, r.w - 20.0, r.h - 2.0);
        let caret = self.fonts.caret_x(&field.text, Font::Body, field.caret);
        // Keep the caret in sight when the text is longer than the field.
        let shift = (caret - inner.w + 2.0).max(0.0);
        self.clip(inner);
        let y = r.y + (r.h - 20.0) / 2.0;
        if state.focus && field.caret != field.anchor {
            let (from, to) = field.selection();
            let (a, b) = (self.fonts.caret_x(&field.text, Font::Body, from), self.fonts.caret_x(&field.text, Font::Body, to));
            self.fill(rect(inner.x + a - shift, y, b - a, 20.0), 0.0, t.ac.fade(0.35));
        }
        let ink = if en.on { t.tx } else { t.tx.fade(0.4) };
        self.ops.push(Op::Text { r: rect(inner.x - shift, y, 10_000.0, 20.0), text: field.string(), font: Font::Body, color: ink, align: Align::Left, wrap: false });
        if state.focus && self.input.caret {
            self.fill(rect(inner.x + caret - shift, y + 1.0, 1.0, 18.0), 0.0, t.tx);
        }
        self.unclip();
        if state.focus {
            // The design's mark of the field being typed in: a thicker accent line under it.
            self.clip(rect(r.x, r.bottom() - 2.0, r.w, 2.0));
            self.fill(r, 4.0, t.ac);
            self.unclip();
        }
    }

    /// A flyout's or dialog's body: shadow, fill, hairline.
    pub fn panel(&mut self, r: Rect, radius: f32, fill: Color) {
        let (line, strength) = (self.t.line, self.t.shadow);
        self.ops.push(Op::Shadow { r, radius, strength });
        self.fill(r, radius, fill);
        self.stroke(r, radius, line);
    }
}
