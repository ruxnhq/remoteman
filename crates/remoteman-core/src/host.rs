//! The host role of the remoteman core. It owns the listening port and the connection: trust
//! and pairing, the hello, cutting video into datagrams, the cursor channel, the clock, and
//! reading input. The host program around it (the Linux agent, the Mac host) makes the display,
//! captures and encodes, and puts input into its desktop.
//!
//! One client at a time: a new one takes the place of the old.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use remoteman_proto::control::{Message, encode_bytes};
use remoteman_proto::input::Event;
use remoteman_proto::media::{self, Packet};
use remoteman_proto::video::{CursorAt, Reassembler, fragment};
use remoteman_transport::{Connection, Identity, RecvStream, Trust, listen, peer_fingerprint, read_message, write_message};

use crate::blob::{self, BlobEvent, BlobSender, Chunk};
use crate::{CursorShape, Display, Mode};

pub struct HostOptions {
    /// Where to listen, e.g. `0.0.0.0:7791`.
    pub listen: SocketAddr,
    /// Where this host's identity and its list of trusted devices are kept.
    pub state: PathBuf,
    /// This computer's name, shown on clients.
    pub name: String,
    /// `linux`, `macos`, ...
    pub os: String,
    /// What this host can do, for clients to offer: `video`, `input`, `cursor`, ...
    pub features: Vec<String>,
    /// Let the next unknown device in, and remember it.
    pub pair: bool,
    /// For tests: throw away one video datagram in this many before sending, as a lossy
    /// network would.
    pub test_drop_one_in: Option<u32>,
}

/// A client that has said hello.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientInfo {
    pub name: String,
    /// The fingerprint of its certificate.
    pub fingerprint: String,
    pub display: Display,
    /// What it decodes in hardware, most preferred first.
    pub decoders: Vec<Mode>,
    /// It draws the cursor itself: leave the cursor out of the picture and report it through
    /// [`Link::cursor_shape`] and [`Link::cursor_at`].
    pub cursor: bool,
    /// The password it sent to unlock a locked host with, if it sent one. Never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlock: Option<String>,
    /// It asked to be let in only into a session that is already running.
    #[serde(default)]
    pub attach_only: bool,
}

/// What the host will send, in answer to a client's hello.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Accept {
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// `bt709` or `bt2020-pq`.
    pub colour: String,
    /// With `bt2020-pq`: where SDR white sits in the signal and the brightest this display
    /// may go, in nits (0: left out, which a client reads as 203 and 1000).
    pub sdr_white: u32,
    pub peak: u32,
}

/// The host program's side. All calls come from one thread, one at a time, and may block.
pub trait HostHandler: Send + 'static {
    /// A trusted client said hello. Make its display and start the picture, sending frames
    /// through `link`; say what they are, or refuse with a reason the client is shown.
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String>;
    /// The client lost a frame: make the next one a keyframe.
    fn keyframe(&mut self);
    /// The client missed frames from `first` on (counting the frames sent to it from 0). A host
    /// whose encoder can drop its references to them answers with a frame that leans on none
    /// of them, sent with [`Link::send_recovery_frame`]; the others send a keyframe, which is
    /// what this does unless overridden.
    fn lost(&mut self, first: u32, last: u32) {
        let _ = (first, last);
        self.keyframe();
    }
    /// The client's input, in order.
    fn input(&mut self, event: Event);
    /// One of the feature messages from the client (`sound`, `mic`, `camera`, `configure`,
    /// `clipboard`, `file_result`): see docs/PROTOCOL.md, "Features". A host that lists a
    /// feature in its hello acts on that feature's messages; the rest it leaves alone.
    fn message(&mut self, _message: &Message) {}
    /// One packet of the client's microphone, as it arrives (after `Message::Mic { on: true }`).
    fn mic(&mut self, _packet: &Packet) {}
    /// One whole frame of the client's camera, in order (after `Message::Camera { on: true }`).
    /// After a loss the core asks the client for a keyframe and holds frames back until it
    /// comes, so every frame handed over is decodable.
    fn camera(&mut self, _frame: &remoteman_proto::video::Frame) {}
    /// A blob from the client (what was copied on it, a file): opened, its content, closed.
    /// Blobs still open when the client goes are over, with no `Close`.
    fn blob(&mut self, _event: &BlobEvent) {}
    /// The client is gone (or was replaced): stop the picture and take the display away.
    fn left(&mut self);
}

/// One encoded frame on its way to the client.
pub struct Frame {
    pub data: Vec<u8>,
    pub key: bool,
    pub recovery: bool,
    pub capture_us: u64,
}

/// What a [`Link`] carries besides the cursor.
pub enum Out {
    Frame(Frame),
    /// The picture starts afresh as this (see [`Link::restart`]).
    Restart(Accept),
    /// The same, of the session's second display (see [`Link::second`]).
    Second(Box<Out>),
}

/// What a [`Link`] carries that isn't the picture.
pub enum CursorMsg {
    Shape(CursorShape),
    At { x: f32, y: f32, visible: bool },
    /// A feature message for the client.
    Message(Message),
    /// One packet of the desktop's sound.
    Sound(Packet),
    /// A blob for the client: its kind, its header, and where its content comes from.
    Blob { kind: u8, header: String, content: tokio::sync::mpsc::Receiver<Chunk> },
}

