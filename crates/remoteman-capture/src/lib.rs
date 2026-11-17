//! Linux screen capture: a PipeWire stream (a Mutter or portal screencast) that takes DMA-BUFs only and hands each one, untouched, to a callback on
//! the PipeWire thread. The buffer is only valid during the callback: the compositor reuses it
//! afterwards.

use anyhow::{Context, Result};
pub use pipewire::channel::{Receiver as QuitReceiver, Sender as QuitSender, channel as quit_channel};
use pipewire as pw;
use pw::spa;


/// One frame as the compositor made it.
pub struct RawFrame {
    pub fd: i32,
    pub modifier: u64,
    pub offset: u32,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
}

/// The cursor, as the compositor reports it alongside the picture.
pub struct Cursor {
    /// False: it isn't on this screen.
    pub on_screen: bool,
    /// In the picture's pixels.
    pub x: i32,
    pub y: i32,
    /// Set when the shape changed.
    pub shape: Option<CursorShape>,
}

/// A cursor image: premultiplied RGBA, rows top to bottom. Zero size: no cursor is shown.
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub rgba: Vec<u8>,
}

#[repr(transparent)]
struct CursorMeta(spa::sys::spa_meta_cursor);

impl spa::buffer::meta::Metadata for CursorMeta {
    const META_TYPE: u32 = spa::sys::SPA_META_Cursor;
}

/// The largest cursor image asked for, as Mutter offers it.
const CURSOR_MAX: usize = 384;

fn cursor_meta_size() -> usize {
    std::mem::size_of::<spa::sys::spa_meta_cursor>() + std::mem::size_of::<spa::sys::spa_meta_bitmap>() + CURSOR_MAX * CURSOR_MAX * 4
}

/// Read the cursor metadata of a buffer, if it carries any.
fn read_cursor(meta: &CursorMeta) -> Cursor {
    let m = &meta.0;
    if m.id == 0 {
        return Cursor { on_screen: false, x: 0, y: 0, shape: None };
    }
    let mut shape = None;
    if m.bitmap_offset as usize >= std::mem::size_of::<spa::sys::spa_meta_cursor>() {
        // SAFETY: the metadata area is `cursor_meta_size()` bytes, as negotiated; the bitmap
        // header sits at `bitmap_offset` and its pixels at its own `offset`, both inside it
        // (checked against the size below before reading).
        unsafe {
            let base = meta as *const CursorMeta as *const u8;
            let bitmap = &*(base.add(m.bitmap_offset as usize) as *const spa::sys::spa_meta_bitmap);
            let (w, h, stride) = (bitmap.size.width as usize, bitmap.size.height as usize, bitmap.stride as usize);
            let start = m.bitmap_offset as usize + bitmap.offset as usize;
            if w == 0 || h == 0 {
                shape = Some(CursorShape { width: 0, height: 0, hot_x: 0, hot_y: 0, rgba: Vec::new() });
            } else if w <= CURSOR_MAX && h <= CURSOR_MAX && stride >= w * 4 && start + stride * (h - 1) + w * 4 <= cursor_meta_size() {
                let mut rgba = Vec::with_capacity(w * h * 4);
                for row in 0..h {
                    rgba.extend_from_slice(std::slice::from_raw_parts(base.add(start + row * stride), w * 4));
                }
                shape = Some(CursorShape { width: w as u32, height: h as u32, hot_x: m.hotspot.x, hot_y: m.hotspot.y, rgba });
            }
        }
    }
    Cursor { on_screen: true, x: m.position.x, y: m.position.y, shape }
}

/// The Meta param asking for cursor metadata on every buffer.
fn cursor_meta_param() -> Result<Vec<u8>> {
    use spa::pod::{Property, PropertyFlags, Value};
    serialize(spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa::sys::SPA_PARAM_Meta,
        properties: vec![
            Property { key: spa::sys::SPA_PARAM_META_type, flags: PropertyFlags::empty(), value: Value::Id(spa::utils::Id(spa::sys::SPA_META_Cursor)) },
            Property { key: spa::sys::SPA_PARAM_META_size, flags: PropertyFlags::empty(), value: Value::Int(cursor_meta_size() as i32) },
        ],
    })
}

