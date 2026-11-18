//! The draw thread's half of the kit: a [`Frame`]'s list drawn with Direct2D into a texture of
//! the window's size, which the draw thread then lays over the picture.

use std::collections::HashMap;

use anyhow::{Context, Result};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::core::Interface;
use windows_numerics::{Matrix3x2, Vector2};

use super::icons::{Icon, Seg, Shape, parse};
use super::theme::{Color, LOGO};
use super::{Fonts, Frame, Op, Rect};

pub struct Painter {
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
    /// Round ends and corners, as the design's icons are stroked.
    round: ID2D1StrokeStyle,
    fonts: Fonts,
    /// The paths of each icon, made once.
    paths: HashMap<(Icon, usize), ID2D1PathGeometry>,
    logo: Option<ID2D1PathGeometry>,
}

fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.0, g: c.1, b: c.2, a: c.3 }
}

fn rect_f(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect_f(r), radiusX: radius, radiusY: radius }
}

const UNIT: Matrix3x2 = Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 };

impl Painter {
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        // SAFETY: Direct2D calls with structures that live through each call.
        unsafe {
            // Only the draw thread draws with it.
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).context("create the Direct2D factory")?;
            let dxgi: IDXGIDevice = device.cast()?;
            let d2d = factory.CreateDevice(&dxgi).context("a Direct2D device on the Direct3D one")?;
            let dc = d2d.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
            // What is drawn is laid over a picture afterwards, so letters can't be smoothed
            // against a known background colour: grey smoothing, as Windows 11's own apps use.
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = dc.CreateSolidColorBrush(&color(Color::WHITE), None)?;
            let style = D2D1_STROKE_STYLE_PROPERTIES { startCap: D2D1_CAP_STYLE_ROUND, endCap: D2D1_CAP_STYLE_ROUND, dashCap: D2D1_CAP_STYLE_ROUND, lineJoin: D2D1_LINE_JOIN_ROUND, miterLimit: 10.0, dashStyle: D2D1_DASH_STYLE_SOLID, dashOffset: 0.0 };
            let round = factory.CreateStrokeStyle(&style, None)?;
            Ok(Self { factory, dc, brush, round, fonts: Fonts::new()?, paths: HashMap::new(), logo: None })
        }
    }

    pub fn family(&self) -> String {
        self.fonts.family()
    }

    /// Something Direct2D can draw into, on a texture's surface. The texture is BGRA, 8 bits a
    /// channel, with colour already multiplied by alpha: Direct2D's own format.
    pub fn target(&self, surface: &IDXGISurface) -> Result<ID2D1Bitmap1> {
        self.target_as(surface, DXGI_FORMAT_B8G8R8A8_UNORM)
    }

    /// The same for a surface of another format; fails for those Direct2D doesn't draw into.
    pub fn target_as(&self, surface: &IDXGISurface, format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT) -> Result<ID2D1Bitmap1> {
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: Default::default(),
        };
        // SAFETY: the properties live through the call.
        Ok(unsafe { self.dc.CreateBitmapFromDxgiSurface(surface, Some(&properties))? })
    }

    fn path(&self, segments: &[Seg]) -> Result<ID2D1PathGeometry> {
        // SAFETY: a geometry made, filled and closed here.
        unsafe {
            let path = self.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            let mut open = false;
            for seg in segments {
                match *seg {
                    Seg::Move(x, y) => {
                        if open {
                            sink.EndFigure(D2D1_FIGURE_END_OPEN);
                        }
                        sink.BeginFigure(Vector2 { X: x, Y: y }, D2D1_FIGURE_BEGIN_HOLLOW);
                        open = true;
                    }
                    Seg::Line(x, y) if open => sink.AddLine(Vector2 { X: x, Y: y }),
                    Seg::Arc(rx, ry, turn, large, clockwise, x, y) if open => sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: Vector2 { X: x, Y: y },
                        size: D2D_SIZE_F { width: rx, height: ry },
                        rotationAngle: turn,
                        sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    }),
                    Seg::Close if open => {
                        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                        open = false;
                    }
                    _ => {}
                }
            }
            if open {
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close()?;
            Ok(path)
        }
    }

    fn set(&self, c: Color) {
        // SAFETY: a call on a brush held here.
        unsafe { self.brush.SetColor(&color(c)) };
    }

    /// Draw the 16-unit shapes of `icon` with their corner at `x`, `y`, `size` across.
    fn icon(&mut self, icon: Icon, x: f32, y: f32, size: f32, c: Color, stroke: f32) -> Result<()> {
        let k = size / 16.0;
        self.set(c);
        for (n, shape) in icon.shapes().iter().enumerate() {
            if let Shape::Path(d) = shape {
                if !self.paths.contains_key(&(icon, n)) {
                    let made = self.path(&parse(d))?;
                    self.paths.insert((icon, n), made);
                }
            }
        }
        // SAFETY: Direct2D calls between BeginDraw and EndDraw.
        unsafe {
            self.dc.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: x, M32: y });
            for (n, shape) in icon.shapes().iter().enumerate() {
                match *shape {
                    Shape::Path(_) => self.dc.DrawGeometry(&self.paths[&(icon, n)], &self.brush, stroke, &self.round),
                    Shape::Rect(x, y, w, h, r) => self.dc.DrawRoundedRectangle(&rounded(Rect { x, y, w, h }, r), &self.brush, stroke, &self.round),
                    Shape::Circle(cx, cy, r) => self.dc.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, &self.brush, stroke, &self.round),
                }
            }
            self.dc.SetTransform(&UNIT);
        }
        Ok(())
    }

    /// Draw `frame` into `target`, which is as large as the window.
    pub fn paint(&mut self, target: &ID2D1Bitmap1, frame: &Frame) -> Result<()> {
        // SAFETY: Direct2D calls on a context held here, between BeginDraw and EndDraw.
        unsafe {
            self.dc.SetTarget(target);
            self.dc.SetDpi(96.0 * frame.scale, 96.0 * frame.scale);
            self.dc.BeginDraw();
            self.dc.SetTransform(&UNIT);
            self.dc.Clear(Some(&color(Color::CLEAR)));
            let mut clips = 0;
            for op in &frame.ops {
                match op {
                    Op::Fill { r, radius, color } => {
                        self.set(*color);
                        if *radius > 0.0 {
                            self.dc.FillRoundedRectangle(&rounded(*r, *radius), &self.brush);
                        } else {
                            self.dc.FillRectangle(&rect_f(*r), &self.brush);
                        }
                    }
                    Op::Stroke { r, radius, color, width } => {
                        self.set(*color);
                        let inner = r.inset(width / 2.0);
                        if *radius > 0.0 {
                            self.dc.DrawRoundedRectangle(&rounded(inner, (radius - width / 2.0).max(0.0)), &self.brush, *width, None);
                        } else {
                            self.dc.DrawRectangle(&rect_f(inner), &self.brush, *width, None);
                        }
                    }
                    Op::Text { r, text, font, color, align, wrap } => {
                        if let Ok(layout) = self.fonts.layout(text, *font, r.w, *wrap, *align) {
                            self.set(*color);
                            self.dc.DrawTextLayout(Vector2 { X: r.x, Y: r.y }, &layout, &self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
                        }
                    }
                    Op::Icon { icon, x, y, size, color, stroke } => self.icon(*icon, *x, *y, *size, *color, *stroke)?,
                    Op::Logo { x, y, size } => {
                        if self.logo.is_none() {
                            self.logo = Some(self.path(&parse("M2.5 6h2M2 7.8h2.5M3 9.5h1.5"))?);
                        }
                        let k = size / 16.0;
                        self.dc.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: *x, M32: *y });
                        self.set(LOGO);
                        self.dc.FillRoundedRectangle(&rounded(Rect { x: 0.0, y: 0.0, w: 16.0, h: 16.0 }, 3.5), &self.brush);
                        self.set(Color::WHITE);
                        self.dc.DrawRoundedRectangle(&rounded(Rect { x: 6.0, y: 5.0, w: 7.0, h: 5.0 }, 1.2), &self.brush, 1.2, None);
                        self.dc.DrawGeometry(self.logo.as_ref().unwrap(), &self.brush, 1.1, &self.round);
                        self.dc.SetTransform(&UNIT);
                    }
                    Op::Ring { cx, cy, r, width, track, color, turn } => {
                        self.set(*track);
                        self.dc.DrawEllipse(&D2D1_ELLIPSE { point: Vector2 { X: *cx, Y: *cy }, radiusX: *r, radiusY: *r }, &self.brush, *width, None);
                        // A quarter of the circle, starting `turn` of the way round from the top.
                        let at = |part: f32| {
                            let a = (turn + part) * std::f32::consts::TAU;
                            (cx + r * a.sin(), cy - r * a.cos())
                        };
                        let ((x0, y0), (x1, y1)) = (at(0.0), at(0.25));
                        let arc = self.path(&[Seg::Move(x0, y0), Seg::Arc(*r, *r, 0.0, false, true, x1, y1)])?;
                        self.set(*color);
                        self.dc.DrawGeometry(&arc, &self.brush, *width, &self.round);
                    }
                    Op::Dot { cx, cy, r, color, ring } => {
                        self.set(*color);
                        let (inner, point) = (r - ring / 2.0, Vector2 { X: *cx, Y: *cy });
                        if *ring > 0.0 {
                            self.dc.DrawEllipse(&D2D1_ELLIPSE { point, radiusX: inner, radiusY: inner }, &self.brush, *ring, None);
                        } else {
                            self.dc.FillEllipse(&D2D1_ELLIPSE { point, radiusX: *r, radiusY: *r }, &self.brush);
                        }
                    }
                    Op::Shadow { r, radius, strength } => {
                        // The design's shadow is 30 wide and 10 down. Ten sheets, each a little
                        // larger than the last and each a tenth of the darkness, come to the
                        // same soft edge without an effect and its extra surface.
                        self.set(Color(0.0, 0.0, 0.0, strength / 10.0));
                        for i in 0..10 {
                            let grow = -13.5 + 3.0 * i as f32;
                            let sheet = Rect { x: r.x - grow, y: r.y - grow + 10.0, w: r.w + 2.0 * grow, h: r.h + 2.0 * grow };
                            if sheet.w > 0.0 && sheet.h > 0.0 {
                                self.dc.FillRoundedRectangle(&rounded(sheet, (radius + grow).max(1.0)), &self.brush);
                            }
                        }
                    }
                    Op::Clip(r) => {
                        self.dc.PushAxisAlignedClip(&rect_f(*r), D2D1_ANTIALIAS_MODE_ALIASED);
                        clips += 1;
                    }
                    Op::Unclip => {
                        if clips > 0 {
                            self.dc.PopAxisAlignedClip();
                            clips -= 1;
                        }
                    }
                }
            }
            for _ in 0..clips {
                self.dc.PopAxisAlignedClip();
            }
            let done = self.dc.EndDraw(None, None);
            self.dc.SetTarget(None);
            done.context("Direct2D drawing")?;
        }
        Ok(())
    }
}
