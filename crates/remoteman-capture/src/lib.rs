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

pub enum Event<'a> {
    /// The stream (re)negotiated: earlier buffers are gone.
    Format { width: u32, height: u32, modifier: u64 },
    Frame(&'a RawFrame),
}

struct State {
    format: spa::param::video::VideoInfoRaw,
    on_event: Box<dyn FnMut(Event)>,
}

/// Run the stream on this thread until `quit` fires.
pub fn run(node: u32, want: (u32, u32), fps: u32, modifiers: &[u64], on_event: Box<dyn FnMut(Event)>, quit: pw::channel::Receiver<()>) -> Result<()> {
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
        .add_local_listener_with_user_data(State { format: Default::default(), on_event })
        .param_changed(|stream, st, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() || st.format.parse(param).is_err() {
                return;
            }
            let s = st.format.size();
            (st.on_event)(Event::Format { width: s.width, height: s.height, modifier: st.format.modifier() });
            if let Ok(b) = buffers_param(true) {
                update_params(stream, &[b]);
            }
        })
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
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
    let mut params = [spa::pod::Pod::from_bytes(&value).context("format pod")?];
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
