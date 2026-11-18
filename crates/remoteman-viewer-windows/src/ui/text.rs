//! Text: the design's type sizes, laid out by DirectWrite. The main thread measures with this
//! and the draw thread draws with it, each with its own `Fonts` (DirectWrite's shared factory
//! is the same object underneath, and may be used from any thread).

use std::cell::RefCell;
use std::collections::HashMap;

use anyhow::Result;
use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, HSTRING, w};

/// The design's kinds of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Font {
    /// 14 on 20, the body.
    Body,
    /// The same, semibold: names, headings of groups, accent buttons.
    Strong,
    /// 12 on 16: captions, and the title bar.
    Caption,
    CaptionStrong,
    /// 28 on 36: a page's title.
    Title,
    /// 20 on 28: a dialog's or a status page's title.
    Subtitle,
    /// 11 on 16: tags.
    Tiny,
    /// 10 on 14, spaced out: the names of the figures.
    Key,
    /// 14 on 20, semibold, digits of one width: the figures.
    Value,
    /// 40 on 48: the pairing code.
    Code,
    /// 12 on 16, Consolas: a key's fingerprint.
    Mono,
}

pub struct Spec {
    pub size: f32,
    pub line: f32,
    pub bold: bool,
    pub mono: bool,
    /// Digits of one width.
    pub tabular: bool,
    /// Extra room after each letter, in ems.
    pub spacing: f32,
}

impl Font {
    pub fn spec(self) -> Spec {
        let s = |size, line, bold| Spec { size, line, bold, mono: false, tabular: false, spacing: 0.0 };
        match self {
            Font::Body => s(14.0, 20.0, false),
            Font::Strong => s(14.0, 20.0, true),
            Font::Caption => s(12.0, 16.0, false),
            Font::CaptionStrong => s(12.0, 16.0, true),
            Font::Title => s(28.0, 36.0, true),
            Font::Subtitle => s(20.0, 28.0, true),
            Font::Tiny => s(11.0, 16.0, false),
            Font::Key => Spec { spacing: 0.06, ..s(10.0, 14.0, false) },
            Font::Value => Spec { tabular: true, ..s(14.0, 20.0, true) },
            Font::Code => Spec { tabular: true, spacing: 0.08, ..s(40.0, 48.0, true) },
            Font::Mono => Spec { mono: true, ..s(12.0, 16.0, false) },
        }
    }

    pub fn line(self) -> f32 {
        self.spec().line
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub struct Fonts {
    dw: IDWriteFactory,
    /// "Segoe UI Variable Text" where Windows has it (11), "Segoe UI" where not.
    family: HSTRING,
    formats: RefCell<HashMap<Font, (IDWriteTextFormat, IDWriteInlineObject)>>,
    tabular: IDWriteTypography,
}

impl Fonts {
    pub fn new() -> Result<Self> {
        // SAFETY: DirectWrite calls with out pointers that live through each call.
        unsafe {
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut fonts = None;
            dw.GetSystemFontCollection(&mut fonts, false)?;
            let mut family = HSTRING::from("Segoe UI");
            if let Some(fonts) = fonts {
                for name in [w!("Segoe UI Variable Text"), w!("Segoe UI Variable")] {
                    let (mut index, mut exists) = (0u32, BOOL(0));
                    if fonts.FindFamilyName(name, &mut index, &mut exists).is_ok() && exists.as_bool() {
                        family = HSTRING::from_wide(name.as_wide());
                        break;
                    }
                }
            }
            let tabular = dw.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE { nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES, parameter: 1 })?;
            Ok(Self { dw, family, formats: RefCell::new(HashMap::new()), tabular })
        }
    }

    pub fn family(&self) -> String {
        self.family.to_string()
    }

    fn format(&self, font: Font) -> Result<(IDWriteTextFormat, IDWriteInlineObject)> {
        if let Some(f) = self.formats.borrow().get(&font) {
            return Ok(f.clone());
        }
        let spec = font.spec();
        // SAFETY: the strings live through the call.
        let made = unsafe {
            let weight = if spec.bold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let family = if spec.mono { HSTRING::from("Consolas") } else { self.family.clone() };
            let format = self.dw.CreateTextFormat(&family, None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, spec.size, w!("en-us"))?;
            // Lines as the design's CSS spaces them: the font's own height centred in the line.
            // Segoe UI rises 1.079 em above the baseline and falls 0.251 below it.
            let baseline = (spec.line - 1.33 * spec.size) / 2.0 + 1.079 * spec.size;
            format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, spec.line, baseline)?;
            let dots = self.dw.CreateEllipsisTrimmingSign(&format)?;
            (format, dots)
        };
        self.formats.borrow_mut().insert(font, made.clone());
        Ok(made)
    }

