//! The remoteman core, shared by every client and host.
//!
//! This file is the client role. It owns the connection: pairing, the hello, putting video back
//! together, asking for keyframes after a loss, and the clock exchange. The app around it
//! decodes and shows the frames it is handed, and passes input in. The host role is in
//! [`host`], and both as a C interface in [`ffi`].

pub mod blob;
pub mod ffi;
pub mod host;
#[cfg(unix)]
pub mod receive;
pub mod relay;

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
pub use remoteman_proto::control::{Chroma, Codec, Display, Mode, Placement};
pub use remoteman_proto::input::{Button, Event, TouchPhase};
pub use remoteman_proto::keys;
pub use remoteman_proto::{PORT, PROTO};
use remoteman_proto::video::{CursorAt, KIND_CURSOR, KIND_VIDEO, Reassembler};
pub use blob::{BlobEvent, BlobSender};
pub use remoteman_proto::control::Message;
pub use remoteman_proto::media::{self, Packet};
pub use remoteman_proto::video::Frame;
use remoteman_transport::{Connection, Endpoint, Expect, Identity, connect, read_message, write_message};

pub struct Options {
    /// `host:port`.
    pub host: String,
    /// Where this device's identity and its known hosts are kept.
    pub state: PathBuf,
    /// This device's name, shown on the host.
    pub name: String,
    /// Accept whichever host answers and remember it, if this host isn't known yet.
    pub pair: bool,
    pub display: Display,
    /// What this device decodes in hardware, most preferred first.
    pub decoders: Vec<Mode>,
    /// For tests: throw away one video datagram in this many, as a lossy network would.
    pub test_drop_one_in: Option<u32>,
    /// For tests: hold each loss report back this many milliseconds, as a slow path back to
    /// the host would.
    pub test_delay_lost_ms: Option<u32>,
    /// This app draws the cursor itself: the host leaves it out of the picture and reports its
    /// shape and position through [`Handler::cursor_shape`] and [`Handler::cursor_at`].
    pub cursor: bool,
    /// The password of the host's user, to unlock a locked host with: given on a second
    /// try, after the first failed with "the host refused: locked: ...".
    pub unlock: Option<String>,
    /// Only into a session that is already running on the host: never its login screen.
    pub attach_only: bool,
}

/// The host's cursor image: premultiplied RGBA, rows top to bottom. Zero size: no cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorShape {
    pub width: u32,
    pub height: u32,
    pub hot_x: i32,
    pub hot_y: i32,
    pub rgba: Vec<u8>,
}

/// What the video datagrams carry, from the host's `stream` message.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub epoch: u16,
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// `bt709` or `bt2020-pq`.
    pub colour: String,
}

/// The app's side. Calls come from the core's own threads, one at a time.
pub trait Handler: Send + 'static {
    /// The stream is (re)starting with this format: set the decoder up afresh.
    fn stream(&mut self, info: &StreamInfo);
    /// One whole frame, in order. After a loss, frames are held back until a keyframe, so every
    /// frame handed over is decodable. `age_us` is how long ago the host captured it, once the
    /// clocks have been compared.
    fn frame(&mut self, frame: &Frame, age_us: Option<i64>);
    /// The host's cursor changed shape (only with [`Options::cursor`]).
    fn cursor_shape(&mut self, _shape: &CursorShape) {}
    /// Where the host's cursor is, as fractions of the display; `visible` false when it has
    /// left this display. The host reports every move, the app's own included, so an app that
    /// draws the cursor at its own pointer uses this only when it hasn't moved it itself.
    fn cursor_at(&mut self, _x: f32, _y: f32, _visible: bool) {}
    /// One of the feature messages from the host (`sound_format`, `camera_keyframe`, `status`,
    /// `file_result`, ...): see docs/PROTOCOL.md, "Features".
    fn message(&mut self, _message: &Message) {}
    /// One packet of the desktop's sound, as it arrives (after [`Client::send_message`] with
    /// `Message::Sound { on: true }`). Gaps in `seq` are packets that were lost; the app
    /// conceals them and keeps a small playout buffer.
    fn sound(&mut self, _packet: &Packet) {}
    /// A blob from the host (what was copied there, a file): opened, its content, closed.
    fn blob(&mut self, _event: &BlobEvent) {}
    /// How far connecting has got. Called on the connecting thread, before
    /// [`Client::connect`] returns; after [`Stage::Display`] come `stream` and the first `frame`.
    fn stage(&mut self, _stage: Stage) {}
    /// This device is new to the host, whose owner now has `seconds` to confirm it there.
    /// `code` is what both sides show for comparing. Called on the connecting thread, in the
    /// middle of [`Client::connect`], which goes on waiting.
    fn pairing(&mut self, _code: &str, _seconds: u32) {}
    /// The connection is over.
    fn closed(&mut self, reason: &str);
}

