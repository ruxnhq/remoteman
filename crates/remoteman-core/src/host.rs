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
use remoteman_proto::video::{CursorAt, fragment};
use remoteman_transport::{Connection, Identity, RecvStream, Trust, listen, peer_fingerprint, read_message, write_message};

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
}

/// A client that has said hello.
#[derive(Debug, Clone)]
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
}

/// What the host will send, in answer to a client's hello.
#[derive(Debug, Clone)]
pub struct Accept {
    pub mode: Mode,
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    /// `bt709` or `bt2020-pq`.
    pub colour: String,
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
    /// The client is gone (or was replaced): stop the picture and take the display away.
    fn left(&mut self);
}

struct Frame {
    data: Vec<u8>,
    key: bool,
    recovery: bool,
    capture_us: u64,
}

enum CursorMsg {
    Shape(CursorShape),
    At { x: f32, y: f32, visible: bool },
}

/// The way to one connected client. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Link {
    frames: tokio::sync::mpsc::Sender<Frame>,
    cursor: tokio::sync::mpsc::UnboundedSender<CursorMsg>,
    clock: Instant,
}

impl Link {
    /// This session's clock in microseconds: stamp each frame with it when it is captured.
    pub fn now_us(&self) -> u64 {
        self.clock.elapsed().as_micros() as u64
    }

    /// Send one encoded frame (HEVC Annex-B, or AV1 OBUs). Waits while the network is behind,
    /// so an encoder calling this is held back and frames get skipped before encoding, never
    /// after. Returns false once the client is gone.
    pub fn send_frame(&self, data: &[u8], key: bool, capture_us: u64) -> bool {
        self.frames.blocking_send(Frame { data: data.to_vec(), key, recovery: false, capture_us }).is_ok()
    }

    /// As [`Self::send_frame`], for the first frame after a reported loss that leans on nothing
    /// the client missed: the client picks the picture up again at it.
    pub fn send_recovery_frame(&self, data: &[u8], capture_us: u64) -> bool {
        self.frames.blocking_send(Frame { data: data.to_vec(), key: false, recovery: true, capture_us }).is_ok()
    }

    /// The cursor's image changed (for clients that draw it themselves). Zero size: no cursor.
    pub fn cursor_shape(&self, shape: CursorShape) {
        let _ = self.cursor.send(CursorMsg::Shape(shape));
    }

    /// Where the cursor is, as fractions of the display; `visible` false when it isn't on it.
    pub fn cursor_at(&self, x: f32, y: f32, visible: bool) {
        let _ = self.cursor.send(CursorMsg::At { x, y, visible });
    }
}

enum Call {
    Connected(ClientInfo, Link, tokio::sync::oneshot::Sender<Result<Accept, String>>),
    Keyframe,
    Lost(u32, u32),
    Input(Event),
    Left,
}

pub struct Host {
    runtime: Option<tokio::runtime::Runtime>,
    endpoint: remoteman_transport::Endpoint,
    trust: Arc<Trust>,
    link: Arc<Mutex<Option<Link>>>,
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
        std::thread::Builder::new().name("remoteman host".into()).spawn(move || {
            while let Ok(call) = call_rx.recv() {
                match call {
                    Call::Connected(client, link, reply) => {
                        let _ = reply.send(handler.connected(&client, link));
                    }
                    Call::Keyframe => handler.keyframe(),
                    Call::Lost(first, last) => handler.lost(first, last),
                    Call::Input(event) => handler.input(event),
                    Call::Left => handler.left(),
                }
            }
        })?;

