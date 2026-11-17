//! Screen capture from a PipeWire node (a Mutter ScreenCast stream), adapted from espalier.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pipewire as pw;
use pw::spa;

/// Byte order of a captured pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Bgrx,
    Bgra,
    Rgbx,
    Rgba,
}

/// Where a frame's pixels are.
pub enum FrameData {
    /// CPU memory, rows `stride` bytes apart.
    Cpu { stride: usize, bytes: Vec<u8> },
    /// CUDA memory, imported from the compositor's DMA-BUF.
    Gpu(super::gpu::GpuFrame),
}

/// One captured frame.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: FrameData,
    /// When PipeWire handed it over.
    pub captured: Instant,
}

impl Frame {
    /// Tightly packed BGRA rows (for writing images). GPU frames are downloaded first.
    pub fn to_bgra(&self) -> Result<Vec<u8>> {
        let row = self.width as usize * 4;
        let downloaded;
        let (stride, data) = match &self.data {
            FrameData::Cpu { stride, bytes } => (*stride, bytes.as_slice()),
            FrameData::Gpu(g) => {
                downloaded = g.download()?;
                (row, downloaded.as_slice())
            }
        };
        let mut out = Vec::with_capacity(row * self.height as usize);
        for y in 0..self.height as usize {
            let line = &data[y * stride..y * stride + row];
            match self.format {
                PixelFormat::Bgrx | PixelFormat::Bgra => out.extend_from_slice(line),
                PixelFormat::Rgbx | PixelFormat::Rgba => {
                    for px in line.chunks_exact(4) {
                        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                    }
                }
            }
        }
        if matches!(self.format, PixelFormat::Bgrx | PixelFormat::Rgbx) {
            for px in out.chunks_exact_mut(4) {
                px[3] = 255;
            }
        }
        Ok(out)
    }
}

/// The newest frame, handed from the PipeWire thread to the consumer.
#[derive(Default)]
struct Slot {
    frame: Option<Frame>,
    /// A spent frame's buffer, reused to avoid reallocating every frame.
    spare: Option<Vec<u8>>,
    error: Option<String>,
}

type Shared = Arc<(Mutex<Slot>, Condvar)>;

pub struct Capture {
    shared: Shared,
    /// Stops the PipeWire loop.
    pw_quit: pw::channel::Sender<()>,
    pw_thread: Option<std::thread::JoinHandle<()>>,
}

impl Capture {
    /// Capture PipeWire node `node`, asking for `want` pixels at up to `fps`.
    pub fn from_node(node: u32, want: (u32, u32), fps: u32) -> Result<Self> {
        let shared: Shared = Default::default();
        let (pw_quit, quit_rx) = pw::channel::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let s = shared.clone();
                let pw_thread = std::thread::Builder::new().name("screencast".into()).spawn(move || {
            if let Err(e) = run_stream(node, want, fps, s.clone(), quit_rx, ready_tx) {
                let (slot, cv) = &*s;
                slot.lock().unwrap().error = Some(format!("{e:#}"));
                cv.notify_all();
            }
        })?;
        // Stream setup errors surface here; frames follow asynchronously.
        let _ = ready_rx.recv_timeout(Duration::from_secs(5));
        if let Some(e) = shared.0.lock().unwrap().error.take() {
            anyhow::bail!("screencast stream: {e}");
        }
        Ok(Self {
            shared,
            pw_quit,
            pw_thread: Some(pw_thread),
        })
    }

    /// The next new frame, or `None` if the screen hasn't changed within `timeout`.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>> {
        let (slot, cv) = &*self.shared;
        let deadline = Instant::now() + timeout;
        let mut s = slot.lock().unwrap();
        loop {
            if let Some(e) = s.error.take() {
                anyhow::bail!("screencast stopped: {e}");
            }
            if let Some(f) = s.frame.take() {
                return Ok(Some(f));
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            s = cv.wait_timeout(s, deadline - now).unwrap().0;
        }
    }