/// The key and value pairs of a Tag param.
fn read_tags(param: &spa::pod::Pod) -> Vec<(String, String)> {
    use spa::pod::Value;
    let mut out = Vec::new();
    let Ok((_, Value::Object(object))) = spa::pod::deserialize::PodDeserializer::deserialize_any_from(param.as_bytes()) else { return out };
    for p in object.properties {
        if p.key != spa::sys::SPA_PARAM_TAG_info {
            continue;
        }
        if let Value::Struct(items) = p.value {
            // An item count, then keys and values in turn.
            let mut strings = items.into_iter().skip(1).filter_map(|v| if let Value::String(s) = v { Some(s) } else { None });
            while let (Some(k), Some(v)) = (strings.next(), strings.next()) {
                out.push((k, v));
            }
        }
    }
    out
}

pub enum Event<'a> {
    /// The scale the compositor draws this screen at (Mutter says so for virtual monitors).
    /// Pointer and touch positions are in pixels divided by it.
    Scale(f32),
    /// The cursor moved or changed (only when cursor metadata was asked for). It can come with
    /// or without a new picture.
    Cursor(Cursor),
    /// The stream (re)negotiated: earlier buffers are gone.
    Format { width: u32, height: u32, modifier: u64 },
    Frame(&'a RawFrame),
}

struct State {
    cursor: bool,
    format: spa::param::video::VideoInfoRaw,
    on_event: Box<dyn FnMut(Event)>,
}

/// Run the stream on this thread until `quit` fires. `scale` is the HiDPI scale wanted for a
/// virtual monitor (Mutter reads it from a stream tag); `None` leaves it to the compositor.
/// With `cursor`, the cursor comes as [`Event::Cursor`] (the stream must have been made with
/// the cursor as metadata).
pub fn run(node: u32, want: (u32, u32), fps: u32, scale: Option<f32>, cursor: bool, modifiers: &[u64], on_event: Box<dyn FnMut(Event)>, quit: pw::channel::Receiver<()>) -> Result<()> {
    use spa::param::video::VideoFormat as V;
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connect to PipeWire")?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "remoteman",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let _listener = stream
        .add_local_listener_with_user_data(State { cursor, format: Default::default(), on_event })
        .param_changed(|stream, st, id, param| {
            let Some(param) = param else { return };
            if id == spa::sys::SPA_PARAM_Tag {
                for (key, value) in read_tags(param) {
                    if key == "org.gnome.scale" {
                        if let Ok(scale) = value.parse::<f32>() {
                            (st.on_event)(Event::Scale(scale));
                        }
                    }
                }
                return;
            }
            if id != spa::param::ParamType::Format.as_raw() || st.format.parse(param).is_err() {
                return;
            }
            let s = st.format.size();
            (st.on_event)(Event::Format { width: s.width, height: s.height, modifier: st.format.modifier() });
            let mut params = Vec::new();
            if let Ok(b) = buffers_param(true) {
                params.push(b);
            }
            if st.cursor {
                if let Ok(m) = cursor_meta_param() {
                    params.push(m);
                }
            }
            update_params(stream, &params);
        })
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            if st.cursor {
                if let Some(meta) = buffer.find_meta::<CursorMeta>() {
                    let cursor = read_cursor(meta);
                    (st.on_event)(Event::Cursor(cursor));
                }
            }
            let size = st.format.size();
            let modifier = st.format.modifier();
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            if d.type_() != spa::buffer::DataType::DmaBuf {
                return;
            }
            let chunk = d.chunk();
            if chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                return;
            }
            let stride = match chunk.stride() {
                s if s > 0 => s as u32,
                _ => size.width * 4,
            };
            let frame = RawFrame { fd: d.fd() as i32, modifier, offset: chunk.offset(), stride, width: size.width, height: size.height };
            (st.on_event)(Event::Frame(&frame));
        })
        .register()?;

    let value = format_param(&[V::BGRx, V::BGRA], Some(modifiers), want, fps)?;
    let tag = match scale {
        Some(s) => Some(tag_param(&[("org.gnome.preferred-scale", &format!("{s}"))])?),
        None => None,
    };
    let mut params = vec![spa::pod::Pod::from_bytes(&value).context("format pod")?];
    if let Some(tag) = &tag {
        params.push(spa::pod::Pod::from_bytes(tag).context("tag pod")?);
    }
    // No MAP_BUFFERS: the pixels never come to the CPU.
    stream.connect(spa::utils::Direction::Input, Some(node), pw::stream::StreamFlags::AUTOCONNECT, &mut params)?;
    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| ml.quit());
    mainloop.run();
    Ok(())
}