/// The far end of a [`Link`] made with [`Link::pipe`]: what the host program sent, to be
/// carried on to wherever the client really is. Each half is read from a thread of its own.
pub struct LinkEnd {
    pub frames: tokio::sync::mpsc::Receiver<Out>,
    pub cursor: tokio::sync::mpsc::UnboundedReceiver<CursorMsg>,
}

/// The way to one connected client. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Link {
    frames: tokio::sync::mpsc::Sender<Out>,
    cursor: tokio::sync::mpsc::UnboundedSender<CursorMsg>,
    clock: Instant,
    /// What is sent through this copy is the second display's picture.
    second: bool,
}

impl Link {
    /// The way to the same client for the picture of a second display in its session: frames
    /// sent through it are that display's, with their own count and their own fresh starts.
    /// Call [`Self::restart`] on it first, to say what the picture is. Its cursor is drawn
    /// into the picture (the cursor calls do nothing here).
    pub fn second(&self) -> Link {
        Link { second: true, ..self.clone() }
    }

    fn out(&self, out: Out) -> Out {
        if self.second { Out::Second(Box::new(out)) } else { out }
    }

    /// A link that leads to `LinkEnd` and not to a client: for a host program that reaches its
    /// client through another process (the Linux agent behind the system daemon). `elapsed_us`
    /// is what the client's session clock reads now, so both processes stamp alike.
    pub fn pipe(elapsed_us: u64) -> (Link, LinkEnd) {
        let (frames, frame_rx) = tokio::sync::mpsc::channel::<Out>(4);
        let (cursor, cursor_rx) = tokio::sync::mpsc::unbounded_channel::<CursorMsg>();
        let now = Instant::now();
        let clock = now.checked_sub(Duration::from_micros(elapsed_us)).unwrap_or(now);
        (Link { frames, cursor, clock, second: false }, LinkEnd { frames: frame_rx, cursor: cursor_rx })
    }

    /// Start the picture afresh as something else: another size or codec, or another desktop
    /// altogether (the login screen giving way to the session). The client is told, drops what
    /// it has and waits for a keyframe, so the next frame sent must be one; frames count from
    /// 0 again. Returns false once the client is gone.
    pub fn restart(&self, accept: Accept) -> bool {
        self.frames.blocking_send(self.out(Out::Restart(accept))).is_ok()
    }

    /// This session's clock in microseconds: stamp each frame with it when it is captured.
    pub fn now_us(&self) -> u64 {
        self.clock.elapsed().as_micros() as u64
    }

    /// Send one encoded frame (HEVC Annex-B, or AV1 OBUs). Waits while the network is behind,
    /// so an encoder calling this is held back and frames get skipped before encoding, never
    /// after. Returns false once the client is gone.
    pub fn send_frame(&self, data: &[u8], key: bool, capture_us: u64) -> bool {
        self.frames.blocking_send(self.out(Out::Frame(Frame { data: data.to_vec(), key, recovery: false, capture_us }))).is_ok()
    }

    /// As [`Self::send_frame`], for the first frame after a reported loss that leans on nothing
    /// the client missed: the client picks the picture up again at it.
    pub fn send_recovery_frame(&self, data: &[u8], capture_us: u64) -> bool {
        self.frames.blocking_send(self.out(Out::Frame(Frame { data: data.to_vec(), key: false, recovery: true, capture_us }))).is_ok()
    }

    /// The cursor's image changed (for clients that draw it themselves). Zero size: no cursor.
    pub fn cursor_shape(&self, shape: CursorShape) {
        if self.second {
            return;
        }
        let _ = self.cursor.send(CursorMsg::Shape(shape));
    }

    /// Where the cursor is, as fractions of the display; `visible` false when it isn't on it.
    pub fn cursor_at(&self, x: f32, y: f32, visible: bool) {
        if self.second {
            return;
        }
        let _ = self.cursor.send(CursorMsg::At { x, y, visible });
    }

    /// Send one of the feature messages (`sound_format`, `camera_keyframe`, `status`,
    /// `file_result`): see docs/PROTOCOL.md, "Features". Never blocks.
    pub fn send_message(&self, message: Message) {
        let _ = self.cursor.send(CursorMsg::Message(message));
    }

    /// Send one packet of the desktop's sound (after the client's `Message::Sound { on: true }`
    /// and this host's `Message::SoundFormat`). `seq` counts up by one per packet; `start`
    /// marks the first after a pause. Never blocks: a packet the network can't take is dropped.
    pub fn send_sound(&self, start: bool, seq: u32, time_us: u64, data: &[u8]) {
        let _ = self.cursor.send(CursorMsg::Sound(Packet { kind: media::KIND_SOUND, start, seq, time_us, data: data.to_vec() }));
    }

    /// Start sending a blob (`media::BLOB_CLIPBOARD` or `media::BLOB_FILE`) with this JSON
    /// header; write its content to what comes back.
    pub fn open_blob(&self, kind: u8, header: String) -> BlobSender {
        let (sender, content) = BlobSender::pair();
        let _ = self.cursor.send(CursorMsg::Blob { kind, header, content });
        sender
    }
}

