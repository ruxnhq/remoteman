//! The camera end to end, on the desktop's own PipeWire (there is no other): H.264 test clips
//! pushed frame by frame at 30 a second, the camera node read back the way a camera program
//! reads it, and what comes out compared with FFmpeg's own decoding of the same clips.
//!
//! Everything it puts on the graph is named `remoteman-test-...` and is gone when it ends (about
//! 13 seconds). Its consumers name the test camera as their target and refuse any other
//! (`node.dont-fallback`), so the real webcam is never opened. No GPU: the clips are made with a
//! software encoder and decoded in software.
//!
//! Needs the `ffmpeg` tool with libx264 or libopenh264, and a running PipeWire; without them
//! the test says so and is skipped. `gst-launch-1.0` (with the PipeWire plugin) is used for an
//! independent read-back; without it that check alone is skipped.
//!
//! `cargo test -p remoteman-camera --test camera -- --nocapture` shows the PASS/FAIL lines.

mod common;

use std::collections::HashMap;
use std::fmt::Display;
use std::hash::Hasher;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::spa;
use remoteman_camera::{Camera, IDLE_INTERVAL, Pushed};
use spa::param::video::VideoFormat;

const W: usize = 1280;
const H: usize = 720;
const I420_BYTES: usize = W * H * 3 / 2;
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 30);

// ---------------------------------------------------------------------------------------------
// Reporting

#[derive(Default)]
struct Report {
    failed: Vec<String>,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: impl Display) {
        println!("{} {name}: {detail}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            self.failed.push(name.to_owned());
        }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// (mean, max) of some durations, in milliseconds.
fn mean_max(times: &[Duration]) -> (f64, f64) {
    let max = times.iter().max().copied().unwrap_or_default();
    (ms(times.iter().sum::<Duration>()) / times.len().max(1) as f64, ms(max))
}

// ---------------------------------------------------------------------------------------------
// Tools

/// A tool's standard output, if it ran and succeeded.
fn output_of(tool: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(tool).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn ffmpeg_command(args: &[&str]) -> Command {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]).args(args);
    command
}

/// Run ffmpeg to its end; panics with what it said if it fails.
fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = ffmpeg_command(args).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

/// A whole buffer's fingerprint. (std's hasher is compiled optimised even in a debug test.)
fn hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

/// Run ffmpeg writing raw pictures of `bytes` each to its standard output; returns each one's
/// fingerprint, and picture number `keep` itself.
fn reference(args: &[&str], bytes: usize, keep: Option<usize>) -> (Vec<u64>, Vec<u8>) {
    let mut child = ffmpeg_command(args).stdout(Stdio::piped()).spawn().expect("run ffmpeg");
    let mut out = child.stdout.take().unwrap();
    let (mut hashes, mut kept, mut picture) = (Vec::new(), Vec::new(), vec![0u8; bytes]);
    while out.read_exact(&mut picture).is_ok() {
        if keep == Some(hashes.len()) {
            kept = picture.clone();
        }
        hashes.push(hash(&picture));
    }
    assert!(child.wait().expect("ffmpeg").success(), "ffmpeg {args:?} failed");
    (hashes, kept)
}

/// A software H.264 encoder this ffmpeg has, never a hardware one.
fn software_encoder() -> Option<&'static str> {
    let encoders = output_of("ffmpeg", &["-hide_banner", "-encoders"])?;
    ["libx264", "libopenh264"].into_iter().find(|name| encoders.lines().any(|l| l.split_whitespace().nth(1) == Some(name)))
}

/// A test clip as a device would send it: Annex-B, no B-frames, a keyframe every 2 seconds with
/// the parameter sets in front.
fn make_clip(encoder: &str, path: &Path, size: &str, seconds: &str) {
    let source = format!("testsrc2=size={size}:rate=30");
    let mut args = vec!["-f", "lavfi", "-i", &source, "-t", seconds, "-c:v", encoder, "-bf", "0", "-g", "60", "-pix_fmt", "yuv420p"];
    if encoder == "libx264" {
        args.extend(["-preset", "veryfast", "-tune", "zerolatency", "-sc_threshold", "0", "-x264-params", "aud=1"]);
    }
    args.extend(["-bsf:v", "h264_mp4toannexb", "-f", "h264", path.to_str().unwrap()]);
    ffmpeg(&args);
}

