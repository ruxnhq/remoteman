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
pub use crate::permissions::{ConnectionEvent, Permissions};
pub use crate::join::SecondDevice;
use crate::join::{self, Lead, LeadEnd, LeadSlot};
use crate::permissions::{ConnectionLog, PermissionStore};
use crate::second_factor::{self, Check, SecondFactorStore};
use crate::rate::RateController;
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
    /// Set by the daemon, never by the device: it logged in on this computer's own login
    /// screen (Plasma Login's), so the session it is handed opened on the computer's
    /// monitors, unlocked. It is treated as unlocked for the device: the monitors go off
    /// while it is here and the session is locked when it leaves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub logged_in_here: bool,
}

/// What [`Host::second_factor`] says when it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecondFactorStatus {
    /// Devices told "don't ask again" and not yet expired.
    pub remembered: usize,
}

/// What the owner needs to put the second factor into an authenticator app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecondFactorSetup {
    /// Base32.
    pub secret: String,
    /// The secret in groups of four, for typing in.
    pub grouped: String,
    /// `otpauth://totp/remoteman:<name>?secret=...&issuer=remoteman`, for a QR code or a link.
    pub uri: String,
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
    /// What the host encodes at when the client has set no ceiling, in bits a second. A host that
    /// follows the network (see [`Self::bitrate`]) says it here; 0 (the default) means it
    /// doesn't, and nothing below is ever called.
    fn default_bitrate(&mut self) -> u32 {
        0
    }
    /// The picture should be encoded at this many bits a second from now on: the core lowers it
    /// when the client reports loss and raises it again after a clean stretch, never above the
    /// ceiling (what the client set with `configure`, or [`Self::default_bitrate`]); see
    /// docs/PROTOCOL.md, "Settings". Change the encoder's rate live, with no keyframe. A ceiling
    /// the client sets is applied by the host itself through [`Self::message`], as before; this
    /// is called only for the steps below it. The core tells the client with `status`.
    fn bitrate(&mut self, bits_per_second: u32) {
        let _ = bits_per_second;
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
    /// Something the host's owner may want to be told of: a device `paired`, `connected` or
    /// was `refused` (see [`Host::connections`]). Called on the handler's thread, in turn with
    /// everything else, so the core never waits for it; keep it short anyway.
    fn notice(&mut self, _event: &ConnectionEvent) {}
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

pub(crate) enum Call {
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
    Notice(ConnectionEvent),
}

/// The permissions and the log, shared by the host's tasks. A [`Gate`] asks them about one
/// device.
pub(crate) struct State {
    perms: PermissionStore,
    log: ConnectionLog,
    /// Called, on the handler's thread, with each notice (see [`Host::set_notice_hook`]).
    notice: Mutex<Option<Box<dyn FnMut(&ConnectionEvent) + Send>>>,
    /// The second factor (docs/PROTOCOL.md, "Second factor"): the secret, the devices that need
    /// not be asked, the count of wrong codes.
    factor: SecondFactorStore,
    /// How long a device has to answer the host's ask for a code (60 s; [`Host::set_second_factor_wait`]).
    factor_wait: std::sync::atomic::AtomicU64,
    /// This computer's name, for the authenticator app's list.
    name: String,
    /// What a device that connects while another is using the computer does
    /// ([`SecondDevice`], as 0, 1, 2), kept in `host/second_device`.
    second: std::sync::atomic::AtomicU8,
    second_path: PathBuf,
}

impl State {
    fn second_device(&self) -> SecondDevice {
        SecondDevice::from_u8(self.second.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or_default()
    }
}

/// What the device being served may do, asked again at each use so a change applies at once.
#[derive(Clone)]
pub(crate) struct Gate {
    pub(crate) state: Arc<State>,
    pub(crate) fingerprint: String,
}

impl Gate {
    pub(crate) fn get(&self) -> Permissions {
        self.state.perms.get(&self.fingerprint)
    }
}

/// Whether a message from the client is one its permissions let through. Asking for a thing
/// to start is what is refused; saying it stopped always goes through.
fn message_allowed(m: &Message, p: &Permissions) -> bool {
    match m {
        Message::Sound { on } => !on || p.sound,
        Message::Mic { on, .. } => !on || p.mic,
        Message::Camera { on, .. } => !on || p.camera,
        Message::Clipboard { on } => !on || p.clipboard,
        Message::FileResult { .. } => p.files,
        _ => true,
    }
}

/// What a host program that isn't a [`HostHandler`] with its own `bitrate` gives the core to
/// follow the network with instead (see [`Host::set_bitrate_hook`]): its default rate, and what
/// to call with each new target.
struct BitrateHook {
    default: u32,
    apply: Box<dyn FnMut(u32) + Send>,
}

/// Every host the core runs lists `parity` (docs/PROTOCOL.md, "Parity"): the core cuts the
/// datagrams, and sends parity only to a client that asked for it.
fn with_parity(mut features: Vec<String>) -> Vec<String> {
    if !features.iter().any(|f| f == "parity") {
        features.push("parity".into());
    }
    features
}

/// The bitrate following the network, for the connected client, on the handler's thread.
#[derive(Default)]
struct Follow {
    /// Whether anything takes the new targets.
    follows: bool,
    /// What the host encodes at when the client set no ceiling.
    default: u32,
    /// The ceiling the client set (0: none).
    asked: u32,
    control: Option<RateController>,
}

impl Follow {
    fn start(handler: &mut dyn HostHandler, hook: &Mutex<Option<BitrateHook>>) -> Self {
        let own = handler.default_bitrate();
        let hooked = hook.lock().unwrap().as_ref().map(|h| h.default);
        let default = if own > 0 { own } else { hooked.unwrap_or(0) };
        let mut f = Self { follows: own > 0 || hooked.is_some(), default, asked: 0, control: None };
        f.rebuild();
        f
    }

    fn rebuild(&mut self) {
        let ceiling = if self.asked > 0 { self.asked } else { self.default };
        self.control = (self.follows && ceiling > 0).then(|| RateController::new(ceiling));
    }

    /// The client's `configure`: the host applied its ceiling itself, and the picture may have
    /// started afresh at it, so the target starts again from there.
    fn configured(&mut self, bitrate: Option<u32>) {
        if let Some(b) = bitrate {
            self.asked = b;
        }
        self.rebuild();
    }

    /// Put a new target into effect: the host's encoder, and the client's `status`.
    fn apply(&self, step: Option<u32>, handler: &mut dyn HostHandler, hook: &Mutex<Option<BitrateHook>>, link: &Mutex<Option<Link>>) {
        let Some(target) = step else { return };
        match hook.lock().unwrap().as_mut() {
            Some(h) if handler.default_bitrate() == 0 => (h.apply)(target),
            _ => handler.bitrate(target),
        }
        if let Some(link) = link.lock().unwrap().as_ref() {
            link.send_message(Message::Status { bitrate: Some(target) });
        }
    }
}

/// A device this host trusts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    /// The fingerprint of its certificate: what it is trusted by.
    pub fingerprint: String,
    /// The name it gave in its last hello, or the address it paired from until it has said one.
    pub name: String,
    /// It is a client connected right now (the one being served, or one that joined it).
    pub connected: bool,
    /// It is connected as a second device that joined the session of another.
    pub joined: bool,
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
    /// When [`Host::pending`] was last asked: the sign that somebody (the owner's app) is there to
    /// answer. With `approver_wait` (ms, 0: off) a device that waits that long with nobody having
    /// asked is turned away at once instead of after the full minute.
    last_look: Mutex<Option<Instant>>,
    approver_wait: std::sync::atomic::AtomicU64,
}

pub struct Host {
    /// The session joiners join, while one is up.
    lead: LeadSlot,
    confirm: Arc<Confirm>,
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: remoteman_transport::Endpoint,
    trust: Arc<Trust>,
    link: Arc<Mutex<Option<Link>>>,
    hook: Arc<Mutex<Option<BitrateHook>>>,
    who: Who,
    /// What this host can do, as the next hello will say it.
    features: Arc<Mutex<Vec<String>>>,
    pub(crate) state: Arc<State>,
    calls: Mutex<mpsc::Sender<Call>>,
    /// Told once the handler thread has ended and let go of everything it held.
    handler_done: Mutex<mpsc::Receiver<()>>,
    /// This host's certificate fingerprint, for pairing by hand.
    pub fingerprint: String,
    /// The address actually listened on.
    pub address: SocketAddr,
}

/// The fingerprint of the host whose state is in `state` (its identity is in `state/host`, as
/// [`Host::start`] keeps it), the identity made first if there is none: what a viewer checks
/// this host against, without starting it.
pub fn fingerprint(state: &std::path::Path) -> Result<String> {
    Ok(Identity::load_or_create(&state.join("host"))?.fingerprint)
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
        let link: Arc<Mutex<Option<Link>>> = Arc::default();
        let hook: Arc<Mutex<Option<BitrateHook>>> = Arc::default();
        let state = Arc::new(State { perms: PermissionStore::load(&options.state.join("host/permissions.json")), log: ConnectionLog::open(&options.state.join("connections.log")), notice: Mutex::default(), factor: SecondFactorStore::load(&options.state.join("host/second_factor.json")), factor_wait: 60_000.into(), name: options.name.clone(), second: (std::fs::read_to_string(options.state.join("host/second_device")).ok().and_then(|s| SecondDevice::parse(&s)).unwrap_or_default() as u8).into(), second_path: options.state.join("host/second_device") });
        let (thread_link, thread_hook, thread_state) = (link.clone(), hook.clone(), state.clone());
        std::thread::Builder::new().name("remoteman host".into()).spawn(move || {
            let (link, hook, state) = (thread_link, thread_hook, thread_state);
            let clock = Instant::now();
            let mut follow = Follow::default();
            loop {
                let call = match call_rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(call) => Some(call),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let now = clock.elapsed().as_millis() as u64;
                // The bitrate following the network: a step up after a clean stretch.
                let mut step = follow.control.as_mut().and_then(|c| c.tick(now));
                let Some(call) = call else {
                    follow.apply(step, &mut *handler, &hook, &link);
                    continue;
                };
                match call {
                    Call::Connected(client, link_to_client, reply) => {
                        let answer = handler.connected(&client, link_to_client);
                        follow = if answer.is_ok() { Follow::start(&mut *handler, &hook) } else { Follow::default() };
                        step = None;
                        let _ = reply.send(answer);
                    }
                    Call::Keyframe => handler.keyframe(),
                    Call::Lost(first, last) => {
                        handler.lost(first, last);
                        step = follow.control.as_mut().and_then(|c| c.loss(now)).or(step);
                    }
                    Call::Input(event) => handler.input(event),
                    Call::Message(message) => {
                        handler.message(&message);
                        if let Message::Configure { bitrate, .. } = &message {
                            follow.configured(*bitrate);
                        }
                    }
                    Call::Mic(packet) => handler.mic(&packet),
                    Call::Camera(frame) => handler.camera(&frame),
                    Call::Blob(event, held) => {
                        handler.blob(&event);
                        drop(held);
                    }
                    Call::Notice(event) => {
                        handler.notice(&event);
                        if let Some(f) = state.notice.lock().unwrap().as_mut() {
                            f(&event);
                        }
                    }
                    Call::Left => {
                        handler.left();
                        follow = Follow::default();
                        step = None;
                    }
                }
                follow.apply(step, &mut *handler, &hook, &link);
            }
            // Nothing more will come: whatever the handler still holds is let go of here,
            // and whoever is stopping the host is told when that is done.
            drop(handler);
            let _ = done.send(());
        })?;

        let who = Who::default();
        let features = Arc::new(Mutex::new(with_parity(options.features.clone())));
        let confirm = Arc::new(Confirm { own: identity.fingerprint.clone(), ..Confirm::default() });
        let lead = LeadSlot::default();
        runtime.spawn(accept_loop(endpoint.clone(), trust.clone(), options, calls.clone(), link.clone(), who.clone(), features.clone(), confirm.clone(), state.clone(), lead.clone()));
        Ok(Self { lead, confirm, runtime: Some(runtime), endpoint, trust, link, hook, who, features, state, calls: Mutex::new(calls), handler_done: Mutex::new(handler_done), fingerprint: identity.fingerprint, address })
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

    /// For a host that runs where nobody may be there to answer (a Windows service at the sign-in
    /// screen): a new device that has waited this long without anybody having called [`Host::pending`]
    /// since it came is refused, with "Nobody is signed in on this PC to accept the device", rather
    /// than left waiting out the minute. The owner's app asks about once a second, so it is "there".
    pub fn set_approver_wait(&self, wait: Option<Duration>) {
        self.confirm.approver_wait.store(wait.map_or(0, |w| w.as_millis() as u64), std::sync::atomic::Ordering::Relaxed);
    }

    /// The device waiting to be confirmed, if there is one.
    pub fn pending(&self) -> Option<PendingDevice> {
        *self.confirm.last_look.lock().unwrap() = Some(Instant::now());
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
        let features = with_parity(features);
        *self.features.lock().unwrap() = features;
        self.send_features();
    }

    /// Tell the connected client what it may use now: the host's features less what its
    /// permissions take away.
    fn send_features(&self) {
        let fingerprint = self.who.lock().unwrap().as_ref().map(|(fp, _)| fp.clone());
        if let (Some(link), Some(fp)) = (self.link(), fingerprint) {
            let features = self.state.perms.get(&fp).filter(&self.features.lock().unwrap());
            link.send_message(Message::Features { features });
        }
    }

    /// What a device may do (everything, for one nobody has restricted).
    pub fn permissions(&self, fingerprint: &str) -> Permissions {
        self.state.perms.get(fingerprint)
    }

    /// Change what a device may do. Kept across restarts. If it is the connected one it is
    /// told at once with a `features` message, and the handler is told to switch off what was
    /// taken away (sound, microphone, camera and clipboard sharing, as if the client had
    /// stopped them); input, files and the rest are dropped by the core from this moment.
    pub fn set_permissions(&self, fingerprint: &str, permissions: Permissions) -> Result<()> {
        let before = self.state.perms.get(fingerprint);
        self.state.perms.set(fingerprint, permissions)?;
        let connected = self.who.lock().unwrap().as_ref().is_some_and(|(fp, _)| fp == fingerprint);
        if connected {
            self.send_features();
            if before.sound && !permissions.sound {
                self.tell_handler(Message::Sound { on: false });
            }
            if before.mic && !permissions.mic {
                self.tell_handler(Message::Mic { on: false, codec: String::new(), rate: 0, channels: 0, frame_us: 0 });
            }
            if before.camera && !permissions.camera {
                self.tell_handler(Message::Camera { on: false, codec: String::new(), width: 0, height: 0, fps: 0 });
            }
            if before.clipboard && !permissions.clipboard {
                self.tell_handler(Message::Clipboard { on: false });
            }
        }
        Ok(())
    }

    /// Whether devices must give a code from an authenticator app (`Some`, with how many are
    /// remembered so as not to be asked), or not (`None`).
    pub fn second_factor(&self) -> Option<SecondFactorStatus> {
        self.state.factor.enabled().then(|| SecondFactorStatus { remembered: self.state.factor.remembered(second_factor::now()) })
    }

    /// Start turning the second factor on: a new secret, which the owner puts into an authenticator
    /// app. It is asked for only after [`Host::confirm_second_factor`] has had a right code. Fails
    /// if it is on already.
    pub fn enable_second_factor(&self) -> Result<SecondFactorSetup> {
        let secret = self.state.factor.enable()?;
        Ok(SecondFactorSetup { uri: second_factor::otpauth_uri(&self.state.name, &secret), grouped: second_factor::grouped(&secret), secret })
    }

    /// The code the app shows for the secret of [`Host::enable_second_factor`]: if it is right the
    /// second factor is on from now.
    pub fn confirm_second_factor(&self, code: &str) -> bool {
        self.state.factor.confirm(code, second_factor::now()).unwrap_or(false)
    }

    /// Turn the second factor off and throw the secret and the remembered devices away.
    pub fn disable_second_factor(&self) -> Result<()> {
        self.state.factor.disable()
    }

    /// Ask every device for a code again, even those told "don't ask again".
    pub fn forget_remembered(&self) -> Result<()> {
        self.state.factor.forget_remembered()
    }

    /// For tests: how long a device has to answer the ask for a code.
    pub fn set_second_factor_wait(&self, wait: Duration) {
        self.state.factor_wait.store(wait.as_millis() as u64, std::sync::atomic::Ordering::Relaxed);
    }

    /// For a host program whose handler doesn't implement [`HostHandler::notice`] (the C
    /// interface): `notice` is called, on the handler's thread, with each event the handler's
    /// own `notice` would get.
    pub fn set_notice_hook(&self, notice: impl FnMut(&ConnectionEvent) + Send + 'static) {
        *self.state.notice.lock().unwrap() = Some(Box::new(notice));
    }

    /// Take the hook of [`Self::set_notice_hook`] away.
    pub fn clear_notice_hook(&self) {
        *self.state.notice.lock().unwrap() = None;
    }

    /// The last `n` lines of the connection log, oldest first.
    pub fn connections(&self, n: usize) -> Vec<ConnectionEvent> {
        self.state.log.last(n)
    }

    /// Make the core lower and raise the picture's bitrate with the network for a host program
    /// whose handler doesn't implement [`HostHandler::bitrate`] (the C interface): `apply` is
    /// called, on the handler's thread, with each new target in bits a second, never above the
    /// client's ceiling or `default` (what the host encodes at when the client set none; 0:
    /// nothing is followed until the client sets one). Takes effect for the next client.
    pub fn set_bitrate_hook(&self, default: u32, apply: impl FnMut(u32) + Send + 'static) {
        *self.hook.lock().unwrap() = Some(BitrateHook { default, apply: Box::new(apply) });
    }

    /// Hand the handler a message as if the client had sent it, on the handler's own thread
    /// and in turn with everything else: how the host program itself switches something off
    /// for the connected client (`Message::Clipboard { on: false }` when its owner stops
    /// allowing the clipboard, say).
    pub fn tell_handler(&self, message: Message) {
        let _ = self.calls.lock().unwrap().send(Call::Message(message));
    }

    /// What a device that connects while another is using this computer does. Takes effect for
    /// the next connection; kept with the host's state.
    pub fn set_second_device(&self, mode: SecondDevice) {
        self.state.second.store(mode as u8, std::sync::atomic::Ordering::Relaxed);
        let _ = std::fs::write(&self.state.second_path, mode.as_str());
    }

    pub fn second_device(&self) -> SecondDevice {
        self.state.second_device()
    }

    /// Every device this host trusts, in the order they were paired.
    pub fn devices(&self) -> Vec<Device> {
        let connected = self.who.lock().unwrap().as_ref().map(|(fp, _)| fp.clone());
        let joined = self.lead.lock().unwrap().as_ref().map(|l| l.joined()).unwrap_or_default();
        self.trust.list().into_iter().map(|(fingerprint, name)| Device { connected: connected.as_deref() == Some(&fingerprint) || joined.contains(&fingerprint), joined: joined.contains(&fingerprint), fingerprint, name }).collect()
    }

    /// Stop trusting a device. It is dropped if it is the one connected, and refused from now
    /// on until it is paired again. Says whether it was trusted.
    pub fn revoke(&self, fingerprint: &str) -> Result<bool> {
        let was = self.trust.remove(fingerprint)?;
        let _ = self.state.perms.forget(fingerprint);
        let _ = self.state.factor.forget_device(fingerprint);
        if let Some(lead) = self.lead.lock().unwrap().as_ref() {
            lead.close_device(fingerprint, CLOSE_REVOKED, b"this device is no longer trusted here");
        }
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

async fn accept_loop(endpoint: remoteman_transport::Endpoint, trust: Arc<Trust>, options: HostOptions, calls: mpsc::Sender<Call>, current_link: Arc<Mutex<Option<Link>>>, who: Who, features: Arc<Mutex<Vec<String>>>, confirm: Arc<Confirm>, state: Arc<State>, lead_slot: LeadSlot) {
    let options = Arc::new(options);
    let mut current: Option<(tokio::task::JoinHandle<()>, Connection)> = None;
    while let Some(incoming) = endpoint.accept().await {
        // A stranger is refused inside the handshake; this only fails for them.
        let Ok(conn) = incoming.await else { continue };
        let Some(fingerprint) = peer_fingerprint(&conn) else { continue };
        // Only reachable while pairing: a device nobody knows. It is trusted here and now,
        // or held until the host's owner has confirmed it (in `serve`, once it has said its name).
        let mut held = None;
        let mut fresh = false;
        if !trust.contains(&fingerprint) {
            trust.set_pairing(false);
            if confirm.wanted.load(std::sync::atomic::Ordering::Relaxed) {
                held = Some(confirm.clone());
            } else {
                let _ = trust.add(&fingerprint, &conn.remote_address().ip().to_string());
                fresh = true;
            }
        }
        // With the host set to `watch` or `control`, a device that isn't the lead's own joins
        // the lead's session instead of ending it (docs/PROTOCOL.md, "More than one device").
        let second = state.second_device();
        let lead = if second == SecondDevice::TakeOver { None } else { lead_slot.lock().unwrap().clone().filter(|l| l.is_open() && l.fingerprint != fingerprint) };
        if let Some(lead) = lead {
            let (options, calls, trust, state) = (options.clone(), calls.clone(), trust.clone(), state.clone());
            let offered = features.lock().unwrap().clone();
            let (lead_slot, current_link) = (lead_slot.clone(), current_link.clone());
            tokio::spawn(async move {
                let started = serve(&conn, fingerprint, &options, &calls, &current_link, &lead_slot, &trust, &state, offered, held, fresh, Some((lead, second == SecondDevice::Control))).await;
                if let Err((_, e)) = started {
                    if conn.close_reason().is_none() {
                        conn.close(1u32.into(), format!("{e:#}").as_bytes());
                    }
                }
            });
            continue;
        }
        // The newest client takes over. The old session is closed and left to finish by
        // itself, so the handler hears `left` for it exactly once, and before the new `connected`.
        if let Some((old, old_conn)) = current.take() {
            old_conn.close(0u32.into(), b"another device took over");
            let _ = old.await;
        }
        let (options, calls, current_link, lead_slot) = (options.clone(), calls.clone(), current_link.clone(), lead_slot.clone());
        let session_conn = conn.clone();
        *who.lock().unwrap() = Some((fingerprint.clone(), conn.clone()));
        // Revoked between its handshake and here: it doesn't get a session.
        if held.is_none() && !trust.contains(&fingerprint) {
            conn.close(CLOSE_REVOKED.into(), b"this device is no longer trusted here");
        }
        let (who, trust, state) = (who.clone(), trust.clone(), state.clone());
        let offered = features.lock().unwrap().clone();
        current = Some((tokio::spawn(async move {
            let started = serve(&conn, fingerprint, &options, &calls, &current_link, &lead_slot, &trust, &state, offered, held, fresh, None).await;
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

/// Ends when a device that began waiting at `since` has waited the host's `approver_wait` with
/// nobody having asked [`Host::pending`] since; never, when that is off or somebody did.
async fn nobody_asks(confirm: &Confirm, since: Instant) {
    let wait = Duration::from_millis(confirm.approver_wait.load(std::sync::atomic::Ordering::Relaxed));
    if wait.is_zero() {
        return std::future::pending().await;
    }
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let looked = confirm.last_look.lock().unwrap().is_some_and(|l| l >= since);
        if looked {
            return std::future::pending().await;
        }
        if since.elapsed() >= wait {
            return;
        }
    }
}

/// The second factor's turn at admission: wait for `second_factor` messages from the client until
/// one is right (and then go on), or too many are wrong (error `second_factor`, the device refused
/// for 10 minutes), or the time is up (a client that doesn't know the message never answers).
async fn ask_second_factor(send: &mut remoteman_transport::SendStream, recv: &mut RecvStream, state: &State, fingerprint: &str, name: &str, address: &str, record: &(dyn Fn(ConnectionEvent) + Sync)) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(state.factor_wait.load(std::sync::atomic::Ordering::Relaxed));
    let fail = |code: &str, message: &str, event: &str| {
        let mut e = ConnectionEvent::new(event, name, fingerprint, address);
        e.reason = message.into();
        record(e);
        Message::Error { code: code.into(), message: message.into() }
    };
    loop {
        let message = match tokio::time::timeout_at(deadline, read_message(recv)).await {
            Err(_) => {
                let error = fail("second_factor", "This computer asks for a code from an authenticator app; update remoteman", "refused");
                let _ = write_message(send, &error).await;
                let _ = send.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
                bail!("no second-factor code came in time");
            }
            Ok(m) => m?.context("the client left before giving a code")?,
        };
        let Message::SecondFactor { code, remember } = message else { continue };
        match state.factor.check(fingerprint, &code, remember, second_factor::now()) {
            Check::Right => {
                record(ConnectionEvent::new("second_factor_ok", name, fingerprint, address));
                return Ok(());
            }
            Check::Wrong(left) => {
                record(ConnectionEvent::new("second_factor_wrong", name, fingerprint, address));
                write_message(send, &Message::SecondFactorWrong { left }).await?;
            }
            Check::Refused => {
                let error = fail("second_factor", "Too many wrong codes. Try again in 10 minutes.", "second_factor_wrong");
                let _ = write_message(send, &error).await;
                let _ = send.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
                bail!("too many wrong second-factor codes");
            }
        }
    }
}

/// Writes the `left` line of the connection log when a session ends, however it ends.
pub(crate) struct LeftLog {
    state: Arc<State>,
    conn: Connection,
    name: String,
    fingerprint: String,
    address: String,
    since: Instant,
    joined: bool,
}

impl LeftLog {
    pub(crate) fn new(state: Arc<State>, conn: Connection, name: String, fingerprint: String, address: String, joined: bool) -> Self {
        Self { state, conn, name, fingerprint, address, since: Instant::now(), joined }
    }
}

impl Drop for LeftLog {
    fn drop(&mut self) {
        let mut event = ConnectionEvent::new("left", &self.name, &self.fingerprint, &self.address);
        event.duration = Some(self.since.elapsed().as_secs());
        event.joined = self.joined;
        if let Some(reason) = self.conn.close_reason() {
            event.reason = reason.to_string();
        }
        self.state.log.append(&event);
    }
}

/// A frame that nothing has followed for this long is sent once more. On a desktop where
/// nothing moves no later frame comes, and a later frame is the only way a client notices that
/// one went missing: without this, whatever the network lost of the last frame before the
/// picture went still was never shown. A client that has the frame drops the copy unseen.
const REPEAT_AFTER: Duration = Duration::from_millis(100);

/// One client's session. The flag in the result says whether the handler's `connected` ran (and
/// so whether `left` is owed).
async fn serve(conn: &Connection, fingerprint: String, options: &HostOptions, calls: &mpsc::Sender<Call>, current_link: &Mutex<Option<Link>>, lead_slot: &LeadSlot, trust: &Trust, state: &Arc<State>, features: Vec<String>, held: Option<Arc<Confirm>>, fresh: bool, join: Option<(Arc<Lead>, bool)>) -> Result<bool, (bool, anyhow::Error)> {
    let no = |e: anyhow::Error| (false, e);
    let gate = Gate { state: state.clone(), fingerprint: fingerprint.clone() };
    let address = conn.remote_address().to_string();
    // Writes the event to the log and tells the handler (which may show a notice).
    let record = |event: ConnectionEvent| {
        state.log.append(&event);
        if event.event != "left" {
            let _ = calls.send(Call::Notice(event));
        }
    };
    let clock = Instant::now();
    let (mut send, mut recv) = conn.accept_bi().await.context("control stream").map_err(no)?;
    let Some(Message::ClientHello { proto, name, display, decoders, cursor, unlock, way_in, parity }) = read_message(&mut recv).await.map_err(no)? else {
        return Err(no(anyhow::anyhow!("the first message wasn't client_hello")));
    };
    // A new device, held for the owner's word: it is told to wait (and can show the code),
    // and gets its hello only after a yes.
    if let Some(confirm) = held {
        let device = PendingDevice { name: name.trim().to_owned(), fingerprint: fingerprint.clone(), code: remoteman_transport::pair_code(&confirm.own, &fingerprint), seconds: CONFIRM_SECONDS };
        let (answer, answered) = tokio::sync::oneshot::channel::<bool>();
        *confirm.waiting.lock().unwrap() = Some((device, Instant::now(), answer));
        write_message(&mut send, &Message::Pairing { seconds: CONFIRM_SECONDS }).await.map_err(no)?;
        let mut nobody = false;
        let decision = tokio::select! {
            said = tokio::time::timeout(Duration::from_secs(CONFIRM_SECONDS as u64), answered) => said.ok().and_then(|s| s.ok()),
            _ = conn.closed() => Some(false),
            _ = nobody_asks(&confirm, Instant::now()) => { nobody = true; None }
        };
        // Whatever came of it, nobody is waiting any more (unless a newer device already is).
        {
            let mut waiting = confirm.waiting.lock().unwrap();
            if waiting.as_ref().is_some_and(|(d, _, _)| d.fingerprint == fingerprint) {
                waiting.take();
            }
        }
        match decision {
            Some(true) => {
                trust.add(&fingerprint, name.trim()).map_err(no)?;
                record(ConnectionEvent::new("paired", name.trim(), &fingerprint, &address));
            }
            other => {
                let message = if other.is_some() { "pairing was declined on the computer" } else if nobody { "Nobody is signed in on this PC to accept the device" } else { "nobody confirmed the pairing on the computer in time" };
                let mut event = ConnectionEvent::new("refused", name.trim(), &fingerprint, &address);
                event.reason = message.into();
                record(event);
                let _ = write_message(&mut send, &Message::Error { code: "not_paired".into(), message: message.into() }).await;
                let _ = send.finish();
                // (Time for the reason to arrive before the connection goes.)
                tokio::time::sleep(Duration::from_millis(200)).await;
                conn.close(CLOSE_REVOKED.into(), message.as_bytes());
                return Err(no(anyhow::anyhow!("a new device was not confirmed")));
            }
        }
    }
    if fresh {
        record(ConnectionEvent::new("paired", name.trim(), &fingerprint, &address));
    }
    // What this device may do: its features are the host's less what it isn't allowed.
    // A paired device must also give a code from an authenticator app, when that is on and the
    // device was not told "don't ask again" (docs/PROTOCOL.md, "Second factor").
    let factor = state.factor.required_for(&fingerprint, second_factor::now());
    if factor && state.factor.is_refused(&fingerprint, second_factor::now()) {
        let message = "Too many wrong codes. Try again in 10 minutes.";
        let mut event = ConnectionEvent::new("refused", name.trim(), &fingerprint, &address);
        event.reason = message.into();
        record(event);
        let _ = write_message(&mut send, &Message::Error { code: "second_factor".into(), message: message.into() }).await;
        let _ = send.finish();
        tokio::time::sleep(Duration::from_millis(200)).await;
        return Err(no(anyhow::anyhow!("the device is refused for wrong second-factor codes")));
    }
    let mut features = gate.get().filter(&features);
    if let Some((_, control)) = &join {
        features = join::features(features, *control);
    }
    // Parity only where both sides said so.
    let parity = parity && features.iter().any(|f| f == "parity");
    write_message(&mut send, &Message::HostHello { proto: remoteman_proto::PROTO, name: options.name.clone(), os: options.os.clone(), features, second_factor: factor.then(|| "totp".to_owned()) }).await.map_err(no)?;
    // The list of trusted devices shows the name the device gives itself.
    if !name.trim().is_empty() {
        let _ = trust.rename(&fingerprint, name.trim());
    }
    if proto != remoteman_proto::PROTO {
        let message = format!("this host speaks protocol {}", remoteman_proto::PROTO);
        let _ = write_message(&mut send, &Message::Error { code: "unsupported".into(), message }).await;
        return Err(no(anyhow::anyhow!("the client speaks protocol {proto}")));
    }

    // Nothing more is sent, and the handler hears nothing, until the code is right.
    if factor {
        ask_second_factor(&mut send, &mut recv, state, &gate.fingerprint, name.trim(), &address, &record).await.map_err(no)?;
    }

    // A device that joins another's session has no handler session of its own.
    if let Some((lead, control)) = join {
        return join::serve(conn, send, recv, &lead, control, name.trim().to_owned(), &decoders, parity, calls, gate, address, &record).await;
    }

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Out>(4);
    let (cursor_tx, mut cursor_rx) = tokio::sync::mpsc::unbounded_channel::<CursorMsg>();
    let link = Link { frames: frame_tx, cursor: cursor_tx, clock, second: false };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let client_name = name.trim().to_owned();
    let info = ClientInfo { name, fingerprint, display, decoders, cursor, unlock, attach_only: way_in.as_deref() == Some("attach"), logged_in_here: false };
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
            let mut event = ConnectionEvent::new("refused", &client_name, &gate.fingerprint, &address);
            event.reason = message.clone();
            record(event);
            // The handler refused, so it holds nothing for this client.
            return Err(no(anyhow::anyhow!(message)));
        }
        Err(_) => return Err(no(anyhow::anyhow!("the host is shutting down"))),
    };
    record(ConnectionEvent::new("connected", &client_name, &gate.fingerprint, &address));
    // However this session ends, it is logged once as having left.
    let _left = LeftLog::new(state.clone(), conn.clone(), client_name, gate.fingerprint.clone(), address.clone(), false);
    let yes = |e: anyhow::Error| (true, e);
    // Counts up each time the picture starts afresh.
    let epoch = std::sync::atomic::AtomicU32::new(1);
    let now = || epoch.load(std::sync::atomic::Ordering::Relaxed) as u16;
    let first_stream = Message::Stream { epoch: now(), mode: accept.mode, width: accept.width, height: accept.height, refresh: accept.refresh, colour: accept.colour, index: 0, sdr_white: accept.sdr_white, peak: accept.peak };
    write_message(&mut send, &first_stream).await.map_err(yes)?;
    // From here devices may join this session; it ends for them when it ends.
    let lead = Arc::new(Lead::new(gate.fingerprint.clone(), first_stream));
    *lead_slot.lock().unwrap() = Some(lead.clone());
    let _lead_end = LeadEnd { lead: lead.clone(), slot: lead_slot.clone() };
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
                    lead.restart(&m);
                    write_message(&mut *send.lock().await, &m).await?;
                    continue;
                }
            };
            lead.frame(&f.data, f.key, f.capture_us, now(), number);
            let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
            // With parity the data is cut a little smaller, and a copy sent later is cut so too.
            let (datagrams, cut) = if parity {
                (remoteman_proto::video::fragment_with_parity(&f.data, f.key, f.recovery, now(), number, f.capture_us, max, remoteman_proto::video::PARITY_GROUP), remoteman_proto::video::parity_cut(max))
            } else {
                (fragment(&f.data, f.key, f.recovery, now(), number, f.capture_us, max), max)
            };
            for d in datagrams {
                sent = sent.wrapping_add(1);
                if drop_one_in.is_some_and(|n| sent % n == 0) {
                    continue;
                }
                conn.send_datagram_wait(d.into()).await?;
            }
            if f.key || f.recovery {
                answer.store(number.wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
            }
            last[0] = Some((f, now(), number, tokio::time::Instant::now() + REPEAT_AFTER, cut));
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
                    // What the device isn't allowed is dropped here, whatever it was told.
                    if message_allowed(&m, &gate.get()) {
                        let _ = calls.send(Call::Message(m));
                    }
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
                    lead.cursor_shape(&m);
                    write_message(&mut *send.lock().await, &m).await?;
                }
                CursorMsg::At { x, y, visible } => {
                    let at = CursorAt { epoch: now(), visible, x, y };
                    if last != Some(at) {
                        last = Some(at);
                        let w = at.write();
                        lead.cursor_at(&w);
                        let _ = conn.send_datagram(w.into());
                    }
                }
                CursorMsg::Message(m) => {
                    if matches!(m, Message::SoundFormat { .. }) {
                        lead.sound_format(&m);
                    }
                    write_message(&mut *send.lock().await, &m).await?
                }
                CursorMsg::Sound(packet) => {
                    let w = packet.write();
                    lead.sound(&w);
                    let _ = conn.send_datagram(w.into());
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
            let (calls, room, id, gate) = (calls.clone(), room.clone(), blob_id, gate.clone());
            // Each stream is read by a task of its own from its very first byte: a stream is
            // announced as soon as a later one carries data, so the input stream can turn up
            // here long before the client has typed anything.
            readers.spawn(async move {
                let mut first = [0u8; 1];
                if stream.read_exact(&mut first).await.is_err() {
                    return;
                }
                if first[0] >= media::BLOB_FIRST {
                    // A clipboard or a file the device isn't allowed to send is turned away.
                    let p = gate.get();
                    if (first[0] == media::BLOB_CLIPBOARD && !p.clipboard) || (first[0] == media::BLOB_FILE && !p.files) {
                        let _ = stream.stop(2u32.into());
                        return;
                    }
                    blob::receive(stream, first[0], id, move |event| {
                        let (calls, room) = (calls.clone(), room.clone());
                        async move {
                            let held = room.acquire_owned().await.ok();
                            calls.send(Call::Blob(event, held)).is_ok()
                        }
                    })
                    .await;
                } else {
                    let _ = read_input(stream, first[0], &calls, &gate).await;
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
                Some(&media::KIND_MIC) if !gate.get().mic => {}
                Some(&media::KIND_CAMERA) if !gate.get().camera => {}
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

pub(crate) async fn read_input(mut stream: RecvStream, first: u8, calls: &mpsc::Sender<Call>, gate: &Gate) -> Result<()> {
    // `first` was read off the stream to see what it was.
    let mut buf = vec![first];
    let mut chunk = [0u8; 4096];
    while let Some(n) = stream.read(&mut chunk).await? {
        buf.extend_from_slice(&chunk[..n]);
        let mut gone = false;
        // A view-only device's events (of every kind) are read and thrown away.
        let input = gate.get().input;
        take_events(&mut buf, |event| {
            if input {
                gone |= calls.send(Call::Input(event)).is_err();
            }
        })?;
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

    /// The fingerprint is made once and then read back: the same however often it is asked,
    /// and the SHA-256 of the certificate kept where the host keeps it.
    #[test]
    fn fingerprint_is_made_once_and_kept() {
        let state = std::env::temp_dir().join(format!("rm-host-fingerprint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let first = fingerprint(&state).unwrap();
        let again = fingerprint(&state).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let cert = std::fs::read(state.join("host").join("cert.der")).unwrap();
        assert_eq!(first, remoteman_transport::fingerprint(&cert));
        std::fs::remove_dir_all(&state).unwrap();
    }
}