    /// `text` laid out in a box `width` wide. `wrap`: on as many lines as it needs; otherwise
    /// one line, cut short with an ellipsis if it is too long.
    pub fn layout(&self, text: &str, font: Font, width: f32, wrap: bool, align: Align) -> Result<IDWriteTextLayout> {
        let (format, dots) = self.format(font)?;
        let spec = font.spec();
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: DirectWrite calls on a layout made here.
        unsafe {
            let layout = self.dw.CreateTextLayout(&wide, &format, width.max(0.0), 10_000.0)?;
            let all = DWRITE_TEXT_RANGE { startPosition: 0, length: wide.len() as u32 };
            layout.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP })?;
            layout.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            if !wrap && width < 9_000.0 {
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                layout.SetTrimming(&trim, &dots)?;
            }
            if spec.tabular {
                layout.SetTypography(&self.tabular, all)?;
            }
            if spec.spacing != 0.0 {
                if let Ok(spaced) = windows::core::Interface::cast::<IDWriteTextLayout1>(&layout) {
                    spaced.SetCharacterSpacing(0.0, spec.spacing * spec.size, 0.0, all)?;
                }
            }
            Ok(layout)
        }
    }

    /// How wide `text` is on one line.
    pub fn width(&self, text: &str, font: Font) -> f32 {
        let Ok(layout) = self.layout(text, font, 10_000.0, false, Align::Left) else { return 0.0 };
        // SAFETY: a query on a layout held here.
        unsafe { layout.GetMetrics().map_or(0.0, |m| m.widthIncludingTrailingWhitespace.ceil()) }
    }

    /// How tall `text` is when wrapped to `width`.
    pub fn height(&self, text: &str, font: Font, width: f32) -> f32 {
        let Ok(layout) = self.layout(text, font, width, true, Align::Left) else { return font.line() };
        // SAFETY: a query on a layout held here.
        unsafe { layout.GetMetrics().map_or(font.line(), |m| m.lineCount.max(1) as f32 * font.line()) }
    }

    /// Where the caret stands before the `at`th UTF-16 unit of a line of text.
    pub fn caret_x(&self, text: &[u16], font: Font, at: usize) -> f32 {
        let Ok(layout) = self.layout(&String::from_utf16_lossy(text), font, 10_000.0, false, Align::Left) else { return 0.0 };
        let (mut x, mut y, mut m) = (0.0, 0.0, DWRITE_HIT_TEST_METRICS::default());
        // SAFETY: out pointers that live through the call.
        unsafe {
            let _ = layout.HitTestTextPosition(at as u32, false, &mut x, &mut y, &mut m);
        }
        x
    }

    /// Which place between letters is nearest to `x` in a line of text.
    pub fn caret_at(&self, text: &[u16], font: Font, x: f32) -> usize {
        let Ok(layout) = self.layout(&String::from_utf16_lossy(text), font, 10_000.0, false, Align::Left) else { return 0 };
        let (mut trailing, mut inside, mut m) = (BOOL(0), BOOL(0), DWRITE_HIT_TEST_METRICS::default());
        // SAFETY: out pointers that live through the call.
        unsafe {
            let _ = layout.HitTestPoint(x, 4.0, &mut trailing, &mut inside, &mut m);
        }
        ((m.textPosition + if trailing.as_bool() { m.length } else { 0 }) as usize).min(text.len())
    }
}