/// Kills a child process (by its own handle) when dropped, whatever happens in between.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ---------------------------------------------------------------------------------------------
// A consumer: a pw-stream reading the test camera, as a camera program would.

/// One buffer received.
struct Received {
    at: Instant,
    /// The fingerprint of its picture (a YUY2 picture is first put back into three planes).
    hash: u64,
    bytes: usize,
    /// From the buffer's header: its timestamp (ns, monotonic) and count.
    pts: i64,
    seq: u64,
}

#[derive(Default)]
struct Shared {
    /// The negotiated format and size.
    format: Option<(VideoFormat, u32, u32)>,
    received: Vec<Received>,
    /// The last picture, as received.
    last: Vec<u8>,
    /// Pictures whose YUY2 rows did not carry the same colour twice (they always should).
    bad_rows: usize,
    error: Option<String>,
}

struct Consumer {
    quit: pw::channel::Sender<()>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Mutex<Shared>>,
}

impl Consumer {
    /// Read the camera node called `target` in `format`, and nothing else.
    fn start(target: &str, format: VideoFormat) -> Consumer {
        let (quit, quit_rx) = pw::channel::channel::<()>();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (target, for_thread) = (target.to_owned(), shared.clone());
        let thread = std::thread::spawn(move || {
            if let Err(e) = consume(&target, format, for_thread.clone(), quit_rx) {
                for_thread.lock().unwrap().error = Some(format!("{e:#}"));
            }
        });
        Consumer { quit, thread: Some(thread), shared }
    }

    fn count(&self) -> usize {
        self.shared.lock().unwrap().received.len()
    }

