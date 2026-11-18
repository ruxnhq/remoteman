//! The other direction: this computer's own camera, for a viewer to send to the computer it is
//! looking at. A PipeWire stream reads the camera (the default one, or the node named), each
//! picture is fitted into 1280 by 720, and FFmpeg encodes it as the protocol asks: H.264,
//! Annex-B, the parameter sets in front of every keyframe, no B-frames, a keyframe every two
//! seconds and whenever one is asked for.
//!
//! The encoder is one of the system FFmpeg's that needs no software of another licence:
//! NVIDIA's, or OpenH264 where FFmpeg was built with it. (VA-API, for AMD and Intel, needs the
//! picture uploaded to the GPU first and is not wired yet.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use ff::format::Pixel;
use ff::software::scaling;
use ff::util::frame::Video;
use ffmpeg_next as ff;
use pipewire as pw;
use pw::spa;
use spa::param::video::VideoFormat;

/// What the protocol's one sure mode allows.
const MAX: (u32, u32) = (1280, 720);
const FPS: u32 = 30;
const BITRATE: usize = 2_500_000;

/// The encoders tried, in order, with their options.
const ENCODERS: [(&str, &[(&str, &str)]); 2] =
    [("h264_nvenc", &[("preset", "p1"), ("tune", "ull"), ("zerolatency", "1"), ("delay", "0"), ("forced-idr", "1"), ("rc", "cbr")]), ("libopenh264", &[("allow_skip_frames", "1")])];

fn any_encoder() -> bool {
    ENCODERS.iter().any(|(name, _)| ff::encoder::find_by_name(name).is_some())
}

/// A running camera. Dropping it lets go of the camera.
pub struct Source {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    key: Arc<AtomicBool>,
}

