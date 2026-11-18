//! The four PipeWire nodes: each is one stream, run by a loop on its own thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::stream::{Stream, StreamFlags, StreamState};

use crate::frame::Framer;
use crate::jitter::{JITTER_FRAMES, JitterBuffer, JitterStats};
use crate::{Channels, Packet, SAMPLE_RATE};

/// The graph is asked to run in cycles of one frame while one of these nodes is in it. With
/// PipeWire's usual 1024 samples, packets would leave in bursts of two or three every 21 ms,
/// which a 30 ms jitter buffer barely covers. (PipeWire rounds to a power of two: 256.)
const LATENCY: &str = "480/48000";

/// How long PipeWire gets to create a node.
const START_TIMEOUT: Duration = Duration::from_secs(5);

/// The most samples per channel written into one output buffer when PipeWire doesn't say how
/// many it wants.
const MAX_QUANTUM: usize = 8192;

/// A node on the PipeWire graph and the thread that runs it. Dropping it removes the node.
pub struct Node {
    name: String,
    id: u32,
    status: Arc<Status>,
    quit: Mutex<pw::channel::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Status {
    streaming: AtomicBool,
    error: Mutex<Option<String>>,
}

impl Node {
    /// The `node.name`, which is how other programs and [`crate::DefaultSink`] refer to it.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The id PipeWire gave it (as `pw-cli ls Node` shows).
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Whether sound is flowing: something is linked to the node and running. A sink nobody
    /// plays to, or a source nobody records from, isn't.
    pub fn is_streaming(&self) -> bool {
        self.status.streaming.load(Ordering::Relaxed)
    }