    /// Wait (up to `limit`) until `count` buffers have come.
    fn wait_for(&self, count: usize, limit: Duration) -> bool {
        let start = Instant::now();
        while self.count() < count {
            if start.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// Disconnect from the camera and hand back what was received.
    fn stop(mut self) -> Shared {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        std::mem::take(&mut *self.shared.lock().unwrap())
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        let _ = self.quit.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[repr(transparent)]
struct Header(spa::sys::spa_meta_header);

impl spa::buffer::meta::Metadata for Header {
    const META_TYPE: u32 = spa::sys::SPA_META_Header;
}

fn pod(object: spa::pod::Object) -> Vec<u8> {
    spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object)).expect("serialize pod").0.into_inner()
}

fn consume(target: &str, format: VideoFormat, shared: Arc<Mutex<Shared>>, quit: pw::channel::Receiver<()>) -> anyhow::Result<()> {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::pod::{ChoiceValue, Property, PropertyFlags, Value};
    use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Video");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Camera");
    props.insert(*pw::keys::NODE_NAME, format!("remoteman-test-consumer-{}", std::process::id()));
    // This camera or none: never another one in its place, never moved to one.
    props.insert("target.object", target);
    props.insert("node.dont-fallback", "true");
    props.insert("node.dont-reconnect", "true");
    props.insert("node.dont-move", "true");
    let stream = pw::stream::StreamBox::new(&core, "remoteman-test-consumer", props)?;

    let _listener = stream
        .add_local_listener_with_user_data(shared)
        .state_changed(|_, shared, _, new| {
            if let pw::stream::StreamState::Error(e) = new {
                shared.lock().unwrap().error = Some(e);
            }
        })
        .param_changed(|stream, shared, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            shared.lock().unwrap().format = Some((info.format(), info.size().width, info.size().height));
            // Ask for the header on every buffer.
            let meta = pod(spa::pod::Object {
                type_: spa::sys::SPA_TYPE_OBJECT_ParamMeta,
                id: spa::sys::SPA_PARAM_Meta,
                properties: vec![
                    Property { key: spa::sys::SPA_PARAM_META_type, flags: PropertyFlags::empty(), value: Value::Id(Id(spa::sys::SPA_META_Header)) },
                    Property { key: spa::sys::SPA_PARAM_META_size, flags: PropertyFlags::empty(), value: Value::Int(std::mem::size_of::<spa::sys::spa_meta_header>() as i32) },
                ],
            });
            let _ = stream.update_params(&mut [spa::pod::Pod::from_bytes(&meta).unwrap()]);
        })
        .process(|stream, shared| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let at = Instant::now();
            let (pts, seq) = buffer.find_meta::<Header>().map_or((0, 0), |h| (h.0.pts, h.0.seq));
            let Some(block) = buffer.datas_mut().first_mut() else { return };
            let (offset, size) = (block.chunk().offset() as usize, block.chunk().size() as usize);
            let Some(data) = block.data() else { return };
            let Some(picture) = data.get(offset..offset + size) else { return };
            let mut shared = shared.lock().unwrap();
            let hash = match shared.format {
                Some((VideoFormat::YUY2, w, h)) if size == w as usize * h as usize * 2 => {
                    let (planes, rows_agree) = yuy2_to_i420(picture, w as usize, h as usize);
                    shared.bad_rows += usize::from(!rows_agree);
                    hash(&planes)
                }
                _ => hash(picture),
            };
            shared.last.clear();
            shared.last.extend_from_slice(picture);
            shared.received.push(Received { at, hash, bytes: size, pts, seq });
        })
        .register()?;

    let prop = |key: FormatProperties, value| Property { key: key.as_raw(), flags: PropertyFlags::empty(), value };
    let wanted = pod(spa::pod::Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            prop(FormatProperties::MediaType, Value::Id(Id(MediaType::Video.as_raw()))),
            prop(FormatProperties::MediaSubtype, Value::Id(Id(MediaSubtype::Raw.as_raw()))),
            prop(FormatProperties::VideoFormat, Value::Id(Id(format.as_raw()))),
            // Any size and rate: the test checks what the camera gives.
            prop(
                FormatProperties::VideoSize,
                Value::Choice(ChoiceValue::Rectangle(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range { default: Rectangle { width: 640, height: 480 }, min: Rectangle { width: 1, height: 1 }, max: Rectangle { width: 8192, height: 8192 } },
                ))),
            ),
            prop(
                FormatProperties::VideoFramerate,
                Value::Choice(ChoiceValue::Fraction(Choice(
                    ChoiceFlags::empty(),
                    ChoiceEnum::Range { default: Fraction { num: 15, denom: 1 }, min: Fraction { num: 0, denom: 1 }, max: Fraction { num: 240, denom: 1 } },
                ))),
            ),
        ],
    });
    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut [spa::pod::Pod::from_bytes(&wanted).unwrap()],
    )?;
    let ml = mainloop.clone();
    let _quit = quit.attach(mainloop.loop_(), move |()| ml.quit());
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// A YUY2 picture as three planes, taking the colour of every other row; and whether the rows
/// in between carried the same colour (the camera makes YUY2 from 4:2:0 by saying each colour
/// row twice).
fn yuy2_to_i420(yuy2: &[u8], w: usize, h: usize) -> (Vec<u8>, bool) {
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y, chroma) = out.split_at_mut(w * h);
    let (cb, cr) = chroma.split_at_mut(w * h / 4);
    let mut rows_agree = true;
    for (row, line) in yuy2.chunks_exact(w * 2).enumerate() {
        for (x, quad) in line.chunks_exact(4).enumerate() {
            y[row * w + x * 2] = quad[0];
            y[row * w + x * 2 + 1] = quad[2];
            let at = row / 2 * (w / 2) + x;
            if row % 2 == 0 {
                cb[at] = quad[1];
                cr[at] = quad[3];
            } else {
                rows_agree &= cb[at] == quad[1] && cr[at] == quad[3];
            }
        }
    }
    (out, rows_agree)
}

// ---------------------------------------------------------------------------------------------
// The test

/// What a received picture is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Is {
    /// The neutral picture: black.
    Black,
    /// Frame n of the 1280x720 clip, exactly as FFmpeg decodes it.
    Landscape(usize),
    /// Frame n of the 720x1280 clip, exactly as FFmpeg scales it to fit and pads it.
    Portrait(usize),
}

/// Push `frames` at 30 a second. Returns when each push began and how long it took, and whether
/// every one was shown.
fn push_paced(camera: &mut Camera, frames: &[common::Frame], capture_base_us: u64) -> (Vec<Instant>, Vec<Duration>, bool) {
    let start = Instant::now();
    let (mut began, mut took, mut all_shown) = (Vec::new(), Vec::new(), true);
    for (i, frame) in frames.iter().enumerate() {
        if let Some(wait) = (FRAME * i as u32).checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        let begin = Instant::now();
        all_shown &= camera.push(&frame.data, frame.key, capture_base_us + i as u64 * 33_333) == Pushed::Shown;
        took.push(begin.elapsed());
        began.push(begin);
    }
    (began, took, all_shown)
}