impl Source {
    /// Open the camera and call `on_frame` with every encoded frame (its bytes, and whether it
    /// is a keyframe), on a thread of the camera's own. `from`: this PipeWire node and no
    /// other, instead of the default camera. Fails if no camera gives a picture within a few
    /// seconds, or this computer has no encoder for it.
    pub fn start(from: Option<&str>, on_frame: impl FnMut(&[u8], bool) + Send + 'static) -> Result<Source> {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Fatal);
        if !any_encoder() {
            bail!("this computer has no H.264 encoder remoteman can use (NVIDIA's or OpenH264)");
        }
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let key = Arc::new(AtomicBool::new(true));
        let (from, key_r) = (from.map(str::to_owned), key.clone());
        let thread = std::thread::Builder::new()
            .name("remoteman-camera-source".into())
            .spawn(move || {
                if let Err(e) = run(from.as_deref(), quit_rx, ready_tx.clone(), key_r, Box::new(on_frame)) {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .context("start the camera thread")?;
        let stop = |thread: JoinHandle<()>| {
            let _ = quit.send(());
            let _ = thread.join();
        };
        match ready_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(Ok(())) => Ok(Source { quit, thread: Some(thread), key }),
            Ok(Err(e)) => {
                stop(thread);
                Err(e)
            }
            Err(_) => {
                stop(thread);
                bail!("no camera gave a picture")
            }
        }
    }

    /// The other side lost a frame: the next one is a keyframe.
    pub fn keyframe(&self) {
        self.key.store(true, Ordering::Relaxed);
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the camera and this stream agreed on.
#[derive(Clone, Copy, PartialEq)]
enum Agreed {
    Raw(VideoFormat, u32, u32),
    Mjpeg,
}

struct Scaler(scaling::Context);

/// One kind of incoming picture, fitted and encoded.
struct Encoding {
    source: (Pixel, u32, u32),
    scaler: Scaler,
    scaled: Video,
    encoder: ff::encoder::video::Encoder,
    packet: ff::Packet,
    count: i64,
}

struct State {
    agreed: Option<Agreed>,
    encoding: Option<Encoding>,
    jpeg: Option<ff::decoder::Video>,
    key: Arc<AtomicBool>,
    last: Option<Instant>,
    ready: Option<mpsc::Sender<Result<()>>>,
    on_frame: Box<dyn FnMut(&[u8], bool) + Send>,
}

/// The largest even size within [`MAX`] of the same shape.
fn fit(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(2) as u64, height.max(2) as u64);
    let (mut ow, mut oh) = (w, h);
    if w > MAX.0 as u64 || h > MAX.1 as u64 {
        if w * MAX.1 as u64 >= h * MAX.0 as u64 {
            (ow, oh) = (MAX.0 as u64, h * MAX.0 as u64 / w);
        } else {
            (ow, oh) = (w * MAX.1 as u64 / h, MAX.1 as u64);
        }
    }
    ((ow as u32 & !1).max(2), (oh as u32 & !1).max(2))
}

fn pixel(format: VideoFormat) -> Option<Pixel> {
    Some(match format {
        VideoFormat::I420 => Pixel::YUV420P,
        VideoFormat::NV12 => Pixel::NV12,
        VideoFormat::YUY2 => Pixel::YUYV422,
        VideoFormat::RGBx => Pixel::RGBZ,
        VideoFormat::BGRx => Pixel::BGRZ,
        VideoFormat::RGB => Pixel::RGB24,
        VideoFormat::BGR => Pixel::BGR24,
        _ => return None,
    })
}

/// Copy one PipeWire block (planes one after another, rows `stride` apart) into a frame.
fn fill(frame: &mut Video, format: Pixel, data: &[u8], stride: usize) -> bool {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    // Each plane: its row's bytes, its row count, and its stride in the block.
    let planes: Vec<(usize, usize, usize)> = match format {
        Pixel::YUV420P => vec![(width, height, stride), (width.div_ceil(2), height.div_ceil(2), stride / 2), (width.div_ceil(2), height.div_ceil(2), stride / 2)],
        Pixel::NV12 => vec![(width, height, stride), (width.div_ceil(2) * 2, height.div_ceil(2), stride)],
        Pixel::YUYV422 => vec![(width * 2, height, stride)],
        Pixel::RGB24 | Pixel::BGR24 => vec![(width * 3, height, stride)],
        _ => vec![(width * 4, height, stride)],
    };
    let mut at = 0;
    for (index, (row, rows, step)) in planes.into_iter().enumerate() {
        if step < row || data.len() < at + step * (rows - 1) + row {
            return false;
        }
        let to = frame.stride(index);
        let plane = frame.data_mut(index);
        for y in 0..rows {
            plane[y * to..y * to + row].copy_from_slice(&data[at + y * step..at + y * step + row]);
        }
        at += step * rows;
    }
    true
}

impl State {
    /// Encode `frame` (any size and layout) and hand the result on.
    fn encode(&mut self, frame: &Video) -> Result<()> {
        let source = (frame.format(), frame.width(), frame.height());
        if self.encoding.as_ref().is_none_or(|e| e.source != source) {
            let (width, height) = fit(source.1, source.2);
            // The first of them that opens: FFmpeg lists NVIDIA's encoder on computers that
            // have no such card, and it only fails when opened.
            let mut opened = None;
            let mut why = anyhow!("this computer has no H.264 encoder remoteman can use");
            for (name, options) in ENCODERS {
                let Some(codec) = ff::encoder::find_by_name(name) else { continue };
                let mut context = ff::codec::Context::new_with_codec(codec).encoder().video()?;
                context.set_width(width);
                context.set_height(height);
                context.set_format(Pixel::YUV420P);
                context.set_time_base((1, FPS as i32));
                context.set_frame_rate(Some((FPS as i32, 1)));
                context.set_bit_rate(BITRATE);
                context.set_max_bit_rate(BITRATE);
                context.set_max_b_frames(0);
                context.set_gop(FPS * 2);
                let mut said = ff::Dictionary::new();
                for (option, value) in options {
                    said.set(option, value);
                }
                match context.open_with(said) {
                    Ok(encoder) => {
                        opened = Some(encoder);
                        break;
                    }
                    Err(e) => why = anyhow!("{name} can't be opened: {e}"),
                }
            }
            let Some(encoder) = opened else { return Err(why) };
            let scaler = scaling::Context::get(source.0, source.1, source.2, Pixel::YUV420P, width, height, scaling::Flags::BILINEAR).context("scale the camera's picture")?;
            self.encoding = Some(Encoding { source, scaler: Scaler(scaler), scaled: Video::new(Pixel::YUV420P, width, height), encoder, packet: ff::Packet::empty(), count: 0 });
            self.key.store(true, Ordering::Relaxed);
        }
        let e = self.encoding.as_mut().unwrap();
        e.scaler.0.run(frame, &mut e.scaled)?;
        e.scaled.set_pts(Some(e.count));
        e.count += 1;
        let key = self.key.swap(false, Ordering::Relaxed);
        e.scaled.set_kind(if key { ff::picture::Type::I } else { ff::picture::Type::None });
        e.encoder.send_frame(&e.scaled)?;
        while e.encoder.receive_packet(&mut e.packet).is_ok() {
            if let Some(data) = e.packet.data() {
                (self.on_frame)(data, e.packet.is_key());
            }
        }
        Ok(())
    }

    /// One buffer from the camera.
    fn picture(&mut self, data: &[u8], stride: usize) -> Result<()> {
        // A camera faster than the stream: the extra pictures are left out.
        if self.last.is_some_and(|at| at.elapsed() < Duration::from_micros(1_000_000 / (FPS as u64 + 2))) {
            return Ok(());
        }
        self.last = Some(Instant::now());
        match self.agreed {
            Some(Agreed::Raw(format, width, height)) => {
                let format = pixel(format).context("a picture layout that wasn't asked for")?;
                let mut frame = Video::new(format, width, height);
                let stride = if stride > 0 { stride } else { data.len() / (height as usize).max(1) };
                if !fill(&mut frame, format, data, stride) {
                    bail!("the camera's buffer is smaller than its picture");
                }
                self.encode(&frame)
            }
            Some(Agreed::Mjpeg) => {
                if self.jpeg.is_none() {
                    let found = ff::decoder::find_by_name("mjpeg").context("FFmpeg has no JPEG decoder")?;
                    self.jpeg = Some(ff::codec::Context::new_with_codec(found).decoder().video()?);
                }
                let mut decoder = self.jpeg.take().unwrap();
                let mut frame = Video::empty();
                let mut result = decoder.send_packet(&ff::Packet::copy(data)).map_err(anyhow::Error::from);
                while result.is_ok() && decoder.receive_frame(&mut frame).is_ok() {
                    result = self.encode(&frame);
                }
                self.jpeg = Some(decoder);
                result
            }
            None => Ok(()),
        }
    }
}

fn run(from: Option<&str>, quit: pw::channel::Receiver<()>, ready: mpsc::Sender<Result<()>>, key: Arc<AtomicBool>, on_frame: Box<dyn FnMut(&[u8], bool) + Send>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connect to PipeWire")?;
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Camera");
    if let Some(node) = from {
        // That node or nothing: never another camera in its place.
        props.insert("target.object", node);
        props.insert("node.dont-fallback", "true");
        props.insert("node.dont-reconnect", "true");
        props.insert("node.dont-move", "true");
    }
    let stream = pw::stream::StreamBox::new(&core, "remoteman-camera-source", props)?;
    let _listener = stream
        .add_local_listener_with_user_data(State { agreed: None, encoding: None, jpeg: None, key, last: None, ready: Some(ready), on_frame })
        .param_changed(|_, st, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((kind, subtype)) = spa::param::format_utils::parse_format(param) else { return };
            if kind != spa::param::format::MediaType::Video {
                return;
            }
            st.agreed = if subtype == spa::param::format::MediaSubtype::Raw {
                let mut info = spa::param::video::VideoInfoRaw::new();
                if info.parse(param).is_err() {
                    return;
                }
                Some(Agreed::Raw(info.format(), info.size().width, info.size().height))
            } else {
                Some(Agreed::Mjpeg)
            };
        })
        .process(|stream, st| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let (offset, size, stride, bad) = {
                let chunk = d.chunk();
                (chunk.offset() as usize, chunk.size() as usize, chunk.stride().max(0) as usize, chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED))
            };
            let Some(data) = d.data() else { return };
            if bad || size == 0 || offset + size > data.len() {
                return;
            }
            let result = st.picture(&data[offset..offset + size], stride);
            // The first picture through, or the reason there won't be one, is what the caller
            // is waiting for.
            if let Some(ready) = st.ready.take() {
                let _ = ready.send(result);
            }
        })
        .register()?;