enum Call {
    Connected(ClientInfo, Link, tokio::sync::oneshot::Sender<Result<Accept, String>>),
    Keyframe,
    Lost(u32, u32),
    Input(Event),
    Message(Message),
    Mic(Packet),
    Camera(remoteman_proto::video::Frame),
    /// With what holds the sender back until the handler has dealt with it.
    Blob(BlobEvent, Option<tokio::sync::OwnedSemaphorePermit>),
    Left,
}

/// A device this host trusts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    /// The fingerprint of its certificate: what it is trusted by.
    pub fingerprint: String,
    /// The name it gave in its last hello, or the address it paired from until it has said one.
    pub name: String,
    /// It is the client connected right now.
    pub connected: bool,
}

/// The connection of the client being served, and its fingerprint.
type Who = Arc<Mutex<Option<(String, Connection)>>>;

/// A device that connected while pairing was open and waits for the host's owner.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingDevice {
    pub name: String,
    pub fingerprint: String,
    /// What the device shows too, if it is the device its owner thinks it is.
    pub code: String,
    /// How long the owner has left to answer.
    pub seconds: u32,
}

/// How long the owner has to confirm a new device.
const CONFIRM_SECONDS: u32 = 60;

/// Pairing that waits for its owner's word.
#[derive(Default)]
struct Confirm {
    /// This host's own fingerprint: half of what the code is made from.
    own: String,
    /// A new device is held until the owner says yes ([`Host::decide`]); otherwise it is
    /// trusted as soon as it connects while pairing is open.
    wanted: std::sync::atomic::AtomicBool,
    waiting: Mutex<Option<(PendingDevice, Instant, tokio::sync::oneshot::Sender<bool>)>>,
}

pub struct Host {
    confirm: Arc<Confirm>,
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: remoteman_transport::Endpoint,
    trust: Arc<Trust>,
    link: Arc<Mutex<Option<Link>>>,
    who: Who,
    /// What this host can do, as the next hello will say it.
    features: Arc<Mutex<Vec<String>>>,
    calls: Mutex<mpsc::Sender<Call>>,
    /// Told once the handler thread has ended and let go of everything it held.
    handler_done: Mutex<mpsc::Receiver<()>>,
    /// This host's certificate fingerprint, for pairing by hand.
    pub fingerprint: String,
    /// The address actually listened on.
    pub address: SocketAddr,
}

impl Host {
    /// Start listening. `handler` is called on a thread of its own from here on.
    pub fn start(options: HostOptions, handler: impl HostHandler) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("remoteman").enable_all().build()?;
        let identity = Identity::load_or_create(&options.state.join("host"))?;
        let trust = Arc::new(Trust::load(&options.state.join("host/trusted")));
        trust.set_pairing(options.pair);
        let endpoint = {
            let _in_runtime = runtime.enter();
            listen(options.listen, &identity, trust.clone())?
        };
        let address = endpoint.local_addr()?;

        // The handler lives on its own thread, so its calls come one at a time and may block.
        let (calls, call_rx) = mpsc::channel::<Call>();
        let mut handler: Box<dyn HostHandler> = Box::new(handler);
        let (done, handler_done) = mpsc::channel::<()>();
        std::thread::Builder::new().name("remoteman host".into()).spawn(move || {
            while let Ok(call) = call_rx.recv() {
                match call {
                    Call::Connected(client, link, reply) => {
                        let _ = reply.send(handler.connected(&client, link));
                    }
                    Call::Keyframe => handler.keyframe(),
                    Call::Lost(first, last) => handler.lost(first, last),
                    Call::Input(event) => handler.input(event),
                    Call::Message(message) => handler.message(&message),
                    Call::Mic(packet) => handler.mic(&packet),
                    Call::Camera(frame) => handler.camera(&frame),
                    Call::Blob(event, held) => {
                        handler.blob(&event);
                        drop(held);
                    }
                    Call::Left => handler.left(),
                }
            }
            // Nothing more will come: whatever the handler still holds is let go of here,
            // and whoever is stopping the host is told when that is done.
            drop(handler);
            let _ = done.send(());
        })?;