/// Black bars left and right of a 406-wide picture in a 1280x720 I420 picture, and something
/// other than black in between.
fn is_pillarboxed(i420: &[u8]) -> bool {
    let row = |r: usize| &i420[r * W..(r + 1) * W];
    let bars = [10, H / 2, H - 10].iter().all(|&r| row(r)[..436].iter().chain(&row(r)[842..]).all(|&v| v == 16));
    let middle = row(H / 2)[436..842].iter().any(|&v| v > 40);
    bars && middle
}

/// Points of an I420 picture (a grid 40 apart) where the picture is one flat colour around
/// them, so that how a reader places the colour samples makes no difference.
fn flat_points(i420: &[u8]) -> Vec<(usize, usize)> {
    let (cb, cr) = (&i420[W * H..W * H * 5 / 4], &i420[W * H * 5 / 4..]);
    let mut points: Vec<(usize, usize)> = Vec::new();
    let mut seen: Vec<(u8, u8, u8)> = Vec::new();
    for y in (40..H - 40).step_by(40) {
        for x in (40..W - 40).step_by(40) {
            let colour = (i420[y * W + x], cb[y / 2 * (W / 2) + x / 2], cr[y / 2 * (W / 2) + x / 2]);
            let flat = (y - 8..y + 8).all(|r| (x - 8..x + 8).all(|c| (i420[r * W + c], cb[r / 2 * (W / 2) + c / 2], cr[r / 2 * (W / 2) + c / 2]) == colour));
            if flat && !seen.contains(&colour) {
                seen.push(colour);
                points.push((x, y));
            }
        }
    }
    points
}

/// The RGB a reader makes of a video-range YCbCr sample, by the BT.709 or the BT.601 matrix.
fn to_rgb((y, cb, cr): (u8, u8, u8), bt709: bool) -> [f64; 3] {
    let (y, cb, cr) = ((y as f64 - 16.0) * 255.0 / 219.0, (cb as f64 - 128.0) * 255.0 / 224.0, (cr as f64 - 128.0) * 255.0 / 224.0);
    let (kr, kb) = if bt709 { (0.2126, 0.0722) } else { (0.299, 0.114) };
    let kg = 1.0 - kr - kb;
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = (y - kr * r - kb * b) / kg;
    [r.clamp(0.0, 255.0), g.clamp(0.0, 255.0), b.clamp(0.0, 255.0)]
}