/// The steps of connecting, in order, for a connecting screen. Each is reported as it begins;
/// if connecting fails, the last one reported is the one that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    /// Looking the host's name up.
    Lookup = 0,
    /// Reaching the host and checking each other's identity (this is where pairing happens).
    Handshake = 1,
    /// Saying hello: names, protocol, what the host can do.
    Hello = 2,
    /// The host is making this device's display and starting the picture.
    Display = 3,
}

fn known_host(state: &std::path::Path, host: &str) -> PathBuf {
    state.join("client/hosts").join(host.replace([':', '/'], "_"))
}

/// Forget a host this device paired with ("address:port", as it was given to
/// [`Client::connect`]): its key is thrown away, so connecting again needs pairing again.
/// Says whether it was known. The host still trusts this device until it is revoked there.
pub fn forget_host(state: &std::path::Path, host: &str) -> Result<bool> {
    match std::fs::remove_file(known_host(state, host)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("forget {host}")),
    }
}

/// Whether this device has paired with `host` ("address:port").
pub fn knows_host(state: &std::path::Path, host: &str) -> bool {
    known_host(state, host).exists()
}

enum FromHost {
    Stream(StreamInfo),
    Cursor(CursorShape),
    Message(Message),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Frames that never arrived whole.
    pub lost: u32,
    /// Times the host was told of a loss (or asked for a keyframe).
    pub keyframe_requests: u32,
    /// The shortest round trip seen, in microseconds (0: none yet).
    pub round_trip_us: u32,
}

#[derive(Default)]
struct Counters {
    lost: AtomicU32,
    requests: AtomicU32,
    rtt: AtomicU32,
}

/// Sends microphone packets on a client's connection, from any thread: as
/// [`Client::send_mic`]. Sending after the client has closed does nothing.
#[derive(Clone)]
pub struct MicSender(Connection);

impl MicSender {
    pub fn send(&self, start: bool, seq: u32, time_us: u64, data: &[u8]) {
        let packet = Packet { kind: media::KIND_MIC, start, seq, time_us, data: data.to_vec() };
        let _ = self.0.send_datagram(packet.write().into());
    }
}

/// Sends camera frames on a client's connection, from any thread: as [`Client::send_camera`].
/// Sending after the client has closed does nothing.
#[derive(Clone)]
pub struct CameraSender(Connection, Arc<std::sync::Mutex<(u16, u32)>>);

impl CameraSender {
    pub fn send(&self, data: &[u8], key: bool, time_us: u64) {
        send_camera(&self.0, &self.1, data, key, time_us);
    }
}

fn send_camera(conn: &Connection, camera: &std::sync::Mutex<(u16, u32)>, data: &[u8], key: bool, time_us: u64) {
    let Some(max) = conn.max_datagram_size() else { return };
    let (epoch, number) = {
        let mut camera = camera.lock().unwrap();
        let now = *camera;
        camera.1 = camera.1.wrapping_add(1);
        now
    };
    for d in remoteman_proto::video::fragment_as(media::KIND_CAMERA, data, key, false, epoch, number, time_us, max) {
        let _ = conn.send_datagram(d.into());
    }
}

/// Why a host closed the connection: the codes of docs/PROTOCOL.md, "Connection".
pub const CLOSE_DROPPED: u32 = 2;
pub const CLOSE_REVOKED: u32 = 3;