        let link: Arc<Mutex<Option<Link>>> = Arc::default();
        let who = Who::default();
        let features = Arc::new(Mutex::new(options.features.clone()));
        let confirm = Arc::new(Confirm { own: identity.fingerprint.clone(), ..Confirm::default() });
        runtime.spawn(accept_loop(endpoint.clone(), trust.clone(), options, calls.clone(), link.clone(), who.clone(), features.clone(), confirm.clone()));
        Ok(Self { confirm, runtime: Some(runtime), endpoint, trust, link, who, features, calls: Mutex::new(calls), handler_done: Mutex::new(handler_done), fingerprint: identity.fingerprint, address })
    }

    /// Let the next unknown device in (it is remembered and pairing turns itself off), or stop
    /// doing so.
    pub fn set_pairing(&self, on: bool) {
        self.trust.set_pairing(on);
    }

    pub fn pairing(&self) -> bool {
        self.trust.pairing()
    }

    /// Whether a new device waits for [`Host::decide`] before it is trusted (off to begin
    /// with: it is trusted as soon as it connects while pairing is open).
    pub fn set_confirm(&self, on: bool) {
        self.confirm.wanted.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// The device waiting to be confirmed, if there is one.
    pub fn pending(&self) -> Option<PendingDevice> {
        let waiting = self.confirm.waiting.lock().unwrap();
        let (device, since, _) = waiting.as_ref()?;
        Some(PendingDevice { seconds: CONFIRM_SECONDS.saturating_sub(since.elapsed().as_secs() as u32), ..device.clone() })
    }

    /// Trust the waiting device from now on, or turn it away. `false`: none was waiting.
    pub fn decide(&self, accept: bool) -> bool {
        match self.confirm.waiting.lock().unwrap().take() {
            Some((_, _, answer)) => answer.send(accept).is_ok(),
            None => false,
        }
    }

    /// Change what this host can do while it runs (its owner switched a feature on or off).
    /// The next client's hello says so, and the connected one is told with a `features`
    /// message; nothing is restarted. Messages for a feature that is off are the handler's
    /// to ignore.
    pub fn set_features(&self, features: Vec<String>) {
        *self.features.lock().unwrap() = features.clone();
        if let Some(link) = self.link() {
            link.send_message(Message::Features { features });
        }
    }

    /// Hand the handler a message as if the client had sent it, on the handler's own thread
    /// and in turn with everything else: how the host program itself switches something off
    /// for the connected client (`Message::Clipboard { on: false }` when its owner stops
    /// allowing the clipboard, say).
    pub fn tell_handler(&self, message: Message) {
        let _ = self.calls.lock().unwrap().send(Call::Message(message));
    }

    /// Every device this host trusts, in the order they were paired.
    pub fn devices(&self) -> Vec<Device> {
        let connected = self.who.lock().unwrap().as_ref().map(|(fp, _)| fp.clone());
        self.trust.list().into_iter().map(|(fingerprint, name)| Device { connected: connected.as_deref() == Some(&fingerprint), fingerprint, name }).collect()
    }

    /// Stop trusting a device. It is dropped if it is the one connected, and refused from now
    /// on until it is paired again. Says whether it was trusted.
    pub fn revoke(&self, fingerprint: &str) -> Result<bool> {
        let was = self.trust.remove(fingerprint)?;
        let who = self.who.lock().unwrap();
        if let Some((fp, conn)) = who.as_ref() {
            if fp == fingerprint {
                conn.close(CLOSE_REVOKED.into(), b"this device is no longer trusted here");
            }
        }
        Ok(was)
    }

    /// Drop the connected client and keep listening. It stays trusted, so it may come back.
    /// Says whether there was one.
    pub fn drop_client(&self) -> bool {
        match self.who.lock().unwrap().as_ref() {
            Some((_, conn)) => {
                conn.close(CLOSE_DROPPED.into(), b"the host ended the connection");
                true
            }
            None => false,
        }
    }

    /// Drop the connected client with these words as the closing reason (one that begins
    /// `locked: ` asks the client for its user's password, as an `error` with that code does;
    /// docs/PROTOCOL.md, "A locked host"). It stays trusted. Says whether there was one.
    pub fn close_client(&self, reason: &str) -> bool {
        match self.who.lock().unwrap().as_ref() {
            Some((_, conn)) => {
                conn.close(CLOSE_DROPPED.into(), reason.as_bytes());
                true
            }
            None => false,
        }
    }

    /// The connected client, if there is one.
    pub fn link(&self) -> Option<Link> {
        self.link.lock().unwrap().clone()
    }

    /// Stop listening and drop the client.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.endpoint.close(0u32.into(), b"bye");
        if let Some(rt) = self.runtime.take() {
            let endpoint = self.endpoint.clone();
            let _ = rt.block_on(async { tokio::time::timeout(Duration::from_millis(500), endpoint.wait_idle()).await });
            rt.shutdown_timeout(Duration::from_secs(1));
            // The handler is still taking down what the client had (its display, the encoder
            // and its GPU context). A program that exits in the middle of that can hang in
            // the GPU driver's own exit code, so wait for it: the handler's thread ends once
            // nothing can call it any more.
            let (closed, _) = mpsc::channel::<Call>();
            *self.calls.lock().unwrap() = closed;
            let _ = self.handler_done.lock().unwrap().recv_timeout(Duration::from_secs(5));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Why the host closed a connection, as the code of the close: 0 it stopped or another device
/// took over, 1 something failed, 2 the host's owner ended it, 3 the device's trust was taken away.
pub const CLOSE_DROPPED: u32 = 2;
pub const CLOSE_REVOKED: u32 = 3;

async fn accept_loop(endpoint: remoteman_transport::Endpoint, trust: Arc<Trust>, options: HostOptions, calls: mpsc::Sender<Call>, current_link: Arc<Mutex<Option<Link>>>, who: Who, features: Arc<Mutex<Vec<String>>>, confirm: Arc<Confirm>) {
    let options = Arc::new(options);
    let mut current: Option<(tokio::task::JoinHandle<()>, Connection)> = None;
    while let Some(incoming) = endpoint.accept().await {
        // A stranger is refused inside the handshake; this only fails for them.
        let Ok(conn) = incoming.await else { continue };
        let Some(fingerprint) = peer_fingerprint(&conn) else { continue };
        // Only reachable while pairing: a device nobody knows. It is trusted here and now,
        // or held until the host's owner has confirmed it (in `serve`, once it has said its name).
        let mut held = None;
        if !trust.contains(&fingerprint) {
            trust.set_pairing(false);
            if confirm.wanted.load(std::sync::atomic::Ordering::Relaxed) {
                held = Some(confirm.clone());
            } else {
                let _ = trust.add(&fingerprint, &conn.remote_address().ip().to_string());
            }
        }
        // The newest client takes over. The old session is closed and left to finish by
        // itself, so the handler hears `left` for it exactly once, and before the new `connected`.
        if let Some((old, old_conn)) = current.take() {
            old_conn.close(0u32.into(), b"another device took over");
            let _ = old.await;
        }
        let (options, calls, current_link) = (options.clone(), calls.clone(), current_link.clone());
        let session_conn = conn.clone();
        *who.lock().unwrap() = Some((fingerprint.clone(), conn.clone()));
        // Revoked between its handshake and here: it doesn't get a session.
        if held.is_none() && !trust.contains(&fingerprint) {
            conn.close(CLOSE_REVOKED.into(), b"this device is no longer trusted here");
        }
        let (who, trust) = (who.clone(), trust.clone());
        let offered = features.lock().unwrap().clone();
        current = Some((tokio::spawn(async move {
            let started = serve(&conn, fingerprint, &options, &calls, &current_link, &trust, offered, held).await;
            {
                let mut who = who.lock().unwrap();
                if who.as_ref().is_some_and(|(_, c)| c.stable_id() == conn.stable_id()) {
                    who.take();
                }
            }
            match started {
                // The handler was told about this client, so it is told when it goes.
                Ok(true) | Err((true, _)) => {
                    let _ = calls.send(Call::Left);
                    current_link.lock().unwrap().take();
                }
                _ => {}
            }
            if let Err((_, e)) = started {
                if conn.close_reason().is_none() {
                    conn.close(1u32.into(), format!("{e:#}").as_bytes());
                }
            }
        }), session_conn));
    }
}

/// A frame that nothing has followed for this long is sent once more. On a desktop where
/// nothing moves no later frame comes, and a later frame is the only way a client notices that
/// one went missing: without this, whatever the network lost of the last frame before the
/// picture went still was never shown. A client that has the frame drops the copy unseen.
const REPEAT_AFTER: Duration = Duration::from_millis(100);

/// One client's session. The flag in the result says whether the handler's `connected` ran (and
/// so whether `left` is owed).
async fn serve(conn: &Connection, fingerprint: String, options: &HostOptions, calls: &mpsc::Sender<Call>, current_link: &Mutex<Option<Link>>, trust: &Trust, features: Vec<String>, held: Option<Arc<Confirm>>) -> Result<bool, (bool, anyhow::Error)> {
    let no = |e: anyhow::Error| (false, e);
    let clock = Instant::now();
    let (mut send, mut recv) = conn.accept_bi().await.context("control stream").map_err(no)?;
    let Some(Message::ClientHello { proto, name, display, decoders, cursor, unlock, way_in }) = read_message(&mut recv).await.map_err(no)? else {
        return Err(no(anyhow::anyhow!("the first message wasn't client_hello")));
    };
    // A new device, held for the owner's word: it is told to wait (and can show the code),
    // and gets its hello only after a yes.
    if let Some(confirm) = held {
        let device = PendingDevice { name: name.trim().to_owned(), fingerprint: fingerprint.clone(), code: remoteman_transport::pair_code(&confirm.own, &fingerprint), seconds: CONFIRM_SECONDS };
        let (answer, answered) = tokio::sync::oneshot::channel::<bool>();
        *confirm.waiting.lock().unwrap() = Some((device, Instant::now(), answer));
        write_message(&mut send, &Message::Pairing { seconds: CONFIRM_SECONDS }).await.map_err(no)?;
        let decision = tokio::select! {
            said = tokio::time::timeout(Duration::from_secs(CONFIRM_SECONDS as u64), answered) => said.ok().and_then(|s| s.ok()),
            _ = conn.closed() => Some(false),
        };
        // Whatever came of it, nobody is waiting any more (unless a newer device already is).
        {
            let mut waiting = confirm.waiting.lock().unwrap();
            if waiting.as_ref().is_some_and(|(d, _, _)| d.fingerprint == fingerprint) {
                waiting.take();
            }
        }
        match decision {
            Some(true) => trust.add(&fingerprint, name.trim()).map_err(no)?,
            other => {
                let message = if other.is_some() { "pairing was declined on the computer" } else { "nobody confirmed the pairing on the computer in time" };
                let _ = write_message(&mut send, &Message::Error { code: "not_paired".into(), message: message.into() }).await;
                let _ = send.finish();
                // (Time for the reason to arrive before the connection goes.)
                tokio::time::sleep(Duration::from_millis(200)).await;
                conn.close(CLOSE_REVOKED.into(), message.as_bytes());
                return Err(no(anyhow::anyhow!("a new device was not confirmed")));
            }
        }
    }
    write_message(&mut send, &Message::HostHello { proto: remoteman_proto::PROTO, name: options.name.clone(), os: options.os.clone(), features }).await.map_err(no)?;
    // The list of trusted devices shows the name the device gives itself.
    if !name.trim().is_empty() {
        let _ = trust.rename(&fingerprint, name.trim());
    }
    if proto != remoteman_proto::PROTO {
        let message = format!("this host speaks protocol {}", remoteman_proto::PROTO);
        let _ = write_message(&mut send, &Message::Error { code: "unsupported".into(), message }).await;
        return Err(no(anyhow::anyhow!("the client speaks protocol {proto}")));
    }

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Out>(4);
    let (cursor_tx, mut cursor_rx) = tokio::sync::mpsc::unbounded_channel::<CursorMsg>();
    let link = Link { frames: frame_tx, cursor: cursor_tx, clock, second: false };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let info = ClientInfo { name, fingerprint, display, decoders, cursor, unlock, attach_only: way_in.as_deref() == Some("attach") };
    // Reachable through `Host::link` before the handler hears of the client, so nothing it
    // sends from the start is lost.
    *current_link.lock().unwrap() = Some(link.clone());
    calls.send(Call::Connected(info, link, reply_tx)).ok().context("the host is shutting down").map_err(no)?;
    let accept = match reply_rx.await {
        Ok(Ok(a)) => a,
        Ok(Err(message)) => {
            current_link.lock().unwrap().take();
            // A handler that says "locked: ..." means the client may try again with its
            // user's password in the hello; one that says "logging in: ..." means the
            // computer is getting its user's session ready and the client should simply try
            // again in a few seconds; anything else is a plain failure.
            let (code, said) = if let Some(said) = message.strip_prefix("locked: ") {
                ("locked", said)
            } else if let Some(said) = message.strip_prefix("logging in: ") {
                ("logging_in", said)
            } else {
                ("failed", message.as_str())
            };
            let _ = write_message(&mut send, &Message::Error { code: code.into(), message: said.to_owned() }).await;
            // The handler refused, so it holds nothing for this client.
            return Err(no(anyhow::anyhow!(message)));
        }
        Err(_) => return Err(no(anyhow::anyhow!("the host is shutting down"))),
    };
    let yes = |e: anyhow::Error| (true, e);
    // Counts up each time the picture starts afresh.
    let epoch = std::sync::atomic::AtomicU32::new(1);
    let now = || epoch.load(std::sync::atomic::Ordering::Relaxed) as u16;
    write_message(&mut send, &Message::Stream { epoch: now(), mode: accept.mode, width: accept.width, height: accept.height, refresh: accept.refresh, colour: accept.colour, index: 0, sdr_white: accept.sdr_white, peak: accept.peak })
        .await
        .map_err(yes)?;
    // Three things write to the control stream: answers to the client, cursor shapes, and a
    // new stream description when the picture starts afresh.
    let send = tokio::sync::Mutex::new(send);

    let drop_one_in = options.test_drop_one_in.filter(|n| *n > 1);
    // The number of the last frame sent that the client can restart from, plus one.
    let answer = std::sync::atomic::AtomicU32::new(0);
    let video = async {
        let mut number = 0u32;
        let mut sent = 0u32;
        // The second display's picture: how often it has started afresh, and its frame count.
        let (mut epoch2, mut number2) = (0u16, 0u32);
        // The last frame sent of each picture, and when it goes out once more if nothing has
        // followed it (see `REPEAT_AFTER`).
        // With the datagram size it was cut for: the copy is cut the same way, so that it fits the pieces
        // the client already holds, and goes only while datagrams of that size are still taken
        // (the limit grows in a connection's first moments, and can shrink on a bad path).
        let mut last: [Option<(Frame, u16, u32, tokio::time::Instant, usize)>; 2] = [None, None];
        loop {
            let due = last.iter().flatten().map(|l| l.3).min();
            let out = match due {
                Some(due) => match tokio::time::timeout_at(due, frame_rx.recv()).await {
                    Ok(out) => out,
                    Err(_) => {
                        let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
                        for (index, slot) in last.iter_mut().enumerate() {
                            if slot.as_ref().is_some_and(|l| l.3 <= tokio::time::Instant::now()) {
                                let (f, epoch, number, _, cut) = slot.take().unwrap();
                                if cut > max {
                                    continue;
                                }
                                let kind = if index == 0 { remoteman_proto::video::KIND_VIDEO } else { remoteman_proto::video::KIND_VIDEO2 };
                                for d in remoteman_proto::video::fragment_as(kind, &f.data, f.key, f.recovery, epoch, number, f.capture_us, cut) {
                                    conn.send_datagram_wait(d.into()).await?;
                                }
                            }
                        }
                        continue;
                    }
                },
                None => frame_rx.recv().await,
            };
            let Some(out) = out else { break };
            let f = match out {
                Out::Second(second) => {
                    match *second {
                        Out::Restart(a) => {
                            epoch2 = epoch2.wrapping_add(1);
                            number2 = 0;
                            last[1] = None;
                            let m = Message::Stream { epoch: epoch2, mode: a.mode, width: a.width, height: a.height, refresh: a.refresh, colour: a.colour, index: 1, sdr_white: a.sdr_white, peak: a.peak };
                            write_message(&mut *send.lock().await, &m).await?;
                        }
                        Out::Frame(f) => {
                            let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
                            for d in remoteman_proto::video::fragment_as(remoteman_proto::video::KIND_VIDEO2, &f.data, f.key, f.recovery, epoch2, number2, f.capture_us, max) {
                                conn.send_datagram_wait(d.into()).await?;
                            }
                            last[1] = Some((f, epoch2, number2, tokio::time::Instant::now() + REPEAT_AFTER, max));
                            number2 = number2.wrapping_add(1);
                        }
                        Out::Second(_) => {}
                    }
                    continue;
                }
                Out::Frame(f) => f,
                Out::Restart(a) => {
                    epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    number = 0;
                    answer.store(0, std::sync::atomic::Ordering::Relaxed);
                    last[0] = None;
                    let m = Message::Stream { epoch: now(), mode: a.mode, width: a.width, height: a.height, refresh: a.refresh, colour: a.colour, index: 0, sdr_white: a.sdr_white, peak: a.peak };
                    write_message(&mut *send.lock().await, &m).await?;
                    continue;
                }
            };
            let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
            for d in fragment(&f.data, f.key, f.recovery, now(), number, f.capture_us, max) {
                sent = sent.wrapping_add(1);
                if drop_one_in.is_some_and(|n| sent % n == 0) {
                    continue;
                }
                conn.send_datagram_wait(d.into()).await?;
            }
            if f.key || f.recovery {
                answer.store(number.wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
            }
            last[0] = Some((f, now(), number, tokio::time::Instant::now() + REPEAT_AFTER, max));
            number = number.wrapping_add(1);
        }
        // The handler dropped its link without the client leaving: nothing more to send.
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    let control = async {
        while let Some(m) = read_message(&mut recv).await? {
            match m {
                Message::Keyframe => {
                    let _ = calls.send(Call::Keyframe);
                }
                Message::Lost { first, last, epoch: of } => {
                    // About a picture that has since been replaced: its frame numbers mean
                    // nothing now.
                    if of.is_some_and(|e| e != now()) {
                        continue;
                    }
                    // Said before the last keyframe or recovery point could have reached the
                    // client: that frame is the answer, and a second one would only add a burst.
                    let answered = answer.load(std::sync::atomic::Ordering::Relaxed);
                    if answered == 0 || last >= answered - 1 {
                        let _ = calls.send(Call::Lost(first, last));
                    }
                }
                Message::Ping { id, client_us } => write_message(&mut *send.lock().await, &Message::Pong { id, client_us, host_us: clock.elapsed().as_micros() as u64 }).await?,
                m @ (Message::Sound { .. } | Message::Mic { .. } | Message::Camera { .. } | Message::Configure { .. } | Message::Clipboard { .. } | Message::FileResult { .. } | Message::Display { .. } | Message::DisplayKeyframe { .. }) => {
                    let _ = calls.send(Call::Message(m));
                }
                _ => {}
            }
        }
        anyhow::Ok(())
    };
    let cursor = async {
        // The shape goes on the control stream (it must arrive); the position goes as a
        // datagram (only the newest matters).
        let mut last = None;
        while let Some(c) = cursor_rx.recv().await {
            match c {
                CursorMsg::Shape(s) => {
                    let m = Message::Cursor { width: s.width, height: s.height, hot_x: s.hot_x, hot_y: s.hot_y, rgba: encode_bytes(&s.rgba) };
                    write_message(&mut *send.lock().await, &m).await?;
                }
                CursorMsg::At { x, y, visible } => {
                    let at = CursorAt { epoch: now(), visible, x, y };
                    if last != Some(at) {
                        last = Some(at);
                        let _ = conn.send_datagram(at.write().into());
                    }
                }
                CursorMsg::Message(m) => write_message(&mut *send.lock().await, &m).await?,
                CursorMsg::Sound(packet) => {
                    let _ = conn.send_datagram(packet.write().into());
                }
                CursorMsg::Blob { kind, header, content } => {
                    tokio::spawn(blob::send(conn.clone(), kind, header, content));
                }
            }
        }
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    // Streams the client opens: its input (the first, which begins with an input event's
    // type) and blobs (which begin with a kind of 0x40 or more). Ended with this session.
    let streams = async {
        let mut readers = tokio::task::JoinSet::new();
        // At most this many pieces of blob content wait for the handler at once.
        let room = Arc::new(tokio::sync::Semaphore::new(32));
        let mut blob_id = 0u32;
        loop {
            let mut stream = conn.accept_uni().await?;
            blob_id = blob_id.wrapping_add(1);
            let (calls, room, id) = (calls.clone(), room.clone(), blob_id);
            // Each stream is read by a task of its own from its very first byte: a stream is
            // announced as soon as a later one carries data, so the input stream can turn up
            // here long before the client has typed anything.
            readers.spawn(async move {
                let mut first = [0u8; 1];
                if stream.read_exact(&mut first).await.is_err() {
                    return;
                }
                if first[0] >= media::BLOB_FIRST {
                    blob::receive(stream, first[0], id, move |event| {
                        let (calls, room) = (calls.clone(), room.clone());
                        async move {
                            let held = room.acquire_owned().await.ok();
                            calls.send(Call::Blob(event, held)).is_ok()
                        }
                    })
                    .await;
                } else {
                    let _ = read_input(stream, first[0], &calls).await;
                }
            });
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    };
    // Datagrams the client sends: its microphone and its camera.
    let media_in = async {
        let mut camera = Reassembler::of(media::KIND_CAMERA);
        // After a loss, nothing of the camera is handed on until a keyframe.
        let (mut need_key, mut asked) = (true, None::<Instant>);
        loop {
            let d = conn.read_datagram().await?;
            match d.first() {
                Some(&media::KIND_MIC) => {
                    if let Ok(packet) = Packet::parse(&d) {
                        let _ = calls.send(Call::Mic(packet));
                    }
                }
                Some(&media::KIND_CAMERA) => {
                    // The camera switched on again counts as a new epoch, from frame 0.
                    if let Ok((f, _)) = remoteman_proto::video::Fragment::parse_as(media::KIND_CAMERA, &d) {
                        if f.epoch != camera.epoch() {
                            camera.reset(f.epoch);
                            need_key = true;
                        }
                    }
                    let Ok(Some(frame)) = camera.push(&d) else { continue };
                    if camera.take_lost().is_some() {
                        need_key = true;
                    }
                    if need_key && !frame.key {
                        if asked.is_none_or(|t| t.elapsed() > Duration::from_millis(250)) {
                            asked = Some(Instant::now());
                            write_message(&mut *send.lock().await, &Message::CameraKeyframe).await?;
                        }
                        continue;
                    }
                    need_key = false;
                    let _ = calls.send(Call::Camera(frame));
                }
                _ => {}
            }
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    };
    let result = tokio::select! {
        r = video => r.context("video"),
        r = control => r.context("control"),
        r = cursor => r.context("cursor"),
        r = streams => r.context("streams"),
        r = media_in => r.context("microphone and camera"),
        _ = conn.closed() => Ok(()),
    };
    // The client leaving ends whichever part was waiting on it; that isn't a failure.
    if conn.close_reason().is_some() { Ok(true) } else { result.map(|()| true).map_err(yes) }
}

/// Hand on every whole event at the front of `buf` and take them out of it. An event that is
/// whole but can't be read (a button this version doesn't know, text that isn't UTF-8) is
/// stepped over: ending here would leave the client with a picture and no input for the rest
/// of its session, and nothing to tell it so. Only a type this version doesn't know at all
/// ends the input, since there is no telling where its event stops (docs/PROTOCOL.md, "Input").
fn take_events(buf: &mut Vec<u8>, mut sink: impl FnMut(Event)) -> Result<()> {
    let mut at = 0;
    let result = loop {
        match Event::parse(&buf[at..]) {
            Ok(Some((event, used))) => {
                at += used;
                sink(event);
            }
            Ok(None) => break Ok(()),
            Err(e) => match Event::length(&buf[at..]) {
                Some(n) if buf.len() - at >= n => at += n,
                _ => break Err(e.into()),
            },
        }
    };
    buf.drain(..at);
    result
}

async fn read_input(mut stream: RecvStream, first: u8, calls: &mpsc::Sender<Call>) -> Result<()> {
    // `first` was read off the stream to see what it was.
    let mut buf = vec![first];
    let mut chunk = [0u8; 4096];
    while let Some(n) = stream.read(&mut chunk).await? {
        buf.extend_from_slice(&chunk[..n]);
        let mut gone = false;
        take_events(&mut buf, |event| gone |= calls.send(Call::Input(event)).is_err())?;
        if gone {
            bail!("the host is shutting down");
        }
    }
    // The input stream closing isn't the session ending: wait for the connection to say so.
    std::future::pending::<()>().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteman_proto::input::Button;

    #[test]
    fn input_goes_on_after_an_event_that_cannot_be_read() {
        let mut buf = Vec::new();
        Event::Key { usage: 4, down: true }.write(&mut buf);
        buf.extend_from_slice(&[3, 9, 1]); // a button this version doesn't know
        buf.extend_from_slice(&[7, 2, 0, 0xff, 0xfe]); // text that isn't UTF-8
        buf.extend_from_slice(&[6, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // a touch phase it doesn't know
        Event::Button { button: Button::Left, down: true }.write(&mut buf);
        buf.extend_from_slice(&[5, 4]); // half a key
        let mut got = Vec::new();
        take_events(&mut buf, |e| got.push(e)).expect("input ended at an event that could have been stepped over");
        assert_eq!(got, [Event::Key { usage: 4, down: true }, Event::Button { button: Button::Left, down: true }]);
        assert_eq!(buf, [5, 4]);
        // A type nobody knows still ends it: its length can't be known.
        let mut buf = vec![99, 1, 2];
        assert!(take_events(&mut buf, |_| ()).is_err());
    }
}