#[test]
fn camera_on_the_desktop() {
    // --- What the test needs -------------------------------------------------------------------
    let Some(encoder) = software_encoder() else {
        println!("SKIP: no ffmpeg tool with libx264 or libopenh264");
        return;
    };
    if output_of("pw-cli", &["info", "0"]).is_none() {
        println!("SKIP: no PipeWire to talk to (pw-cli info 0 failed)");
        return;
    }
    let mut report = Report::default();
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("camera");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = |name: &str| dir.join(name).to_str().unwrap().to_owned();

    // --- Clips, and FFmpeg's own idea of their pictures ----------------------------------------
    let (landscape_clip, portrait_clip) = (file("landscape.h264"), file("portrait.h264"));
    make_clip(encoder, Path::new(&landscape_clip), "1280x720", "3");
    make_clip(encoder, Path::new(&portrait_clip), "720x1280", "2");
    let landscape = common::split_access_units(&std::fs::read(&landscape_clip).unwrap());
    let portrait = common::split_access_units(&std::fs::read(&portrait_clip).unwrap());
    let keys = |frames: &[common::Frame]| frames.iter().enumerate().filter(|(_, f)| f.key).map(|(i, _)| i).collect::<Vec<_>>();
    report.check(
        "test clips",
        landscape.len() == 90 && portrait.len() == 60 && keys(&landscape) == [0, 60] && keys(&portrait) == [0],
        format!("{encoder}: 1280x720 {} frames (keyframes at {:?}), 720x1280 {} frames (keyframes at {:?})", landscape.len(), keys(&landscape), portrait.len(), keys(&portrait)),
    );
    if !report.failed.is_empty() {
        panic!("the test clips are not what the test expects");
    }
    // The last frame pushed before the pause is 88: frame 89 is kept back to be damaged.
    const LAST: usize = 88;
    // A 720x1280 picture in a 1280x720 camera: 406x720, 436 from the left.
    let fitted = "scale=406:720:flags=bicubic,pad=1280:720:436:0:black";
    let (landscape_i420, last_i420) = reference(&["-i", &landscape_clip, "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"], I420_BYTES, Some(LAST));
    let (landscape_nv12, _) = reference(&["-i", &landscape_clip, "-f", "rawvideo", "-pix_fmt", "nv12", "pipe:1"], I420_BYTES, None);
    let (portrait_i420, _) = reference(&["-i", &portrait_clip, "-vf", fitted, "-f", "rawvideo", "-pix_fmt", "yuv420p", "pipe:1"], I420_BYTES, None);
    let (portrait_nv12, _) = reference(&["-i", &portrait_clip, "-vf", fitted, "-f", "rawvideo", "-pix_fmt", "nv12", "pipe:1"], I420_BYTES, None);
    assert_eq!((landscape_i420.len(), landscape_nv12.len(), portrait_i420.len(), portrait_nv12.len()), (90, 90, 60, 60), "reference pictures");
    let mut black = vec![16u8; I420_BYTES];
    black[W * H..].fill(128);
    // What each fingerprint is, for pictures read in three planes (I420, and YUY2 put back into
    // planes) and for pictures read as NV12 (black looks the same in both).
    let known = |landscape: &[u64], portrait: &[u64]| {
        let mut map = HashMap::from([(hash(&black), Is::Black)]);
        map.extend(landscape.iter().enumerate().map(|(i, &h)| (h, Is::Landscape(i))));
        map.extend(portrait.iter().enumerate().map(|(i, &h)| (h, Is::Portrait(i))));
        map
    };
    let (planar, nv12) = (known(&landscape_i420, &portrait_i420), known(&landscape_nv12, &portrait_nv12));

    // --- The camera appears ---------------------------------------------------------------------
    let name = format!("remoteman-test-camera-{}", std::process::id());
    let mut camera = Camera::start(&name).expect("start the camera");
    let id = camera.node_id().to_string();
    let info = output_of("pw-cli", &["info", &id]).unwrap_or_default();
    let has = |key: &str, value: &str| info.contains(&format!("{key} = \"{value}\""));
    report.check(
        "node listed as a camera",
        output_of("pw-cli", &["ls", "Node"]).is_some_and(|l| l.contains(&name)) && has("media.class", "Video/Source") && has("media.role", "Camera") && has("node.name", &name) && has("node.description", &name),
        format!("node {id}: media.class=Video/Source {}, media.role=Camera {}, node.name and node.description \"{name}\" {}", has("media.class", "Video/Source"), has("media.role", "Camera"), has("node.name", &name) && has("node.description", &name)),
    );
    let status = output_of("wpctl", &["status"]).unwrap_or_default();
    report.check("in wpctl status", status.contains(&name), "among the video sources");
    let formats = output_of("pw-cli", &["enum-params", &id, "EnumFormat"]).unwrap_or_default();
    let offers = |format: &str| formats.contains(&format!("VideoFormat:{format}"));
    report.check(
        "formats offered",
        offers("NV12") && offers("I420") && offers("YUY2") && formats.matches("1280x720").count() == 3 && formats.matches("30/1").count() == 3,
        format!("NV12 {}, I420 {}, YUY2 {}; {} of 3 at 1280x720, {} of 3 at 30/1 (fixed values, as camera programs read them)", offers("NV12"), offers("I420"), offers("YUY2"), formats.matches("1280x720").count(), formats.matches("30/1").count()),
    );
    report.check("camera size", camera.size() == (1280, 720) && camera.node_name() == name, format!("{:?}", camera.size()));
    println!("INFO loopback device: {:?} (None: that output is not exercised here)", camera.loopback_device());

    // --- Before the first frame: black, 5 a second ---------------------------------------------
    let consumer = Consumer::start(&name, VideoFormat::I420);
    let came = consumer.wait_for(4, Duration::from_secs(3));
    {
        let shared = consumer.shared.lock().unwrap();
        let all_black = shared.received.iter().all(|r| planar.get(&r.hash) == Some(&Is::Black));
        let gaps: Vec<f64> = shared.received.windows(2).map(|p| ms(p[1].at - p[0].at)).collect();
        report.check(
            "before the first frame",
            came && all_black && gaps.iter().skip(1).all(|g| (150.0..260.0).contains(g)),
            format!("{} black pictures, {:.0?} ms apart; negotiated {:?}; error {:?}", shared.received.len(), gaps, shared.format, shared.error),
        );
    }

    // --- A device that starts in the middle of a sequence --------------------------------------
    let early = [camera.push(&landscape[1].data, false, 0), camera.push(&landscape[2].data, false, 0)];
    report.check("frames before a keyframe", early == [Pushed::Lost, Pushed::Waiting], format!("{early:?}: asked for a keyframe once, then waiting"));

    // --- 1280x720 at 30 a second ----------------------------------------------------------------
    let before = consumer.count();
    let (began, took, all_shown) = push_paced(&mut camera, &landscape[..=LAST], 1_000_000);
    std::thread::sleep(Duration::from_millis(60));
    {
        let shared = consumer.shared.lock().unwrap();
        let got: Vec<(&Received, Option<&Is>)> = shared.received[before..].iter().map(|r| (r, planar.get(&r.hash))).collect();
        // Each frame of the clip, the first time it came.
        let mut firsts: Vec<(usize, &Received)> = Vec::new();
        for (r, is) in &got {
            if let Some(Is::Landscape(n)) = is {
                if firsts.last().is_none_or(|(last, _)| last != n) {
                    firsts.push((*n, r));
                }
            }
        }
        let unknown = got.iter().filter(|(_, is)| is.is_none()).count();
        let in_order = firsts.windows(2).all(|p| p[0].0 < p[1].0);
        let span = firsts.last().map_or(Duration::ZERO, |l| l.1.at - firsts[0].1.at);
        let rate = (firsts.len().max(1) - 1) as f64 / span.as_secs_f64().max(1e-9);
        report.check(
            "30 frames a second at 1280x720",
            all_shown && shared.format == Some((VideoFormat::I420, 1280, 720)) && firsts.len() >= 85 && (28.5..31.5).contains(&rate) && got.iter().all(|(r, _)| r.bytes == I420_BYTES),
            format!("pushed {}, received {} different frames in {:.2} s = {:.2} a second; format {:?}, {} bytes each", LAST + 1, firsts.len(), span.as_secs_f64(), rate, shared.format, I420_BYTES),
        );
        report.check(
            "pictures are the clip's, bit for bit",
            unknown == 0 && in_order && firsts.len() >= 85,
            format!("{} of {} received frames identical to FFmpeg's decoding of the same frame, in order; {unknown} unknown pictures", firsts.len(), firsts.len() + unknown),
        );
        let delays: Vec<Duration> = firsts.iter().map(|(n, r)| r.at.saturating_duration_since(began[*n])).collect();
        let (push_mean, push_max) = mean_max(&took);
        let (delay_mean, delay_max) = mean_max(&delays);
        report.check(
            "decode time and delay",
            push_max < 33.0 && delay_max < 50.0,
            format!("push() (decode, copy, hand over) mean {push_mean:.2} ms, max {push_max:.2} ms; push to consumer mean {delay_mean:.2} ms, max {delay_max:.2} ms"),
        );
        let stamps: Vec<i64> = firsts.iter().map(|(_, r)| r.pts).collect();
        let steps: Vec<f64> = stamps.windows(2).map(|p| (p[1] - p[0]) as f64 / 1e6).collect();
        let even = steps.iter().filter(|s| (33.0..34.0).contains(*s)).count();
        let counted = got.windows(2).all(|p| p[1].0.seq == p[0].0.seq + 1);
        report.check(
            "timestamps",
            !steps.is_empty() && even * 10 >= steps.len() * 9 && counted,
            format!("{even} of {} steps between frames are the capture clock's 33.3 ms; buffer counts consecutive: {counted}", steps.len()),
        );
    }

    // --- A pause: the last picture stays, 5 a second; GStreamer reads it meanwhile -------------
    let pause_from = consumer.count();
    let pause_start = Instant::now();
    let png = file("landscape-frame-88.png");
    let gst = Command::new("gst-launch-1.0")
        .args(["-q", "pipewiresrc", &format!("target-object={name}"), "stream-properties=props,node.dont-fallback=true,node.dont-reconnect=true,node.dont-move=true", "num-buffers=2", "!"])
        .args(["videoconvert", "!", "pngenc", "snapshot=true", "!", "filesink", &format!("location={png}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match gst {
        Ok(child) => {
            let mut child = KillOnDrop(child);
            let finished = loop {
                match child.0.try_wait() {
                    Ok(Some(status)) => break status.success(),
                    Ok(None) if pause_start.elapsed() < Duration::from_secs(4) => std::thread::sleep(Duration::from_millis(20)),
                    _ => break false,
                }
            };
            drop(child);
            // What GStreamer saw, against frame 88 as FFmpeg decodes it, where the picture is flat.
            let rgb = if finished { ffmpeg(&["-i", &png, "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]) } else { Vec::new() };
            let points = flat_points(&last_i420);
            let (cb, cr) = (&last_i420[W * H..W * H * 5 / 4], &last_i420[W * H * 5 / 4..]);
            let mut worst = [0f64; 2];
            let mut lines = Vec::new();
            if rgb.len() == W * H * 3 {
                for &(x, y) in &points {
                    let sample = (last_i420[y * W + x], cb[y / 2 * (W / 2) + x / 2], cr[y / 2 * (W / 2) + x / 2]);
                    let seen = &rgb[(y * W + x) * 3..(y * W + x) * 3 + 3];
                    for (matrix, worst) in worst.iter_mut().enumerate() {
                        let expected = to_rgb(sample, matrix == 0);
                        for c in 0..3 {
                            *worst = worst.max((expected[c] - seen[c] as f64).abs());
                        }
                    }
                    lines.push(format!("({x},{y}) YCbCr {sample:?} -> RGB {seen:?}"));
                }
            }
            let best = worst[0].min(worst[1]);
            report.check(
                "GStreamer reads the same picture",
                finished && rgb.len() == W * H * 3 && points.len() >= 6 && best <= 6.0,
                format!(
                    "pipewiresrc ! videoconvert ! pngenc wrote {png} ({}x{} RGB); {} flat points of frame {LAST} compared, worst difference {:.1} of 255 by BT.709, {:.1} by BT.601: {}",
                    W,
                    H,
                    points.len(),
                    worst[0],
                    worst[1],
                    lines.join("; ")
                ),
            );
        }
        Err(e) => println!("SKIP GStreamer reads the same picture: gst-launch-1.0: {e}"),
    }
    if let Some(rest) = Duration::from_millis(1300).checked_sub(pause_start.elapsed()) {
        std::thread::sleep(rest);
    }
    {
        let shared = consumer.shared.lock().unwrap();
        let paused = pause_start.elapsed();
        let repeats = &shared.received[pause_from..];
        let same = repeats.iter().all(|r| planar.get(&r.hash) == Some(&Is::Landscape(LAST)));
        let gaps: Vec<f64> = repeats.windows(2).map(|p| ms(p[1].at - p[0].at)).collect();
        let expected = paused.as_secs_f64() / IDLE_INTERVAL.as_secs_f64();
        report.check(
            "paused: the last picture, 5 a second",
            same && (repeats.len() as f64 - expected).abs() <= 2.0 && gaps.iter().all(|g| (150.0..260.0).contains(g)),
            format!("{} repeats of frame {LAST} in {:.2} s, {:.0}..{:.0} ms apart", repeats.len(), paused.as_secs_f64(), gaps.iter().cloned().fold(f64::MAX, f64::min), gaps.iter().cloned().fold(0.0, f64::max)),
        );
    }

    // --- Damaged frames: never shown, never fatal ----------------------------------------------
    let bad_from = consumer.count();
    let cut = &landscape[LAST + 1].data;
    let noise: Vec<u8> = (0..30_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    let bad = [
        camera.push(&cut[..cut.len() / 2], false, 0), // the next frame, cut in half
        camera.push(cut, false, 0),                   // the same frame whole: too late
        camera.push(&[], true, 0),
        camera.push(&noise, true, 0),
        camera.push(&landscape[0].data[..landscape[0].data.len() / 3], true, 0), // a third of a keyframe
        camera.push(&landscape[5].data, false, 0),
    ];
    std::thread::sleep(Duration::from_millis(250));
    {
        let shared = consumer.shared.lock().unwrap();
        let shown: Vec<Option<&Is>> = shared.received[bad_from..].iter().map(|r| planar.get(&r.hash)).collect();
        report.check(
            "damaged frames",
            bad == [Pushed::Lost, Pushed::Waiting, Pushed::Lost, Pushed::Lost, Pushed::Lost, Pushed::Waiting] && shown.iter().all(|is| *is == Some(&Is::Landscape(LAST))),
            format!("half a frame, that frame whole, nothing, noise, a third of a keyframe, a later frame: {bad:?}; meanwhile {} pictures went out, all still frame {LAST}", shown.len()),
        );
    }

    // --- The device turned on its side: 720x1280 from a keyframe on ----------------------------
    let portrait_from = consumer.count();
    let (_, took, all_shown) = push_paced(&mut camera, &portrait, 9_000_000);
    std::thread::sleep(Duration::from_millis(60));
    let portrait_png = file("portrait-in-landscape.png");
    {
        let shared = consumer.shared.lock().unwrap();
        let got: Vec<Option<&Is>> = shared.received[portrait_from..].iter().map(|r| planar.get(&r.hash)).collect();
        let exact = got.iter().filter(|is| matches!(is, Some(Is::Portrait(_)))).count();
        let other = got.iter().filter(|is| !matches!(is, Some(Is::Portrait(_)) | Some(Is::Landscape(LAST)))).count();
        let (push_mean, push_max) = mean_max(&took);
        report.check(
            "size change: scaled to fit, centred on black, same 1280x720",
            all_shown && exact >= 57 && other == 0 && shared.format == Some((VideoFormat::I420, 1280, 720)) && shared.received[portrait_from..].iter().all(|r| r.bytes == I420_BYTES) && is_pillarboxed(&shared.last),
            format!(
                "pushed {} frames of 720x1280; received {exact} identical to FFmpeg's `{fitted}`, {other} other; still {:?}; bars black: {}; push() mean {push_mean:.2} ms, max {push_max:.2} ms",
                portrait.len(),
                shared.format,
                is_pillarboxed(&shared.last)
            ),
        );
        let raw = file("portrait-in-landscape.yuv");
        std::fs::write(&raw, &shared.last).unwrap();
        ffmpeg(&["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "1280x720", "-i", &raw, "-frames:v", "1", &portrait_png]);
        let _ = std::fs::remove_file(&raw);
        println!("INFO the last 720x1280 picture as the consumer got it: {portrait_png}");
    }
    let shared = consumer.stop();
    report.check("consumer", shared.error.is_none(), format!("{} buffers in all, error {:?}", shared.received.len(), shared.error));

    // --- The other two layouts: one second of the clip each ------------------------------------
    for (format, known, label) in [(VideoFormat::NV12, &nv12, "NV12"), (VideoFormat::YUY2, &planar, "YUY2")] {
        let consumer = Consumer::start(&name, format);
        let came = consumer.wait_for(1, Duration::from_secs(3));
        let (began, _, all_shown) = push_paced(&mut camera, &landscape[..30], 20_000_000);
        std::thread::sleep(Duration::from_millis(60));
        let shared = consumer.stop();
        // (What came before the first push is the picture left from before, said again.)
        let got: Vec<Option<&Is>> = shared.received.iter().filter(|r| r.at >= began[0]).map(|r| known.get(&r.hash)).collect();
        let mut frames: Vec<usize> = got.iter().filter_map(|is| if let Some(Is::Landscape(n)) = is { Some(*n) } else { None }).collect();
        frames.dedup();
        let unknown = got.iter().filter(|is| is.is_none()).count();
        let bytes = if format == VideoFormat::YUY2 { W * H * 2 } else { I420_BYTES };
        report.check(
            &format!("read as {label}"),
            came && all_shown && shared.format == Some((format, 1280, 720)) && frames.len() >= 28 && unknown == 0 && shared.bad_rows == 0 && shared.received.iter().all(|r| r.bytes == bytes) && shared.error.is_none(),
            format!("{} of 30 frames received and identical to FFmpeg's, {unknown} unknown pictures; negotiated {:?}, {bytes} bytes each; error {:?}", frames.len(), shared.format, shared.error),
        );
    }

    // --- The camera goes --------------------------------------------------------------------------
    drop(camera);
    let gone_after = Instant::now();
    let mut gone = false;
    while !gone && gone_after.elapsed() < Duration::from_secs(3) {
        gone = output_of("pw-cli", &["ls", "Node"]).is_some_and(|l| !l.contains("remoteman-test-"));
        if !gone {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    report.check("after drop the node is gone", gone, format!("no remoteman-test- node in `pw-cli ls Node` {:.0} ms after drop", ms(gone_after.elapsed())));

    for clip in [landscape_clip, portrait_clip] {
        let _ = std::fs::remove_file(clip);
    }
    assert!(report.failed.is_empty(), "failed checks: {:?}", report.failed);
}
