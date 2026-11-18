//! The PipeWire camera: an output pw-stream of class `Video/Source` and role `Camera`, which is
//! what the desktop's camera portal, WirePlumber and camera programs look for. It runs on its own
//! thread with its own main loop; pictures come in over a channel.
//!
//! The node drives itself (as `pipewiresink mode=provide` does): for every picture it starts one
//! graph cycle and fills one buffer in the stream's `process` call. While nothing reads the
//! camera, nothing is copied.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use pipewire as pw;
use pw::spa;
use pw::stream::StreamState;

use crate::IDLE_INTERVAL;
use crate::picture::{Layout, Picture};

enum Msg {
    /// A new picture and when it was taken, on the monotonic clock in nanoseconds.
    Picture(Arc<Picture>, i64),
    Quit,
}

/// The running camera node. Dropping it removes the node.
pub(crate) struct Output {
    tx: pw::channel::Sender<Msg>,
    thread: Option<JoinHandle<()>>,
    node_id: u32,
}

impl Output {
    /// Make the node and wait until PipeWire has it. `node_name` is its `node.name`, `label` what
    /// people see in a camera picker.
    pub fn start(node_name: &str, label: &str, width: u32, height: u32, fps: u32) -> Result<Output> {
        let (tx, rx) = pw::channel::channel::<Msg>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();
        let (node_name, label) = (node_name.to_owned(), label.to_owned());
        let thread = std::thread::Builder::new()
            .name("remoteman-camera".into())
            .spawn(move || {
                if let Err(e) = run(&node_name, &label, width, height, fps, rx, ready_tx.clone()) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .context("start the camera thread")?;
        let node_id = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(id)) => id,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e.context("make the PipeWire camera"));
            }
            Err(_) => {
                let _ = tx.send(Msg::Quit);
                let _ = thread.join();
                bail!("PipeWire did not take the camera node within 5 s");
            }
        };
        Ok(Output { tx, thread: Some(thread), node_id })
    }

    /// The node's id in the PipeWire graph.
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Show this picture from now on.
    pub fn show(&self, picture: Arc<Picture>, taken_ns: i64) {
        // (Nothing is queued for a thread that is no longer there to take it.)
        if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            let _ = self.tx.send(Msg::Picture(picture, taken_ns));
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct State {
    /// What the consumer and we agreed on; `None` while nothing reads the camera.
    layout: Option<Layout>,
    /// Buffers flow: a consumer is linked and running.
    streaming: bool,
    /// The picture to show, and when it was taken.
    picture: Arc<Picture>,
    taken_ns: i64,
    /// `picture` has not gone out yet (otherwise the next buffer is a repeat, stamped now).
    fresh: bool,
    /// A buffer is to be filled at the next `process`.
    pending: bool,
    last_out: Instant,
    seq: u64,
    ready: Option<mpsc::Sender<Result<u32>>>,
}

fn run(node_name: &str, label: &str, width: u32, height: u32, fps: u32, rx: pw::channel::Receiver<Msg>, ready: mpsc::Sender<Result<u32>>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connect to PipeWire")?;
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_CLASS, "Video/Source");
    props.insert(*pw::keys::MEDIA_ROLE, "Camera");
    props.insert(*pw::keys::NODE_NAME, node_name);
    props.insert(*pw::keys::NODE_DESCRIPTION, label);
    props.insert(*pw::keys::NODE_NICK, label);
    let stream = pw::stream::StreamRc::new(core, "remoteman-camera", props)?;

    let state = Rc::new(RefCell::new(State {
        layout: None,
        streaming: false,
        picture: Arc::new(Picture::black(width, height)),
        taken_ns: 0,
        fresh: false,
        pending: false,
        last_out: Instant::now(),
        seq: 0,
        ready: Some(ready),
    }));

    // In every callback: the state is released before anything that can call back into the
    // stream (`trigger_process` runs `process` at once when called on this thread).
    let _listener = stream
        .add_local_listener_with_user_data(state.clone())
        .state_changed(|stream, st, _old, new| {
            let trigger = {
                let mut st = st.borrow_mut();
                match &new {
                    StreamState::Error(e) => {
                        eprintln!("camera: PipeWire stream error: {e}");
                        if let Some(ready) = st.ready.take() {
                            let _ = ready.send(Err(anyhow!("PipeWire stream error: {e}")));
                        }
                    }
                    StreamState::Paused | StreamState::Streaming => {
                        // The node exists from here on.
                        if let Some(ready) = st.ready.take() {
                            let _ = ready.send(Ok(stream.node_id()));
                        }
                    }
                    _ => {}
                }
                st.streaming = matches!(new, StreamState::Streaming);
                // A consumer that starts reading gets the current picture at once.
                st.pending = st.streaming;
                st.streaming
            };
            if trigger {
                let _ = stream.trigger_process();
            }
        })
        .param_changed(move |stream, st, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut st = st.borrow_mut();
            st.layout = None;
            let Some(param) = param else { return };
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            use spa::param::video::VideoFormat as V;
            let layout = match info.format() {
                V::NV12 => Layout::Nv12,
                V::I420 => Layout::I420,
                V::YUY2 => Layout::Yuy2,
                _ => return,
            };
            if info.size().width != width || info.size().height != height {
                return;
            }
            st.layout = Some(layout);
            let params = [buffers_param(layout, width, height), header_meta_param()];
            let pods: Option<Vec<&spa::pod::Pod>> = params.iter().map(|p| p.as_ref().ok().and_then(|p| spa::pod::Pod::from_bytes(p))).collect();
            if let Some(mut pods) = pods {
                if let Err(e) = stream.update_params(&mut pods) {
                    eprintln!("camera: update stream params: {e}");
                }
            }
        })
        .process(|stream, st| publish(stream, &mut st.borrow_mut()))
        .register()?;

    use spa::param::video::VideoFormat as V;
    let formats = [V::NV12, V::I420, V::YUY2].iter().map(|&f| format_param(f, width, height, fps)).collect::<Result<Vec<_>>>()?;
    let mut params = formats.iter().map(|f| spa::pod::Pod::from_bytes(f).context("format pod")).collect::<Result<Vec<_>>>()?;
    // Driving, and not connected to anything by us: consumers come to the camera.
    stream.connect(spa::utils::Direction::Output, None, pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::MAP_BUFFERS, &mut params)?;

    let _receiver = rx.attach(mainloop.loop_(), {
        let (state, stream, mainloop) = (state.clone(), stream.clone(), mainloop.clone());
        move |msg| match msg {
            Msg::Picture(picture, taken_ns) => {
                let trigger = {
                    let mut st = state.borrow_mut();
                    st.picture = picture;
                    st.taken_ns = taken_ns;
                    st.fresh = true;
                    st.pending = st.streaming;
                    st.streaming
                };
                if trigger {
                    let _ = stream.trigger_process();
                }
            }
            Msg::Quit => mainloop.quit(),
        }
    });

    // While no new picture comes, the current one goes out again every `IDLE_INTERVAL`.
    let timer = mainloop.loop_().add_timer({
        let (state, stream) = (state.clone(), stream.clone());
        move |_| {
            let trigger = {
                let mut st = state.borrow_mut();
                let due = st.streaming && st.last_out.elapsed() + Duration::from_millis(10) >= IDLE_INTERVAL;
                if due {
                    st.pending = true;
                }
                due
            };
            if trigger {
                let _ = stream.trigger_process();
            }
        }
    });
    let tick = IDLE_INTERVAL / 2;
    timer.update_timer(Some(tick), Some(tick)).into_result().context("start the idle timer")?;

    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// Fill one buffer with the current picture and hand it to the graph.
