//! The design's colours, for light and for dark. The names are the design's own (`--bg`,
//! `--pane`, `--card` and so on in its style sheets).

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

pub const fn rgb(hex: u32) -> Color {
    Color(((hex >> 16) & 0xFF) as f32 / 255.0, ((hex >> 8) & 0xFF) as f32 / 255.0, (hex & 0xFF) as f32 / 255.0, 1.0)
}

pub const fn rgba(hex: u32, alpha: f32) -> Color {
    let c = rgb(hex);
    Color(c.0, c.1, c.2, alpha)
}

impl Color {
    pub const CLEAR: Color = Color(0.0, 0.0, 0.0, 0.0);
    pub const WHITE: Color = rgb(0xFFFFFF);

    /// The same colour, fainter.
    pub fn fade(self, by: f32) -> Color {
        Color(self.0, self.1, self.2, self.3 * by)
    }

    /// This colour with `over` laid on it.
    #[allow(dead_code)]
    pub fn under(self, over: Color) -> Color {
        let a = over.3;
        Color(self.0 * (1.0 - a) + over.0 * a, self.1 * (1.0 - a) + over.1 * a, self.2 * (1.0 - a) + over.2 * a, self.3 + (1.0 - self.3) * a)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// The window.
    pub bg: Color,
    /// Flyouts and the foot of a dialog.
    pub pane: Color,
    pub card: Color,
    /// Hairlines.
    pub line: Color,
    pub tx: Color,
    /// Quieter text.
    pub t2: Color,
    /// The accent, and what is written on it.
    pub ac: Color,
    pub on: Color,
    /// The accent as text.
    pub at: Color,
    /// A subtle fill: hovered and chosen things.
    pub sub: Color,
    pub ok: Color,
    pub warn: Color,
    pub bad: Color,
    /// How dark a shadow is.
    pub shadow: f32,
}

pub const LIGHT: Theme = Theme {
    dark: false,
    bg: rgb(0xF3F3F3),
    pane: rgb(0xFAFAFA),
    card: rgb(0xFFFFFF),
    line: rgba(0x000000, 0.10),
    tx: rgb(0x1B1B1B),
    t2: rgb(0x5A5A5A),
    ac: rgb(0x2458F5),
    on: rgb(0xFFFFFF),
    at: rgb(0x1D4BDB),
    sub: rgba(0x000000, 0.05),
    ok: rgb(0x0F7B0F),
    warn: rgb(0x8A5200),
    bad: rgb(0xC42B1C),
    shadow: 0.20,
};

pub const DARK: Theme = Theme {
    dark: true,
    bg: rgb(0x202020),
    pane: rgb(0x2A2A2A),
    card: rgb(0x2D2D2D),
    line: rgba(0xFFFFFF, 0.11),
    tx: rgb(0xFFFFFF),
    t2: rgb(0xC8C8C8),
    ac: rgb(0x8AB0FF),
    on: rgb(0x0B1437),
    at: rgb(0x9DBCFF),
    sub: rgba(0xFFFFFF, 0.07),
    ok: rgb(0x6CCB5F),
    warn: rgb(0xF5C26B),
    bad: rgb(0xFF99A4),
    shadow: 0.55,
};

/// The bar that floats over the picture, and its menus: dark whatever the system says, since
/// what is under it is somebody's desktop.
pub const OVER: Theme = Theme { line: rgba(0xFFFFFF, 0.13), sub: rgba(0xFFFFFF, 0.08), ..DARK };

/// The floating bar's own fill.
pub const OVER_BAR: Color = rgba(0x202020, 0.94);
/// The figures panel.
pub const FIGURES_FILL: Color = rgba(0x121216, 0.86);
pub const FIGURES_LINE: Color = rgba(0xFFFFFF, 0.14);
pub const FIGURES_KEY: Color = rgb(0xB9C0CC);
/// The product's own blue, in the logo whatever the theme.
pub const LOGO: Color = rgb(0x2458F5);