fn serialize(obj: spa::pod::Object) -> Result<Vec<u8>> {
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("serialize pod: {e:?}"))?
        .0
        .into_inner())
}

/// One EnumFormat for `formats`; with `modifiers`, for DMA-BUFs in those layouts.
fn format_param(formats: &[spa::param::video::VideoFormat], modifiers: Option<&[u64]>, want: (u32, u32), fps: u32) -> Result<Vec<u8>> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

    let prop = |key: FormatProperties, value| Property { key: key.as_raw(), flags: PropertyFlags::empty(), value };
    let mut properties = vec![
        prop(FormatProperties::MediaType, Value::Id(Id(MediaType::Video.as_raw()))),
        prop(FormatProperties::MediaSubtype, Value::Id(Id(MediaSubtype::Raw.as_raw()))),
        prop(
            FormatProperties::VideoFormat,
            Value::Choice(ChoiceValue::Id(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: Id(formats[0].as_raw()),
                    alternatives: formats.iter().map(|f| Id(f.as_raw())).collect(),
                },
            ))),
        ),
        prop(
            FormatProperties::VideoSize,
            Value::Choice(ChoiceValue::Rectangle(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Rectangle { width: want.0, height: want.1 },
                    min: Rectangle { width: 1, height: 1 },
                    max: Rectangle { width: 8192, height: 8192 },
                },
            ))),
        ),
        prop(FormatProperties::VideoFramerate, Value::Fraction(Fraction { num: 0, denom: 1 })),
        prop(
            FormatProperties::VideoMaxFramerate,
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Fraction { num: fps, denom: 1 },
                    min: Fraction { num: 1, denom: 1 },
                    max: Fraction { num: fps, denom: 1 },
                },
            ))),
        ),
    ];
    if let Some(mods) = modifiers {
        // Mandatory: only producers that can allocate one of these layouts match this format.
        properties.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum { default: mods[0] as i64, alternatives: mods.iter().map(|&m| m as i64).collect() },
            ))),
        });
    }
    serialize(spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties,
    })
}

/// The Buffers param: which kind of memory we accept for the negotiated format.
fn buffers_param(dmabuf: bool) -> Result<Vec<u8>> {
    use spa::pod::{Property, PropertyFlags, Value};
    let kinds = if dmabuf {
        1 << spa::sys::SPA_DATA_DmaBuf
    } else {
        (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr)
    };
    serialize(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![Property {
            key: spa::sys::SPA_PARAM_BUFFERS_dataType,
            flags: PropertyFlags::empty(),
            value: Value::Int(kinds as i32),
        }],
    })
}

fn update_params(stream: &pw::stream::Stream, params: &[Vec<u8>]) {
    let pods: Option<Vec<&spa::pod::Pod>> = params.iter().map(|p| spa::pod::Pod::from_bytes(p)).collect();
    if let Some(mut pods) = pods {
        if let Err(e) = stream.update_params(&mut pods) {
            eprintln!("update stream params: {e}");
        }
    }
}

/// A Tag param: key and value pairs from this (the consuming) side of the stream.
fn tag_param(items: &[(&str, &str)]) -> Result<Vec<u8>> {
    use spa::pod::{Property, PropertyFlags, Value};
    let mut info = vec![Value::Int(items.len() as i32)];
    for (k, v) in items {
        info.push(Value::String((*k).to_owned()));
        info.push(Value::String((*v).to_owned()));
    }
    serialize(spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamTag,
        id: spa::sys::SPA_PARAM_Tag,
        properties: vec![
            Property { key: spa::sys::SPA_PARAM_TAG_direction, flags: PropertyFlags::empty(), value: Value::Id(spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT)) },
            Property { key: spa::sys::SPA_PARAM_TAG_info, flags: PropertyFlags::empty(), value: Value::Struct(info) },
        ],
    })
}