fn publish(stream: &pw::stream::Stream, st: &mut State) {
    if !st.pending {
        return;
    }
    let Some(layout) = st.layout else { return };
    // SAFETY: called on the stream's loop thread; a non-null buffer is ours until queued again.
    let raw = unsafe { stream.dequeue_raw_buffer() };
    if raw.is_null() {
        // Every buffer is still with the consumer: this turn is skipped, the next one tries again.
        return;
    }
    let (width, height) = (st.picture.width, st.picture.height);
    let size = layout.size(width, height);
    let now = crate::monotonic_ns();
    // SAFETY: the stream mapped the buffer (MAP_BUFFERS); its one block is at least `maxsize`
    // bytes, checked against the picture's size before writing. The header, when there, is a
    // `spa_meta_header` (asked for by size).
    unsafe {
        let buffer = (*raw).buffer;
        if !buffer.is_null() && (*buffer).n_datas >= 1 && !(*buffer).datas.is_null() {
            let block = &mut *(*buffer).datas;
            let fits = !block.data.is_null() && block.maxsize as usize >= size;
            if fits {
                st.picture.pack(layout, std::slice::from_raw_parts_mut(block.data as *mut u8, size));
            }
            if !block.chunk.is_null() {
                let chunk = &mut *block.chunk;
                chunk.offset = 0;
                chunk.size = if fits { size as u32 } else { 0 };
                chunk.stride = layout.stride(width) as i32;
                chunk.flags = if fits { 0 } else { spa::sys::SPA_CHUNK_FLAG_CORRUPTED as i32 };
            }
            let header = spa::sys::spa_buffer_find_meta_data(buffer, spa::sys::SPA_META_Header, std::mem::size_of::<spa::sys::spa_meta_header>()) as *mut spa::sys::spa_meta_header;
            if !header.is_null() {
                (*header).flags = 0;
                (*header).offset = 0;
                // A repeat is a new moment of the same picture.
                (*header).pts = if st.fresh { st.taken_ns } else { now };
                (*header).dts_offset = 0;
                (*header).seq = st.seq;
            }
        }
        stream.queue_raw_buffer(raw);
    }
    st.seq += 1;
    st.fresh = false;
    st.pending = false;
    st.last_out = Instant::now();
}

