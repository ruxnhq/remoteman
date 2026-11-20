//! Reading the engine's state, and the pieces every page is made of, in WinUI's own styles
//! (Title, Subtitle, BodyStrong, Caption) and the design's metrics (docs/design/windows).

use serde_json::Value;
use windows_reactor::*;

use crate::app::{Msg, Viewer};

static NULL: Value = Value::Null;

/// The state's fields, with nothing where a field is missing or of another kind.
pub trait J {
    fn o(&self, key: &str) -> &Value;
    fn s(&self, key: &str) -> &str;
    fn b(&self, key: &str) -> bool;
    fn n(&self, key: &str) -> f64;
    fn a(&self, key: &str) -> &[Value];
}

impl J for Value {
    fn o(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
    fn s(&self, key: &str) -> &str {
        self.get(key).and_then(Value::as_str).unwrap_or("")
    }
    fn b(&self, key: &str) -> bool {
        self.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
    fn n(&self, key: &str) -> f64 {
        self.get(key).and_then(Value::as_f64).unwrap_or(0.0)
    }
    fn a(&self, key: &str) -> &[Value] {
        self.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Glyphs of Segoe Fluent Icons, as the C# view had them.
pub mod glyph {
    pub const ADD: &str = "\u{E710}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const DISPLAY: &str = "\u{E7F4}";
    pub const MORE: &str = "\u{E712}";
    pub const EDIT: &str = "\u{E70F}";
    pub const DELETE: &str = "\u{E74D}";
    pub const UNREACHABLE: &str = "\u{E8CD}";
    pub const ASLEEP: &str = "\u{E708}";
    pub const LOCK: &str = "\u{E72E}";
    pub const WARNING: &str = "\u{E7BA}";
    pub const CLOSE: &str = "\u{E711}";
    pub const SOUND: &str = "\u{E767}";
    pub const MIC: &str = "\u{E720}";
    pub const CAMERA: &str = "\u{E714}";
    pub const SHARE: &str = "\u{E72D}";
    pub const KEYBOARD: &str = "\u{E765}";
    pub const PICTURE: &str = "\u{EB9F}";
    pub const FULL_SCREEN: &str = "\u{E740}";
    pub const LEAVE_FULL_SCREEN: &str = "\u{E73F}";
    pub const SEND: &str = "\u{E898}";
    pub const MINIMISE: &str = "\u{E921}";
    pub const MAXIMISE: &str = "\u{E922}";
    pub const RESTORE: &str = "\u{E923}";
    pub const WINDOW_CLOSE: &str = "\u{E8BB}";
}

/// A command for the engine: `{"do": WHAT, ...more}`.
pub fn cmd(what: &str, more: Value) -> Value {
    let mut v = if more.is_object() { more } else { Value::Object(Default::default()) };
    v["do"] = Value::String(what.into());
    v
}

/// A button's press, as a command for the engine.
pub fn act(context: &ViewContext<Viewer>, what: &str, more: Value) -> Callback<()> {
    context.message(Msg::Do(cmd(what, more)))
}

/// WinUI's Title style.
pub fn title(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_family("Segoe UI Variable Display").font_size(28.0).font_weight(FontWeight::SEMI_BOLD)
}

/// WinUI's Subtitle style.
pub fn subtitle(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_family("Segoe UI Variable Display").font_size(20.0).font_weight(FontWeight::SEMI_BOLD)
}

/// WinUI's BodyStrong style.
pub fn strong(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_weight(FontWeight::SEMI_BOLD)
}

/// WinUI's Caption style in the secondary text colour.
pub fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(12.0).foreground(ThemeBrush::SecondaryText).text_wrapping(TextWrapping::Wrap)
}

pub fn icon(glyph: &str, size: f64) -> FontIcon {
    FontIcon::new().glyph(glyph).font_size(size)
}

/// A button with words (and a glyph before them).
pub fn button(text: &str, on: Callback<()>, accent: bool, glyph: Option<&str>) -> Button {
    let content: View = match glyph {
        Some(g) => StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children((icon(g, 16.0), TextBlock::new().text(text))).into(),
        None => TextBlock::new().text(text).into(),
    };
    let b = Button::new().content(content).automation_name(text).on_click(on);
    if accent { b.style(ButtonStyle::Accent) } else { b }
}

/// A square button with a glyph, named and explained by its tip.
pub fn square(glyph: &str, tip: &str, on: Option<Callback<()>>) -> Button {
    let b = Button::new().content(icon(glyph, 16.0)).min_width(32.0).height(32.0).automation_name(tip);
    match on {
        Some(on) => b.on_click(on),
        None => b,
    }
}

/// A card: WinUI's card fill and stroke, rounded.
pub fn card(child: impl Into<View>, padding: Thickness) -> Border {
    Border::new().background(ThemeBrush::CardBackground).border_brush(ThemeBrush::CardStroke).border_thickness(1.0).corner_radius(6.0).padding(padding).content(child)
}

/// A group of settings rows: its heading and its card.
pub fn group(heading: &str, rows: Vec<View>) -> View {
    let mut inside: Vec<View> = Vec::new();
    for (n, row) in rows.into_iter().enumerate() {
        if n > 0 {
            inside.push(Border::new().height(1.0).background(ThemeBrush::CardStroke).into());
        }
        inside.push(row);
    }
    StackPanel::new().spacing(8.0).children((strong(heading), card(StackPanel::new().children(inside), Thickness::uniform(0.0)))).into()
}

/// A settings row: its title, what it says under that, and its control; greyed with a tag
/// ("Not available yet") where the product can't do it yet.
pub fn row(title: &str, caption_text: &str, control: Option<View>, on: bool, tag: Option<&str>) -> View {
    let mut words: Vec<View> = vec![TextBlock::new().text(title).text_wrapping(TextWrapping::Wrap).into()];
    if !caption_text.is_empty() {
        words.push(caption(caption_text).into());
    }
    let mut cells: Vec<View> = vec![StackPanel::new().vertical_alignment(VerticalAlignment::Center).opacity(if on { 1.0 } else { 0.5 }).children(words).into()];
    if !on {
        let tag = tag.unwrap_or("Not available yet");
        cells.push(
            Border::new()
                .grid_column(1)
                .border_thickness(1.0)
                .border_brush(ThemeBrush::SystemCaution)
                .corner_radius(9.0)
                .padding(Thickness::new(8.0, 1.0, 8.0, 1.0))
                .vertical_alignment(VerticalAlignment::Center)
                .content(TextBlock::new().text(tag).font_size(11.0).foreground(ThemeBrush::SystemCaution))
                .into(),
        );
    }
    if let Some(control) = control {
        cells.push(Border::new().grid_column(2).vertical_alignment(VerticalAlignment::Center).content(control).into());
    }
    Border::new()
        .padding(Thickness::new(16.0, 10.0, 16.0, 10.0))
        .min_height(60.0)
        .content(Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(14.0).children(cells))
        .into()
}

/// Words in the secondary colour, as a fixed value of a row.
pub fn fixed(text: &str) -> View {
    TextBlock::new().text(text).foreground(ThemeBrush::SecondaryText).into()
}