    /// Why the node stopped working, if it did: PipeWire went away, or removed the node (a
    /// [`Player`] or [`MicCapture`] whose target doesn't exist). It doesn't recover; make a new
    /// one.
    pub fn error(&self) -> Option<String> {
        self.status.error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.quit.lock().unwrap_or_else(|e| e.into_inner()).send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What kind of node a stream is.
struct Spec {
    name: String,
    description: Option<String>,
    /// `media.class` for a device other programs see; `None` for an ordinary stream, which is
    /// linked to the default device or to `target`.
    class: Option<&'static str>,
    direction: spa::utils::Direction,
    channels: Channels,
    target: Option<String>,
    /// Run the callback on PipeWire's realtime thread instead of the node's own.
    realtime: bool,
}

type Process = Box<dyn FnMut(&Stream) + Send>;

fn start(spec: Spec, process: Process) -> Result<Node> {
    let name = spec.name.clone();
    let status = Arc::new(Status::default());
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let thread = {
        let status = status.clone();
        std::thread::Builder::new().name("remoteman-audio".into()).spawn(move || {
            if let Err(e) = run(spec, process, status.clone(), ready_tx.clone(), quit_rx) {
                let e = format!("{e:#}");
                *status.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
                let _ = ready_tx.send(Err(e));
            }
        })?
    };
    let stop = |thread: JoinHandle<()>| {
        let _ = quit_tx.send(());
        let _ = thread.join();
    };
    match ready_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(id)) => Ok(Node { name, id, status, quit: Mutex::new(quit_tx), thread: Some(thread) }),
        Ok(Err(e)) => {
            stop(thread);
            Err(anyhow!("{name}: {e}"))
        }
        Err(_) => {
            stop(thread);
            Err(anyhow!("{name}: PipeWire did not create the node within {START_TIMEOUT:?}"))
        }
    }
}

fn run(spec: Spec, process: Process, status: Arc<Status>, ready: mpsc::Sender<Result<u32, String>>, quit: pw::channel::Receiver<()>) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None).context("connect to PipeWire")?;

    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::NODE_NAME => spec.name.as_str(),
        *pw::keys::NODE_LATENCY => LATENCY,
    };
    if let Some(description) = &spec.description {
        props.insert(*pw::keys::NODE_DESCRIPTION, description.as_str());
    }
    let mut flags = StreamFlags::MAP_BUFFERS;
    match spec.class {
        // A device: nothing is linked until a program (or the session manager, for the
        // default device) picks it.
        Some(class) => {
            props.insert(*pw::keys::MEDIA_CLASS, class);
            props.insert(*pw::keys::NODE_VIRTUAL, "true");
        }
        None => {
            flags |= StreamFlags::AUTOCONNECT;
            let (category, role) = match spec.direction {
                spa::utils::Direction::Output => ("Playback", "Music"),
                _ => ("Capture", "Communication"),
            };
            props.insert(*pw::keys::MEDIA_CATEGORY, category);
            props.insert(*pw::keys::MEDIA_ROLE, role);
            if let Some(target) = &spec.target {
                props.insert(*pw::keys::TARGET_OBJECT, target.as_str());
                // This node and no other: without these the session manager links the stream
                // to the default device when the target is missing or goes away, moves it when
                // the user or a saved choice says so, and remembers its volume.
                for key in ["node.dont-fallback", "node.dont-reconnect", "node.dont-move"] {
                    props.insert(key, "true");
                }
                for key in ["state.restore-target", "state.restore-props"] {
                    props.insert(key, "false");
                }
            }
        }
    }
    if spec.realtime {
        flags |= StreamFlags::RT_PROCESS;
    }

    let stream = pw::stream::StreamBox::new(&core, &spec.name, props)?;
    let failed = {
        let (status, mainloop) = (status.clone(), mainloop.clone());
        move |why: String| {
            status.streaming.store(false, Ordering::Relaxed);
            status.error.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert(why);
            mainloop.quit();
        }
    };
    let mut announced = false;
    let _listener = stream
        .add_local_listener_with_user_data(process)
        .state_changed({
            let (status, failed, ready) = (status.clone(), failed.clone(), ready.clone());
            move |stream, _, _, state| match state {
                StreamState::Paused | StreamState::Streaming => {
                    status.streaming.store(state == StreamState::Streaming, Ordering::Relaxed);
                    if !announced && stream.node_id() != spa::sys::SPA_ID_INVALID {
                        announced = true;
                        let _ = ready.send(Ok(stream.node_id()));
                    }
                }
                StreamState::Error(e) => failed(format!("PipeWire stream error: {e}")),
                // After it was connected: the node was removed under us.
                StreamState::Unconnected if announced => failed("PipeWire removed the node".into()),
                StreamState::Unconnected | StreamState::Connecting => {}
            }
        })
        .process(|stream, process| process(stream))
        .register()?;
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _, res, message| {
            if id == pw::core::PW_ID_CORE {
                failed(format!("PipeWire connection: {message} ({res})"));
            }
        })
        .register();

    let format = format_param(spec.channels)?;
    let mut params = [spa::pod::Pod::from_bytes(&format).context("format pod")?];
    stream.connect(spec.direction, None, flags, &mut params)?;

    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });
    mainloop.run();
    // Whoever is still waiting for the node learns that it won't come.
    if let Some(e) = status.error.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        let _ = ready.send(Err(e));
    }
    Ok(())
}

/// The one format every node uses: interleaved 32-bit float at 48 kHz. PipeWire converts
/// whatever the other side has.
fn format_param(channels: Channels) -> Result<Vec<u8>> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE);
    info.set_channels(channels.count() as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    match channels {
        Channels::Mono => position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO,
        Channels::Stereo => {
            position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
            position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
        }
    }
    info.set_position(position);
    let object = spa::pod::Object { type_: spa::sys::SPA_TYPE_OBJECT_Format, id: spa::sys::SPA_PARAM_EnumFormat, properties: info.into() };
    Ok(spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(object))
        .map_err(|e| anyhow!("serialize pod: {e:?}"))?
        .0
        .into_inner())
}

/// Where the samples of a buffer belong on the clock.
#[derive(Clone, Copy)]
enum Stamp {
    /// Sound played into a sink arrives a cycle at a time, at the start of the cycle it is
    /// meant for: the first sample is due now.
    Ahead,
    /// Sound recorded from a device arrives when the cycle that recorded it is over: the last
    /// sample is from now.
    Behind,
}