    /// Give a spent frame's memory back for reuse. (GPU frames return to their pool by
    /// themselves.)
    pub fn recycle(&self, frame: Frame) {
        if let FrameData::Cpu { bytes, .. } = frame.data {
            self.shared.0.lock().unwrap().spare = Some(bytes);
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.pw_quit.send(());
        if let Some(t) = self.pw_thread.take() {
            let _ = t.join();
        }
    }
}

/// Buffers taken from the stream, and those skipped as carrying no picture.
pub static DEQUEUED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static EMPTY: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Time spent importing DMA-BUFs: the longest, the total, and how many took over 6 ms.
pub static IMPORT_US_MAX: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
pub static IMPORT_US_SUM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static IMPORT_SLOW: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct StreamState {
    format: spa::param::video::VideoInfoRaw,
    shared: Shared,
    ready: Option<std::sync::mpsc::Sender<()>>,
    /// Imports DMA-BUF frames into CUDA; `None` means shared memory only.
    importer: Option<super::gpu::Importer>,
    /// The negotiated format uses DMA-BUFs.
    dmabuf: bool,
}

fn pixel_format(f: spa::param::video::VideoFormat) -> Option<PixelFormat> {
    use spa::param::video::VideoFormat as V;
    match f {
        V::BGRx => Some(PixelFormat::Bgrx),
        V::BGRA => Some(PixelFormat::Bgra),
        V::RGBx => Some(PixelFormat::Rgbx),
        V::RGBA => Some(PixelFormat::Rgba),
        _ => None,
    }
}

pub(super) fn serialize(obj: spa::pod::Object) -> Result<Vec<u8>> {
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
        .map_err(|e| anyhow::anyhow!("serialize pod: {e:?}"))?
        .0
        .into_inner())
}