        let link: Arc<Mutex<Option<Link>>> = Arc::default();
        runtime.spawn(accept_loop(endpoint.clone(), trust.clone(), options, calls, link.clone()));
        Ok(Self { runtime: Some(runtime), endpoint, trust, link, fingerprint: identity.fingerprint, address })
    }

    /// Let the next unknown device in (it is remembered and pairing turns itself off), or stop
    /// doing so.
    pub fn set_pairing(&self, on: bool) {
        self.trust.set_pairing(on);
    }

    pub fn pairing(&self) -> bool {
        self.trust.pairing()
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
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn accept_loop(endpoint: remoteman_transport::Endpoint, trust: Arc<Trust>, options: HostOptions, calls: mpsc::Sender<Call>, current_link: Arc<Mutex<Option<Link>>>) {
    let options = Arc::new(options);
    let mut current: Option<(tokio::task::JoinHandle<()>, Connection)> = None;
    while let Some(incoming) = endpoint.accept().await {
        // A stranger is refused inside the handshake; this only fails for them.
        let Ok(conn) = incoming.await else { continue };
        let Some(fingerprint) = peer_fingerprint(&conn) else { continue };
        if !trust.contains(&fingerprint) {
            // Only reachable while pairing.
            let _ = trust.add(&fingerprint, &conn.remote_address().ip().to_string());
            trust.set_pairing(false);
        }
        // The newest client takes over. The old session is closed and left to finish by
        // itself, so the handler hears `left` for it exactly once, and before the new `connected`.
        if let Some((old, old_conn)) = current.take() {
            old_conn.close(0u32.into(), b"another device took over");
            let _ = old.await;
        }
        let (options, calls, current_link) = (options.clone(), calls.clone(), current_link.clone());
        let session_conn = conn.clone();
        current = Some((tokio::spawn(async move {
            let started = serve(&conn, fingerprint, &options, &calls, &current_link).await;
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

/// One client's session. The flag in the result says whether the handler's `connected` ran (and
/// so whether `left` is owed).
async fn serve(conn: &Connection, fingerprint: String, options: &HostOptions, calls: &mpsc::Sender<Call>, current_link: &Mutex<Option<Link>>) -> Result<bool, (bool, anyhow::Error)> {
    let no = |e: anyhow::Error| (false, e);
    let clock = Instant::now();
    let (mut send, mut recv) = conn.accept_bi().await.context("control stream").map_err(no)?;
    let Some(Message::ClientHello { proto, name, display, decoders, cursor }) = read_message(&mut recv).await.map_err(no)? else {
        return Err(no(anyhow::anyhow!("the first message wasn't client_hello")));
    };
    write_message(&mut send, &Message::HostHello { proto: remoteman_proto::PROTO, name: options.name.clone(), os: options.os.clone(), features: options.features.clone() }).await.map_err(no)?;
    if proto != remoteman_proto::PROTO {
        let message = format!("this host speaks protocol {}", remoteman_proto::PROTO);
        let _ = write_message(&mut send, &Message::Error { code: "unsupported".into(), message }).await;
        return Err(no(anyhow::anyhow!("the client speaks protocol {proto}")));
    }

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Frame>(4);
    let (cursor_tx, mut cursor_rx) = tokio::sync::mpsc::unbounded_channel::<CursorMsg>();
    let link = Link { frames: frame_tx, cursor: cursor_tx, clock };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let info = ClientInfo { name, fingerprint, display, decoders, cursor };
    // Reachable through `Host::link` before the handler hears of the client, so nothing it
    // sends from the start is lost.
    *current_link.lock().unwrap() = Some(link.clone());
    calls.send(Call::Connected(info, link, reply_tx)).ok().context("the host is shutting down").map_err(no)?;
    let accept = match reply_rx.await {
        Ok(Ok(a)) => a,
        Ok(Err(message)) => {
            current_link.lock().unwrap().take();
            let _ = write_message(&mut send, &Message::Error { code: "failed".into(), message: message.clone() }).await;
            // The handler refused, so it holds nothing for this client.
            return Err(no(anyhow::anyhow!(message)));
        }
        Err(_) => return Err(no(anyhow::anyhow!("the host is shutting down"))),
    };
    let yes = |e: anyhow::Error| (true, e);
    const EPOCH: u16 = 1;
    let (width, height) = (accept.width.max(1) as f32, accept.height.max(1) as f32);
    write_message(&mut send, &Message::Stream { epoch: EPOCH, mode: accept.mode, width: accept.width, height: accept.height, refresh: accept.refresh, colour: accept.colour })
        .await
        .map_err(yes)?;

    let video = async {
        let mut number = 0u32;
        while let Some(f) = frame_rx.recv().await {
            let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
            for d in fragment(&f.data, f.key, f.recovery, EPOCH, number, f.capture_us, max) {
                conn.send_datagram_wait(d.into()).await?;
            }
            number = number.wrapping_add(1);
        }
        // The handler dropped its link without the client leaving: nothing more to send.
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    // Two things write to the control stream: answers to the client, and cursor shapes.
    let send = tokio::sync::Mutex::new(send);
    let control = async {
        while let Some(m) = read_message(&mut recv).await? {
            match m {
                Message::Keyframe => {
                    let _ = calls.send(Call::Keyframe);
                }
                Message::Lost { first, last } => {
                    let _ = calls.send(Call::Lost(first, last));
                }
                Message::Ping { id, client_us } => write_message(&mut *send.lock().await, &Message::Pong { id, client_us, host_us: clock.elapsed().as_micros() as u64 }).await?,
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
                    let at = CursorAt { epoch: EPOCH, visible, x, y };
                    if last != Some(at) {
                        last = Some(at);
                        let _ = conn.send_datagram(at.write().into());
                    }
                }
            }
        }
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    let input = async {
        let stream = conn.accept_uni().await?;
        read_input(stream, calls).await
    };
    let _ = (width, height);
    let result = tokio::select! {
        r = video => r.context("video"),
        r = control => r.context("control"),
        r = cursor => r.context("cursor"),
        r = input => r.context("input"),
        _ = conn.closed() => Ok(()),
    };
    // The client leaving ends whichever part was waiting on it; that isn't a failure.
    if conn.close_reason().is_some() { Ok(true) } else { result.map(|()| true).map_err(yes) }
}

async fn read_input(mut stream: RecvStream, calls: &mpsc::Sender<Call>) -> Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Some(n) = stream.read(&mut chunk).await? {
        buf.extend_from_slice(&chunk[..n]);
        let mut at = 0;
        while let Some((event, used)) = Event::parse(&buf[at..])? {
            at += used;
            if calls.send(Call::Input(event)).is_err() {
                bail!("the host is shutting down");
            }
        }
        buf.drain(..at);
    }
    // The input stream closing isn't the session ending: wait for the connection to say so.
    std::future::pending::<()>().await;
    Ok(())
}