/// The callback of a capturing stream: samples in, packets out through `framer`.
fn capture(channels: Channels, stamp: Stamp, mut clock: Box<dyn FnMut() -> u64 + Send>, mut framer: Framer) -> Process {
    let count = channels.count();
    let mut samples: Vec<f32> = Vec::with_capacity(MAX_QUANTUM * count);
    Box::new(move |stream| {
        let mut now = clock();
        // More than one buffer waits when this thread fell behind.
        while let Some(mut buffer) = stream.dequeue_buffer() {
            let Some(data) = buffer.datas_mut().first_mut() else { continue };
            let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
            let Some(bytes) = data.data() else { continue };
            let end = (offset + size).min(bytes.len());
            samples.clear();
            samples.extend(bytes[offset.min(end)..end].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
            let length_us = (samples.len() / count) as u64 * 1_000_000 / SAMPLE_RATE as u64;
            match stamp {
                Stamp::Ahead => {
                    framer.feed(&samples, now);
                    now += length_us;
                }
                Stamp::Behind => framer.feed(&samples, now.saturating_sub(length_us)),
            }
        }
    })
}

/// The callback of a playing stream: PipeWire asks for samples, `jitter` has them.
fn playback(channels: Channels, jitter: Arc<Mutex<JitterBuffer>>) -> Process {
    let count = channels.count();
    let stride = 4 * count;
    // Sized up front: this runs on the realtime thread, which shouldn't allocate.
    let mut samples = vec![0.0f32; MAX_QUANTUM * count];
    Box::new(move |stream| {
        let Some(mut buffer) = stream.dequeue_buffer() else { return };
        let requested = buffer.requested() as usize;
        let Some(data) = buffer.datas_mut().first_mut() else { return };
        let Some(bytes) = data.data() else { return };
        let room = (bytes.len() / stride).min(MAX_QUANTUM);
        let frames = if requested == 0 { room } else { requested.min(room) };
        let samples = &mut samples[..frames * count];
        jitter.lock().unwrap_or_else(|e| e.into_inner()).pull(samples);
        for (b, s) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(samples.iter()) {
            *b = s.to_le_bytes();
        }
        let chunk = data.chunk_mut();
        *chunk.offset_mut() = 0;
        *chunk.stride_mut() = stride as i32;
        *chunk.size_mut() = (frames * stride) as u32;
    })
}

/// The host's speaker: a sink (`media.class` Audio/Sink) on the desktop's PipeWire. What
/// programs play to it comes out as stereo Opus packets.
///
/// Silence is not sent. While nothing plays to the sink PipeWire doesn't run it at all, and when
/// what plays is digital silence (a paused player that keeps its stream open), the packets stop
/// after 100 ms of it and start again with the first frame that has sound in it.
///
/// For the receiver a pause looks like this: the packets stop, and the next one, whenever it
/// comes, has the next `seq`, with a `capture_us` that is later by the length of the pause. So
/// a hole in `seq` is always loss, to be concealed; packets that just stop are a pause: play
/// silence (the last packets before a pause decode to silence, so nothing is cut off) and
/// start again as at the beginning, filling the jitter buffer first. [`JitterBuffer`] does
/// this.
pub struct SpeakerSink {
    node: Node,
}

impl SpeakerSink {
    /// Create the sink. `name` is its `node.name`, `description` what the desktop's sound
    /// settings show. `clock` is read for each packet's `capture_us` (microseconds); `on_packet`
    /// gets every packet, on the node's thread: it should hand the packet on and return.
    ///
    /// `capture_us` is when the frame's first sample was due to be heard: the time of the graph
    /// cycle that carried it plus its place in the cycle. A cycle's samples arrive together at
    /// its start, so a packet can be handed over before the time of its last sample.
    pub fn open(name: &str, description: &str, clock: impl FnMut() -> u64 + Send + 'static, on_packet: impl FnMut(Packet) + Send + 'static) -> Result<Self> {
        let spec = Spec {
            name: name.into(),
            description: Some(description.into()),
            class: Some("Audio/Sink"),
            direction: spa::utils::Direction::Input,
            channels: Channels::Stereo,
            target: None,
            realtime: false,
        };
        let framer = Framer::new(Channels::Stereo, Box::new(on_packet))?;
        Ok(Self { node: start(spec, capture(Channels::Stereo, Stamp::Ahead, Box::new(clock), framer))? })
    }

    pub fn node(&self) -> &Node {
        &self.node
    }
}

/// The host's microphone: a source (`media.class` Audio/Source) that programs on the desktop can
/// record from, playing the mono Opus packets pushed into it.
pub struct MicSource {
    node: Node,
    jitter: Arc<Mutex<JitterBuffer>>,
}

impl MicSource {
    /// Create the source. `name` is its `node.name`, `description` what the desktop's sound
    /// settings show.
    pub fn open(name: &str, description: &str) -> Result<Self> {
        let spec = Spec {
            name: name.into(),
            description: Some(description.into()),
            class: Some("Audio/Source"),
            direction: spa::utils::Direction::Output,
            channels: Channels::Mono,
            target: None,
            realtime: true,
        };
        let jitter = Arc::new(Mutex::new(JitterBuffer::new(Channels::Mono, JITTER_FRAMES)?));
        Ok(Self { node: start(spec, playback(Channels::Mono, jitter.clone()))?, jitter })
    }

    /// Take a packet from the client's microphone, in whatever order the network delivered it.
    /// It is played after about 30 ms (see [`JitterBuffer`]). While no program records from
    /// the source, only the newest packets are kept.
    pub fn push(&self, packet: Packet) {
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).push(packet);
    }

    /// What became of the packets pushed so far.
    pub fn stats(&self) -> JitterStats {
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).stats()
    }

    pub fn node(&self) -> &Node {
        &self.node
    }
}