fn serialize(object: spa::pod::Object) -> Result<Vec<u8>> {
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object))
        .map_err(|e| anyhow!("serialize pod: {e:?}"))?
        .0
        .into_inner())
}

/// One EnumFormat with every field fixed. Camera programs read these the way they read a
/// webcam's modes: one entry per pixel format, a plain size and a plain frame rate (WebRTC's
/// PipeWire camera code, in Firefox and Chromium, does not look inside ranges).
fn format_param(format: spa::param::video::VideoFormat, width: u32, height: u32, fps: u32) -> Result<Vec<u8>> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::pod::{Property, PropertyFlags, Value};
    use spa::utils::{Fraction, Id, Rectangle, SpaTypes};
    let prop = |key: FormatProperties, value| Property { key: key.as_raw(), flags: PropertyFlags::empty(), value };
    serialize(spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            prop(FormatProperties::MediaType, Value::Id(Id(MediaType::Video.as_raw()))),
            prop(FormatProperties::MediaSubtype, Value::Id(Id(MediaSubtype::Raw.as_raw()))),
            prop(FormatProperties::VideoFormat, Value::Id(Id(format.as_raw()))),
            prop(FormatProperties::VideoSize, Value::Rectangle(Rectangle { width, height })),
            prop(FormatProperties::VideoFramerate, Value::Fraction(Fraction { num: fps, denom: 1 })),
        ],
    })
}

/// The Buffers param: one block per picture, rows packed, in shared memory.
fn buffers_param(layout: Layout, width: u32, height: u32) -> Result<Vec<u8>> {
    use spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags};
    let prop = |key, value| Property { key, flags: PropertyFlags::empty(), value };
    let kinds = (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr);
    serialize(spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
        id: spa::param::ParamType::Buffers.as_raw(),
        properties: vec![
            prop(spa::sys::SPA_PARAM_BUFFERS_buffers, Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), ChoiceEnum::Range { default: 4, min: 2, max: 16 })))),
            prop(spa::sys::SPA_PARAM_BUFFERS_blocks, Value::Int(1)),
            prop(spa::sys::SPA_PARAM_BUFFERS_size, Value::Int(layout.size(width, height) as i32)),
            prop(spa::sys::SPA_PARAM_BUFFERS_stride, Value::Int(layout.stride(width) as i32)),
            prop(spa::sys::SPA_PARAM_BUFFERS_dataType, Value::Choice(ChoiceValue::Int(Choice(ChoiceFlags::empty(), ChoiceEnum::Flags { default: kinds, flags: vec![] })))),
        ],
    })
}

/// The Meta param asking for a header (timestamp and count) on every buffer.
fn header_meta_param() -> Result<Vec<u8>> {
    use spa::pod::{Property, PropertyFlags, Value};
    serialize(spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
        id: spa::sys::SPA_PARAM_Meta,
        properties: vec![
            Property { key: spa::sys::SPA_PARAM_META_type, flags: PropertyFlags::empty(), value: Value::Id(spa::utils::Id(spa::sys::SPA_META_Header)) },
            Property { key: spa::sys::SPA_PARAM_META_size, flags: PropertyFlags::empty(), value: Value::Int(std::mem::size_of::<spa::sys::spa_meta_header>() as i32) },
        ],
    })
}