pub struct Client {
    runtime: Option<tokio::runtime::Runtime>,
    /// For starting work from any thread, the core's own callbacks included.
    tasks: tokio::runtime::Handle,
    control: tokio::sync::mpsc::Sender<Message>,
    /// The camera: how many times it was switched on, and the next frame's number.
    camera: Arc<std::sync::Mutex<(u16, u32)>>,
    conn: Connection,
    _endpoint: Endpoint,
    input: tokio::sync::mpsc::UnboundedSender<Event>,
    counters: Arc<Counters>,
    /// The host's name and operating system, from its hello.
    pub host_name: String,
    pub host_os: String,
    pub host_features: Vec<String>,
    /// The host's certificate fingerprint.
    pub host_fingerprint: String,
}

impl Client {
    /// Connect and say hello. Returns once the host has answered; `handler` then gets the stream.
    pub fn connect(options: Options, handler: impl Handler) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("remoteman").enable_all().build()?;
        let mut this = runtime.block_on(Self::start(options, Box::new(handler)))?;
        this.runtime = Some(runtime);
        Ok(this)
    }

    async fn start(o: Options, mut handler: Box<dyn Handler>) -> Result<Self> {
        handler.stage(Stage::Lookup);
        let addr = o.host.to_socket_addrs().with_context(|| format!("look up {}", o.host))?.next().context("no such host")?;
        let identity = Identity::load_or_create(&o.state.join("client"))?;
        let known = known_host(&o.state, &o.host);
        let expect = match std::fs::read_to_string(&known) {
            Ok(fp) => Expect::Fingerprint(fp.trim().to_owned()),
            Err(_) if o.pair => Expect::Anyone,
            Err(_) => bail!("{} isn't paired yet", o.host),
        };
        let o_drop = o.test_drop_one_in.filter(|n| *n > 1);
        let o_delay = o.test_delay_lost_ms.filter(|ms| *ms > 0);
        handler.stage(Stage::Handshake);
        let (endpoint, conn, fingerprint) = connect(addr, &identity, expect).await?;
        let new_here = !known.exists();
        if new_here {
            std::fs::create_dir_all(known.parent().unwrap())?;
            std::fs::write(&known, &fingerprint)?;
        }
        // A host that turns a new device down is not kept: the next try starts from nothing.
        let forget = |e: anyhow::Error| {
            if new_here {
                let _ = std::fs::remove_file(&known);
            }
            e
        };

        let clock = Instant::now();
        let now_us = move || clock.elapsed().as_micros() as u64;
        handler.stage(Stage::Hello);
        let (mut send, mut recv) = conn.open_bi().await?;
        write_message(&mut send, &Message::ClientHello { proto: remoteman_proto::PROTO, name: o.name, display: o.display, decoders: o.decoders, cursor: o.cursor, unlock: o.unlock, way_in: o.attach_only.then(|| "attach".to_owned()) }).await?;
        let (host_name, host_os, host_features) = loop {
            match read_message(&mut recv).await?.context("the host closed the connection")? {
                Message::HostHello { name, os, features, .. } => break (name, os, features),
                Message::Error { code, message } => return Err(forget(anyhow::anyhow!("the host refused: {code}: {message}"))),
                // New here, and the host's owner is being asked: show the code and wait.
                Message::Pairing { seconds } => handler.pairing(&remoteman_transport::pair_code(&fingerprint, &identity.fingerprint), seconds),
                _ => {}
            }
        };
        handler.stage(Stage::Display);

        let counters = Arc::new(Counters::default());
        let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::channel::<Message>(16);
        let control = ctl_tx.clone();
        let (epoch_tx, mut epoch_rx) = tokio::sync::mpsc::unbounded_channel::<FromHost>();
        let (offset_tx, offset_rx) = tokio::sync::watch::channel(None::<i64>);

        // Control out: pings and keyframe requests.
        tokio::spawn(async move {
            while let Some(m) = ctl_rx.recv().await {
                if write_message(&mut send, &m).await.is_err() {
                    break;
                }
            }
        });
        let pings = ctl_tx.clone();
        tokio::spawn(async move {
            for id in 0.. {
                if pings.send(Message::Ping { id, client_us: now_us() }).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(if id < 10 { 50 } else { 1000 })).await;
            }
        });
        // Control in: stream descriptions, clock answers, errors.
        let c = counters.clone();
        let (closed_tx, mut closed_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let failed = closed_tx.clone();
        tokio::spawn(async move {
            let mut best = u64::MAX;
            while let Ok(Some(m)) = read_message(&mut recv).await {
                match m {
                    Message::Stream { epoch, mode, width, height, refresh, colour } => {
                        let _ = epoch_tx.send(FromHost::Stream(StreamInfo { epoch, mode, width, height, refresh, colour }));
                    }
                    Message::Cursor { width, height, hot_x, hot_y, rgba } => {
                        let rgba = remoteman_proto::control::decode_bytes(&rgba).unwrap_or_default();
                        // An image that isn't the size it claims is shown as no cursor.
                        let ok = rgba.len() == width as usize * height as usize * 4;
                        let shape = if ok { CursorShape { width, height, hot_x, hot_y, rgba } } else { CursorShape { width: 0, height: 0, hot_x: 0, hot_y: 0, rgba: Vec::new() } };
                        let _ = epoch_tx.send(FromHost::Cursor(shape));
                    }
                    Message::Pong { client_us, host_us, .. } => {
                        // The answer with the shortest round trip gives the best clock estimate.
                        let back = now_us();
                        let rtt = back.saturating_sub(client_us);
                        if rtt < best {
                            best = rtt;
                            c.rtt.store(rtt as u32, Ordering::Relaxed);
                            let _ = offset_tx.send(Some(host_us as i64 - ((client_us + back) / 2) as i64));
                        }
                    }
                    Message::Error { code, message } => {
                        let _ = failed.send(format!("{code}: {message}"));
                    }
                    m @ (Message::SoundFormat { .. } | Message::CameraKeyframe | Message::Status { .. } | Message::FileResult { .. } | Message::Features { .. }) => {
                        let _ = epoch_tx.send(FromHost::Message(m));
                    }
                    _ => {}
                }
            }
        });
        // Blobs in: each on a stream the host opens. The channel is short, so a slow app holds
        // the sender back rather than filling memory.
        let (blob_tx, mut blob_rx) = tokio::sync::mpsc::channel::<BlobEvent>(16);
        let blob_conn = conn.clone();
        tokio::spawn(async move {
            let mut id = 0u32;
            while let Ok(mut stream) = blob_conn.accept_uni().await {
                id = id.wrapping_add(1);
                let (tx, id) = (blob_tx.clone(), id);
                tokio::spawn(async move {
                    let mut kind = [0u8; 1];
                    if stream.read_exact(&mut kind).await.is_err() || kind[0] < media::BLOB_FIRST {
                        return;
                    }
                    blob::receive(stream, kind[0], id, move |event| {
                        let tx = tx.clone();
                        async move { tx.send(event).await.is_ok() }
                    })
                    .await;
                });
            }
        });
        // Input out.
        let (input, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let input_conn = conn.clone();
        tokio::spawn(async move {
            let Ok(mut stream) = input_conn.open_uni().await else { return };
            let mut buf = Vec::new();
            while let Some(e) = input_rx.recv().await {
                buf.clear();
                e.write(&mut buf);
                // Take whatever else is waiting, so a burst goes out as one write.
                while let Ok(more) = input_rx.try_recv() {
                    more.write(&mut buf);
                }
                if stream.write_all(&buf).await.is_err() {
                    break;
                }
            }
        });
        // Video in.
        let video = conn.clone();
        let c = counters.clone();
        tokio::spawn(async move {
            let mut r = Reassembler::new();
            // What the next frame handed on must be: a keyframe at the start, and after a loss a
            // keyframe or a recovery point. `Some(first)`: frames from `first` on were missed.
            let (mut started, mut need_key, mut missed) = (false, true, None::<u32>);
            let mut asked = None::<Instant>;
            let mut dropped = 0u32;
            // Datagrams that came ahead of the `stream` message they belong to (the two travel
            // apart and may arrive in either order): kept until it has come. Without this the
            // first frames of a picture, its keyframe among them, are thrown away, and on a
            // desktop where nothing moves no other frame follows.
            let mut early = std::collections::VecDeque::new();
            let mut replay = std::collections::VecDeque::new();
            let reason = loop {
                // Kept datagrams go first, in the order they came.
                let d = if let Some(d) = replay.pop_front() {
                    d
                } else {
                tokio::select! {
                    biased;
                    Some(reason) = closed_rx.recv() => break reason,
                    Some(m) = epoch_rx.recv() => match m {
                        FromHost::Stream(info) => {
                            r.reset(info.epoch);
                            (started, need_key, missed) = (true, true, None);
                            handler.stream(&info);
                            replay.extend(early.drain(..));
                            continue;
                        }
                        FromHost::Cursor(shape) => {
                            handler.cursor_shape(&shape);
                            continue;
                        }
                        FromHost::Message(m) => {
                            handler.message(&m);
                            continue;
                        }
                    },
                    Some(event) = blob_rx.recv() => {
                        handler.blob(&event);
                        continue;
                    }
                    d = video.read_datagram() => match d {
                        Ok(d) => d,
                        Err(e) => break e.to_string(),
                    },
                }
                };
                {
                    {
                        // A picture's datagram for a stream this side hasn't heard of yet (none
                        // at all, or a newer one than the one it knows): kept for when it has.
                        let ahead = d.first() == Some(&KIND_VIDEO) && (!started || d.get(2..4).is_some_and(|e| { let e = u16::from_le_bytes([e[0], e[1]]); e != r.epoch() && e.wrapping_sub(r.epoch()) < 0x8000 }));
                        if ahead {
                            // (A few megabytes at most: older ones give way.)
                            if early.len() >= 4096 {
                                early.pop_front();
                            }
                            early.push_back(d);
                            continue;
                        }
                        if !started {
                            continue;
                        }
                        match d.first() {
                            Some(&KIND_VIDEO) => {}
                            Some(&KIND_CURSOR) => {
                                if let Ok(c) = CursorAt::parse(&d) {
                                    handler.cursor_at(c.x, c.y, c.visible);
                                }
                                continue;
                            }
                            Some(&media::KIND_SOUND) => {
                                if let Ok(p) = Packet::parse(&d) {
                                    handler.sound(&p);
                                }
                                continue;
                            }
                            // Something a later version sends: not for us.
                            _ => continue,
                        }
                        if let Some(n) = o_drop {
                            dropped += 1;
                            if dropped % n == 0 {
                                continue;
                            }
                        }
                        let frame = match r.push(&d) {
                            Ok(Some(f)) => f,
                            Ok(None) => continue,
                            Err(e) => break format!("bad video datagram: {e}"),
                        };
                        if let Some((first, last)) = r.take_lost() {
                            c.lost.fetch_add(last - first + 1, Ordering::Relaxed);
                            // Keep the earliest loss not yet recovered from.
                            missed = Some(missed.map_or(first, |m: u32| m.min(first)));
                            asked = None;
                        }
                        let usable = frame.key || (frame.recovery && !need_key);
                        if (need_key || missed.is_some()) && !usable {
                            // Later frames lean on what was missed. Tell the host (again, if it
                            // has had time to answer and hasn't).
                            if asked.is_none_or(|t| t.elapsed() > Duration::from_millis(40)) {
                                let m = match missed {
                                    Some(first) if !need_key => Message::Lost { first, last: frame.number, epoch: Some(r.epoch()) },
                                    _ => Message::Keyframe,
                                };
                                let sent = match o_delay {
                                    Some(ms) => {
                                        let late = ctl_tx.clone();
                                        tokio::spawn(async move {
                                            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
                                            let _ = late.send(m).await;
                                        });
                                        true
                                    }
                                    None => ctl_tx.try_send(m).is_ok(),
                                };
                                if sent {
                                    c.requests.fetch_add(1, Ordering::Relaxed);
                                    asked = Some(Instant::now());
                                }
                            }
                            continue;
                        }
                        (need_key, missed) = (false, None);
                        let age = (*offset_rx.borrow()).map(|offset| now_us() as i64 + offset - frame.capture_us as i64);
                        handler.frame(&frame, age);
                    }
                }
            };
            handler.closed(&reason);
        });

        Ok(Self {
            runtime: None,
            tasks: tokio::runtime::Handle::current(),
            control,
            camera: Arc::new(std::sync::Mutex::new((0, 0))),
            conn,
            _endpoint: endpoint,
            input,
            counters,
            host_name,
            host_os,
            host_features,
            host_fingerprint: fingerprint,
        })
    }

    /// Send one of the feature messages (`sound`, `mic`, `camera`, `configure`, `clipboard`,
    /// `file_result`): see docs/PROTOCOL.md, "Features". Never blocks.
    pub fn send_message(&self, message: Message) {
        if let Message::Camera { on: true, .. } = &message {
            // A camera switched on starts its frames from 0 in a new epoch.
            let mut camera = self.camera.lock().unwrap();
            *camera = (camera.0.wrapping_add(1), 0);
        }
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(message)) = self.control.try_send(message) {
            let control = self.control.clone();
            self.tasks.spawn(async move {
                let _ = control.send(message).await;
            });
        }
    }

    /// For sending this device's microphone from the thread that records it.
    pub fn mic_sender(&self) -> MicSender {
        MicSender(self.conn.clone())
    }

    /// Send one packet of this device's microphone (after `Message::Mic { on: true, .. }`).
    /// `seq` counts up by one per packet; `start` marks the first after a pause. Never blocks:
    /// a packet the network can't take is dropped.
    pub fn send_mic(&self, start: bool, seq: u32, time_us: u64, data: &[u8]) {
        let packet = Packet { kind: media::KIND_MIC, start, seq, time_us, data: data.to_vec() };
        let _ = self.conn.send_datagram(packet.write().into());
    }

    /// Send one whole frame of this device's camera (after `Message::Camera { on: true, .. }`):
    /// H.264 or HEVC in Annex-B form, parameter sets in front of every keyframe, no B-frames.
    /// Never blocks: fragments the network can't take are dropped, and the host asks for a
    /// keyframe ([`Handler::message`] with `Message::CameraKeyframe`).
    pub fn send_camera(&self, data: &[u8], key: bool, time_us: u64) {
        send_camera(&self.conn, &self.camera, data, key, time_us);
    }

    /// For sending this device's camera from the thread that encodes it.
    pub fn camera_sender(&self) -> CameraSender {
        CameraSender(self.conn.clone(), self.camera.clone())
    }

    /// Start sending a blob (`media::BLOB_CLIPBOARD` or `media::BLOB_FILE`) with this JSON
    /// header; write its content to what comes back.
    pub fn open_blob(&self, kind: u8, header: String) -> BlobSender {
        let (sender, rx) = BlobSender::pair();
        self.tasks.spawn(blob::send(self.conn.clone(), kind, header, rx));
        sender
    }

    /// Send one input event. Never blocks.
    pub fn send(&self, event: Event) {
        let _ = self.input.send(event);
    }

    pub fn stats(&self) -> Stats {
        Stats {
            lost: self.counters.lost.load(Ordering::Relaxed),
            keyframe_requests: self.counters.requests.load(Ordering::Relaxed),
            round_trip_us: self.counters.rtt.load(Ordering::Relaxed),
        }
    }

    /// Leave. The handler's `closed` is called before this returns or shortly after.
    /// The code the host closed the connection with (0 it stopped or another device took over,
    /// 1 something failed, [`CLOSE_DROPPED`], [`CLOSE_REVOKED`]). `None` while the connection
    /// is up, and when it ended some other way (the network, this side leaving).
    pub fn close_code(&self) -> Option<u32> {
        remoteman_transport::closed_with(&self.conn)
    }

    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.conn.close(0u32.into(), b"done");
        if let Some(rt) = self.runtime.take() {
            // Let the goodbye reach the host before the runtime goes; otherwise the host only
            // notices after its idle timeout, and the virtual monitor lingers until then.
            let endpoint = self._endpoint.clone();
            let _ = rt.block_on(async { tokio::time::timeout(Duration::from_millis(500), endpoint.wait_idle()).await });
            rt.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}
