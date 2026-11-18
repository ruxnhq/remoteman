//! The design's icons: 16 by 16, drawn as strokes. Each is the SVG of the design's markup, kept
//! as text and read here into lines and arcs.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    Plus,
    Sliders,
    More,
    Computer,
    Check,
    ChevronDown,
    ChevronRight,
    Back,
    Clock,
    Moon,
    NoAnswer,
    Lock,
    Info,
    Keyboard,
    Sound,
    Microphone,
    Camera,
    Folder,
    FullScreen,
    LeaveFullScreen,
    Minimise,
    Maximise,
    Restore,
    Close,
}

pub enum Shape {
    Path(&'static str),
    /// x, y, width, height, corner radius.
    Rect(f32, f32, f32, f32, f32),
    /// centre and radius.
    Circle(f32, f32, f32),
}

use Shape::*;

impl Icon {
    pub fn shapes(self) -> &'static [Shape] {
        match self {
            Icon::Plus => &[Path("M8 3v10M3 8h10")],
            Icon::Sliders => &[Path("M2 5h7M13 5h1M2 11h1M7 11h7"), Circle(11.0, 5.0, 1.6), Circle(5.0, 11.0, 1.6)],
            Icon::More => &[Path("M3.5 8h.01M8 8h.01M12.5 8h.01")],
            Icon::Computer => &[Rect(2.0, 3.0, 12.0, 8.0, 1.5), Path("M6 14h4M8 11v3")],
            Icon::Check => &[Path("M3 8.5l3 3 7-7")],
            Icon::ChevronDown => &[Path("M4 6l4 4 4-4")],
            Icon::ChevronRight => &[Path("M6 4l4 4-4 4")],
            Icon::Back => &[Path("M13 8H3.5M7.5 4l-4 4 4 4")],
            Icon::Clock => &[Circle(8.0, 8.0, 6.0), Path("M8 4.5V8l2.2 1.6")],
            Icon::Moon => &[Path("M13 9.5A5.5 5.5 0 016.5 3 5.5 5.5 0 1013 9.5z")],
            Icon::NoAnswer => &[Rect(2.0, 3.0, 12.0, 8.0, 1.5), Path("M6 14h4M8 11v3M6.5 5.5l3 3M9.5 5.5l-3 3")],
            Icon::Lock => &[Rect(3.5, 7.0, 9.0, 6.5, 1.5), Path("M5.5 7V5a2.5 2.5 0 015 0v2")],
            Icon::Info => &[Circle(8.0, 8.0, 6.0), Path("M8 5v3.5M8 11h.01")],
            Icon::Keyboard => &[Rect(1.5, 4.0, 13.0, 8.0, 1.5), Path("M4 7h.01M6.5 7h.01M9 7h.01M11.5 7h.01M5 9.5h6")],
            Icon::Sound => &[Path("M2.5 6.5h2l3-2.5v8l-3-2.5h-2zM10.5 6a3 3 0 010 4M12.3 4.3a5.5 5.5 0 010 7.4")],
            Icon::Microphone => &[Rect(6.0, 2.0, 4.0, 7.0, 2.0), Path("M3.5 8a4.5 4.5 0 009 0M8 12.5V14")],
            Icon::Camera => &[Rect(1.5, 4.5, 9.0, 7.0, 1.5), Path("M10.5 7l4-2v6l-4-2")],
            Icon::Folder => &[Path("M1.5 4.5h4l1.5 1.5h7.5v6.5h-13z")],
            Icon::FullScreen => &[Path("M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10")],
            Icon::LeaveFullScreen => &[Path("M6 2.5V6H2.5M13.5 6H10V2.5M10 13.5V10h3.5M2.5 10H6v3.5")],
            Icon::Minimise => &[Path("M3.5 8h9")],
            Icon::Maximise => &[Rect(3.5, 3.5, 9.0, 9.0, 0.0)],
            Icon::Restore => &[Rect(3.5, 5.5, 7.0, 7.0, 0.0), Path("M5.5 5.5v-2h7v7h-2")],
            Icon::Close => &[Path("M3.5 3.5l9 9M12.5 3.5l-9 9")],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    /// Radii, turn, the larger arc, clockwise, and where it ends.
    Arc(f32, f32, f32, bool, bool, f32, f32),
    Close,
}

/// Read an SVG path of the kind the design uses: M, L, H, V, A and Z, absolute or relative,
/// numbers run together as SVG allows (`l4 4 4-4`, `a3 3 0 010 4`).
pub fn parse(path: &str) -> Vec<Seg> {
    let s = path.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    let (mut x, mut y, mut sx, mut sy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let mut command = b'M';
    fn skip(s: &[u8], i: &mut usize) {
        while *i < s.len() && (s[*i] == b' ' || s[*i] == b',') {
            *i += 1;
        }
    }
    fn number(s: &[u8], i: &mut usize) -> f32 {
        skip(s, i);
        let start = *i;
        if *i < s.len() && (s[*i] == b'-' || s[*i] == b'+') {
            *i += 1;
        }
        let mut dot = false;
        while *i < s.len() && (s[*i].is_ascii_digit() || (s[*i] == b'.' && !dot)) {
            dot |= s[*i] == b'.';
            *i += 1;
        }
        std::str::from_utf8(&s[start..*i]).ok().and_then(|t| t.parse().ok()).unwrap_or(0.0)
    }
    /// An arc's two flags are single digits, and may have nothing between them and the next
    /// number.
    fn flag(s: &[u8], i: &mut usize) -> bool {
        skip(s, i);
        let set = *i < s.len() && s[*i] == b'1';
        *i += 1;
        set
    }
    loop {
        skip(s, &mut i);
        if i >= s.len() {
            break;
        }
        if s[i].is_ascii_alphabetic() {
            command = s[i];
            i += 1;
        }
        let relative = command.is_ascii_lowercase();
        let (bx, by) = if relative { (x, y) } else { (0.0, 0.0) };
        match command.to_ascii_uppercase() {
            b'M' => {
                x = bx + number(s, &mut i);
                y = by + number(s, &mut i);
                (sx, sy) = (x, y);
                out.push(Seg::Move(x, y));
                // More pairs after a move are lines.
                command = if relative { b'l' } else { b'L' };
            }
            b'L' => {
                x = bx + number(s, &mut i);
                y = by + number(s, &mut i);
                out.push(Seg::Line(x, y));
            }
            b'H' => {
                x = bx + number(s, &mut i);
                out.push(Seg::Line(x, y));
            }
            b'V' => {
                y = by + number(s, &mut i);
                out.push(Seg::Line(x, y));
            }
            b'A' => {
                let (rx, ry, turn) = (number(s, &mut i), number(s, &mut i), number(s, &mut i));
                let (large, clockwise) = (flag(s, &mut i), flag(s, &mut i));
                x = bx + number(s, &mut i);
                y = by + number(s, &mut i);
                out.push(Seg::Arc(rx, ry, turn, large, clockwise, x, y));
            }
            b'Z' => {
                (x, y) = (sx, sy);
                out.push(Seg::Close);
            }
            _ => break,
        }
    }
    out
}