    let raw = format_param(Some(&[VideoFormat::I420, VideoFormat::NV12, VideoFormat::YUY2, VideoFormat::RGBx, VideoFormat::BGRx, VideoFormat::RGB, VideoFormat::BGR]))?;
    let jpeg = format_param(None)?;
    let mut params = [spa::pod::Pod::from_bytes(&raw).context("format pod")?, spa::pod::Pod::from_bytes(&jpeg).context("format pod")?];
    stream.connect(spa::utils::Direction::Input, None, pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS, &mut params)?;
    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| ml.quit());
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// What this stream takes: raw pictures in these layouts, or (`None`) JPEG ones, which is how
/// most webcams give their larger sizes at a full rate. Any size; 1280 by 720 at 30 a second
/// for choice.
fn format_param(raw: Option<&[VideoFormat]>) -> Result<Vec<u8>> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};
    let prop = |key: FormatProperties, value| Property { key: key.as_raw(), flags: PropertyFlags::empty(), value };
    let subtype = if raw.is_some() { MediaSubtype::Raw } else { MediaSubtype::Mjpg };
    let mut properties = vec![prop(FormatProperties::MediaType, Value::Id(Id(MediaType::Video.as_raw()))), prop(FormatProperties::MediaSubtype, Value::Id(Id(subtype.as_raw())))];
    if let Some(formats) = raw {
        let ids: Vec<Id> = formats.iter().map(|f| Id(f.as_raw())).collect();
        properties.push(prop(FormatProperties::VideoFormat, Value::Choice(ChoiceValue::Id(Choice(ChoiceFlags::empty(), ChoiceEnum::Enum { default: ids[0], alternatives: ids })))));
    }
    properties.push(prop(
        FormatProperties::VideoSize,
        Value::Choice(ChoiceValue::Rectangle(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range { default: Rectangle { width: MAX.0, height: MAX.1 }, min: Rectangle { width: 16, height: 16 }, max: Rectangle { width: 4096, height: 4096 } },
        ))),
    ));
    properties.push(prop(
        FormatProperties::VideoFramerate,
        Value::Choice(ChoiceValue::Fraction(Choice(
            ChoiceFlags::empty(),
            ChoiceEnum::Range { default: Fraction { num: FPS, denom: 1 }, min: Fraction { num: 0, denom: 1 }, max: Fraction { num: 240, denom: 1 } },
        ))),
    ));
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(spa::pod::Object { type_: SpaTypes::ObjectParamFormat.as_raw(), id: spa::param::ParamType::EnumFormat.as_raw(), properties }),
    )
    .map_err(|e| anyhow!("serialize pod: {e:?}"))?
    .0
    .into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_within_the_protocols_size() {
        assert_eq!(fit(1280, 720), (1280, 720));
        assert_eq!(fit(640, 480), (640, 480));
        assert_eq!(fit(1920, 1080), (1280, 720));
        assert_eq!(fit(1080, 1920), (404, 720));
        assert_eq!(fit(1600, 1200), (960, 720));
        assert_eq!(fit(641, 481), (640, 480));
    }

    #[test]
    fn rows_are_copied_by_stride() {
        let mut frame = Video::new(Pixel::YUYV422, 4, 2);
        // Two rows of 8 bytes, 10 apart.
        let block: Vec<u8> = (0..20).collect();
        assert!(fill(&mut frame, Pixel::YUYV422, &block[..18], 10));
        let stride = frame.stride(0);
        assert_eq!(&frame.data(0)[..8], &block[..8]);
        assert_eq!(&frame.data(0)[stride..stride + 8], &block[10..18]);
        assert!(!fill(&mut frame, Pixel::YUYV422, &block[..12], 10));
    }
}