/// A viewer's loudspeaker: plays the host's stereo Opus packets through PipeWire.
pub struct Player {
    node: Node,
    jitter: Arc<Mutex<JitterBuffer>>,
}

impl Player {
    /// `name` is the stream's `node.name`. With no `target` it plays on the default output and
    /// follows it. With one (a `node.name`) it plays on that node only: if the node isn't
    /// there or goes away, the stream ends ([`Node::error`]) instead of moving elsewhere.
    pub fn open(name: &str, target: Option<&str>) -> Result<Self> {
        let spec = Spec {
            name: name.into(),
            description: None,
            class: None,
            direction: spa::utils::Direction::Output,
            channels: Channels::Stereo,
            target: target.map(Into::into),
            realtime: true,
        };
        let jitter = Arc::new(Mutex::new(JitterBuffer::new(Channels::Stereo, JITTER_FRAMES)?));
        Ok(Self { node: start(spec, playback(Channels::Stereo, jitter.clone()))?, jitter })
    }

    /// Take a packet from the host, in whatever order the network delivered it. It is played
    /// after about 30 ms (see [`JitterBuffer`]).
    pub fn push(&self, packet: Packet) {
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).push(packet);
    }

    /// What became of the packets pushed so far.
    pub fn stats(&self) -> JitterStats {
        self.jitter.lock().unwrap_or_else(|e| e.into_inner()).stats()
    }

    pub fn node(&self) -> &Node {
        &self.node
    }
}

/// A viewer's microphone: records through PipeWire and hands over mono Opus packets, leaving
/// out silence the way [`SpeakerSink`] does (a muted microphone is digital silence; a live one
/// in a quiet room is not).
pub struct MicCapture {
    node: Node,
}

impl MicCapture {
    /// `name` is the stream's `node.name`. With no `target` it records the default input and
    /// follows it. With one (a `node.name`) it records that node only: if the node isn't there
    /// or goes away, the stream ends ([`Node::error`]) instead of moving elsewhere.
    ///
    /// `clock` is read for each packet's `capture_us` (microseconds), which is when the frame's
    /// first sample was recorded. `on_packet` gets every packet, on the node's thread.
    pub fn open(name: &str, target: Option<&str>, clock: impl FnMut() -> u64 + Send + 'static, on_packet: impl FnMut(Packet) + Send + 'static) -> Result<Self> {
        let spec = Spec {
            name: name.into(),
            description: None,
            class: None,
            direction: spa::utils::Direction::Input,
            channels: Channels::Mono,
            target: target.map(Into::into),
            realtime: false,
        };
        let framer = Framer::new(Channels::Mono, Box::new(on_packet))?;
        Ok(Self { node: start(spec, capture(Channels::Mono, Stamp::Behind, Box::new(clock), framer))? })
    }

    pub fn node(&self) -> &Node {
        &self.node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host and a viewer keep these in state shared between threads.
    #[test]
    fn nodes_can_be_shared_between_threads() {
        fn shared<T: Send + Sync>() {}
        shared::<SpeakerSink>();
        shared::<MicSource>();
        shared::<Player>();
        shared::<MicCapture>();
        shared::<crate::DefaultSink>();
    }
}