/// One EnumFormat for `formats`; with `modifiers`, for DMA-BUFs in those layouts.
pub(super) fn format_param(formats: &[spa::param::video::VideoFormat], modifiers: Option<&[u64]>, want: (u32, u32), fps: u32) -> Result<Vec<u8>> {
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

/// The formats to offer: DMA-BUFs Vulkan can import first (when there's an importer), then
/// shared memory.
fn format_params(modifiers: &[u64], want: (u32, u32), fps: u32) -> Result<Vec<Vec<u8>>> {
    use spa::param::video::VideoFormat as V;
    let mut out = Vec::new();
    if !modifiers.is_empty() {
        out.push(format_param(&[V::BGRx, V::BGRA], Some(modifiers), want, fps)?);
    }
    out.push(format_param(&[V::BGRx, V::BGRA, V::RGBx, V::RGBA], None, want, fps)?);
    Ok(out)
}

/// The Buffers param: which kind of memory we accept for the negotiated format.
pub(super) fn buffers_param(dmabuf: bool) -> Result<Vec<u8>> {
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

pub(super) fn update_params(stream: &pw::stream::Stream, params: &[Vec<u8>]) {
    let pods: Option<Vec<&spa::pod::Pod>> = params.iter().map(|p| spa::pod::Pod::from_bytes(p)).collect();
    if let Some(mut pods) = pods {
        if let Err(e) = stream.update_params(&mut pods) {
            tracing::warn!("update screencast params: {e}");
        }
    }
}

fn run_stream(
    node: u32,
    want: (u32, u32),
    fps: u32,
    shared: Shared,
    quit: pw::channel::Receiver<()>,
    ready: std::sync::mpsc::Sender<()>,
) -> Result<()> {
    pw::init();
    let importer = if std::env::var("RM_CAPTURE").as_deref() == Ok("shm") {
        None
    } else {
        match super::gpu::Importer::new() {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::info!("capturing through CPU memory: no GPU import ({e:#})");
                None
            }
        }
    };
    let modifiers = match &importer {
        Some(i) => i.modifiers(PixelFormat::Bgrx).unwrap_or_else(|e| {
            tracing::info!("capturing through CPU memory: {e:#}");
            Vec::new()
        }),
        None => Vec::new(),
    };
    if let Some(i) = &importer {
        tracing::debug!(node = %i.node, modifiers = modifiers.len(), "DMA-BUF import available");
    }
    let importer = if modifiers.is_empty() { None } else { importer };

    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connect to the screencast")?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "espalier-screen",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;
    let state = StreamState { format: Default::default(), shared, ready: Some(ready), importer, dmabuf: false };
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed(|_, st, _old, new| {
            tracing::debug!(?new, "screencast stream");
            if let pw::stream::StreamState::Error(e) = &new {
                let (slot, cv) = &*st.shared;
                slot.lock().unwrap().error = Some(e.clone());
                cv.notify_all();
            }
            if matches!(new, pw::stream::StreamState::Streaming | pw::stream::StreamState::Paused) {
                if let Some(r) = st.ready.take() {
                    let _ = r.send(());
                }
            }
        })
        .param_changed(|stream, st, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if st.format.parse(param).is_err() {
                return;
            }
            // (libspa only names this flag behind a version feature.)
            let has_modifier = st.format.flags().bits() & spa::sys::SPA_VIDEO_FLAG_MODIFIER != 0;
            st.dmabuf = has_modifier && st.importer.is_some();
            let s = st.format.size();
            tracing::info!(
                format = ?st.format.format(),
                width = s.width,
                height = s.height,
                memory = if st.dmabuf { "GPU (DMA-BUF)" } else { "CPU (shared memory)" },
                modifier = format!("{:#x}", st.format.modifier()),
                "screencast format"
            );
            if let Ok(b) = buffers_param(st.dmabuf) {
                update_params(stream, &[b]);
            }
        })
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            DEQUEUED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(format) = pixel_format(st.format.format()) else { return };
            let size = st.format.size();
            let modifier = st.format.modifier();
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let chunk = d.chunk();
            // Empty or corrupted chunks carry no new picture (e.g. cursor-only updates).
            if chunk.size() == 0 || chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED) {
                EMPTY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            let (offset, len) = (chunk.offset() as usize, chunk.size() as usize);
            let stride = match chunk.stride() {
                s if s > 0 => s as usize,
                _ => size.width as usize * 4,
            };

            let data = if d.type_() == spa::buffer::DataType::DmaBuf {
                let Some(importer) = st.importer.as_mut() else { return };
                let maxsize = d.as_raw().maxsize as usize;
                let t_import = Instant::now();
                let imported = importer.import(d.fd() as i32, maxsize, offset as u32, stride as u32, modifier, size.width, size.height, format);
                let us = t_import.elapsed().as_micros() as u32;
                IMPORT_US_MAX.fetch_max(us, std::sync::atomic::Ordering::Relaxed);
                IMPORT_US_SUM.fetch_add(us as u64, std::sync::atomic::Ordering::Relaxed);
                if us > 6000 {
                    IMPORT_SLOW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                match imported {
                    Ok(g) => FrameData::Gpu(g),
                    Err(e) => {
                        // Fall back to shared memory for the rest of the session.
                        tracing::warn!("DMA-BUF import failed, switching to CPU capture: {e:#}");
                        st.importer = None;
                        st.dmabuf = false;
                        if let Ok(p) = format_params(&[], (st.format.size().width, st.format.size().height), 120) {
                            update_params(stream, &p);
                        }
                        return;
                    }
                }
            } else {
                let Some(bytes) = d.data() else { return };
                let Some(src) = bytes.get(offset..offset + len) else { return };
                let needed = stride * (size.height as usize - 1) + size.width as usize * 4;
                if src.len() < needed {
                    return;
                }
                let mut slot = st.shared.0.lock().unwrap();
                // Reuse the buffer of a frame nobody picked up, or a recycled one.
                let reuse = match slot.frame.take().map(|f| f.data) {
                    Some(FrameData::Cpu { bytes, .. }) => Some(bytes),
                    _ => slot.spare.take(),
                };
                drop(slot);
                let mut v = reuse.unwrap_or_default();
                v.clear();
                v.extend_from_slice(&src[..needed]);
                FrameData::Cpu { stride, bytes: v }
            };
            let (slot, cv) = &*st.shared;
            slot.lock().unwrap().frame =
                Some(Frame { width: size.width, height: size.height, format, data, captured: Instant::now() });
            cv.notify_all();
        })
        .register()?;

    let values = format_params(&modifiers, want, fps)?;
    let mut params: Vec<&spa::pod::Pod> =
        values.iter().map(|v| spa::pod::Pod::from_bytes(v).context("format pod")).collect::<Result<_>>()?;
    stream.connect(
        spa::utils::Direction::Input,
        Some(node),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| ml.quit());
    mainloop.run();
    Ok(())
}
