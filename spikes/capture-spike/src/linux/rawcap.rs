//! A PipeWire stream that takes DMA-BUFs only and hands each one, untouched, to a callback on
//! the PipeWire thread. The buffer is only valid during the callback: the compositor reuses it
//! afterwards.

use anyhow::{Context, Result};
use pipewire as pw;
use pw::spa;

use super::capture::{buffers_param, format_param, update_params};

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
        "remoteman-spike",
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
