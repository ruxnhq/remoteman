//! More than one device at once (docs/PROTOCOL.md, "More than one device"). The first device
//! of a session is the lead: it has the handler as before. A device that connects while the lead
//! is up, with the host's `second_device` set to `watch` or `control`, joins: the lead's
//! session fans its picture, cursor and sound out to it, inside the core. The handler is never
//! told of a joiner as a session; only a joiner's keyframe request and (in `control`) its input
//! reach it.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result};
use remoteman_proto::control::{Message, Mode};
use remoteman_proto::media;
use remoteman_proto::video::{PARITY_GROUP, fragment, fragment_with_parity, parity_cut};
use remoteman_transport::{Connection, RecvStream, SendStream, read_message, write_message};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::host::{Call, ConnectionEvent, Gate, LeftLog, read_input, wait_to_send};

/// What a host does when a device connects while another is using it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SecondDevice {
    /// The new device ends the first one's session (the default).
    #[default]
    TakeOver,
    /// It joins and sees what the first sees; its input is dropped.
    Watch,
    /// It joins, and its input is used too.
    Control,
}

impl SecondDevice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TakeOver => "take_over",
            Self::Watch => "watch",
            Self::Control => "control",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "take_over" | "take-over" => Some(Self::TakeOver),
            "watch" => Some(Self::Watch),
            "control" => Some(Self::Control),
            _ => None,
        }
    }

    pub fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(Self::TakeOver),
            1 => Some(Self::Watch),
            2 => Some(Self::Control),
            _ => None,
        }
    }
}

/// How much picture may wait for one joiner before frames for it are dropped (it then asks
/// for a keyframe, as after any loss).
const QUEUE_LIMIT: usize = 4 << 20;
/// A joiner's keyframe requests reach the handler this far apart, at most, across joiners.
const KEYFRAME_EVERY: Duration = Duration::from_millis(500);
/// A frame that nothing follows is sent once more after this (as for the lead).
const REPEAT_AFTER: Duration = Duration::from_millis(100);

pub(crate) struct JFrame {
    pub data: Vec<u8>,
    pub key: bool,
    pub capture_us: u64,
    pub epoch: u16,
    pub number: u32,
}

pub(crate) enum JMsg {
    /// On the control stream; a `stream` message starts the picture afresh.
    Control(Message),
    Frame(Arc<JFrame>),
    /// A datagram as it is: the cursor's position, a packet of sound.
    Datagram(Vec<u8>),
}

struct Joiner {
    id: u64,
    fingerprint: String,
    conn: Connection,
    tx: UnboundedSender<JMsg>,
    queued: Arc<AtomicUsize>,
    sound: Arc<AtomicBool>,
}

struct Inner {
    closed: bool,
    stream: Message,
    shape: Option<Message>,
    sound_format: Option<Message>,
    joiners: Vec<Joiner>,
    next_id: u64,
}

/// The lead's session, as joiners see it.
pub(crate) struct Lead {
    pub fingerprint: String,
    inner: Mutex<Inner>,
    last_key: Mutex<Option<std::time::Instant>>,
}

pub(crate) type LeadSlot = Arc<Mutex<Option<Arc<Lead>>>>;

pub(crate) enum NotJoined {
    /// The lead's picture is one the joiner can't decode.
    CannotShow,
    /// The lead has gone.
    Gone,
}

impl Lead {
    pub fn new(fingerprint: String, stream: Message) -> Self {
        Self { fingerprint, inner: Mutex::new(Inner { closed: false, stream, shape: None, sound_format: None, joiners: Vec::new(), next_id: 0 }), last_key: Mutex::new(None) }
    }

    pub fn is_open(&self) -> bool {
        !self.inner.lock().unwrap().closed
    }

    /// The lead's session is over: joiners are let go (they are told).
    pub fn end(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.closed = true;
        inner.joiners.clear();
    }

    /// The fingerprints of the joined devices.
    pub fn joined(&self) -> Vec<String> {
        self.inner.lock().unwrap().joiners.iter().map(|j| j.fingerprint.clone()).collect()
    }

    /// Close the joined connections of this device.
    pub fn close_device(&self, fingerprint: &str, code: u32, reason: &[u8]) {
        for j in self.inner.lock().unwrap().joiners.iter().filter(|j| j.fingerprint == fingerprint) {
            j.conn.close(code.into(), reason);
        }
    }

    /// The picture starts afresh.
    pub fn restart(&self, stream: &Message) {
        let mut inner = self.inner.lock().unwrap();
        inner.stream = stream.clone();
        send_all(&mut inner, || JMsg::Control(stream.clone()));
    }

    pub fn frame(&self, data: &[u8], key: bool, capture_us: u64, epoch: u16, number: u32) {
        let mut inner = self.inner.lock().unwrap();
        if inner.joiners.is_empty() {
            return;
        }
        let f = Arc::new(JFrame { data: data.to_vec(), key, capture_us, epoch, number });
        inner.joiners.retain(|j| {
            if j.queued.load(Ordering::Relaxed) > QUEUE_LIMIT {
                return true;
            }
            j.queued.fetch_add(f.data.len(), Ordering::Relaxed);
            j.tx.send(JMsg::Frame(f.clone())).is_ok()
        });
    }

    pub fn cursor_shape(&self, m: &Message) {
        let mut inner = self.inner.lock().unwrap();
        inner.shape = Some(m.clone());
        send_all(&mut inner, || JMsg::Control(m.clone()));
    }

    pub fn sound_format(&self, m: &Message) {
        let mut inner = self.inner.lock().unwrap();
        inner.sound_format = Some(m.clone());
        send_all(&mut inner, || JMsg::Control(m.clone()));
    }

    pub fn cursor_at(&self, d: &[u8]) {
        send_all(&mut self.inner.lock().unwrap(), || JMsg::Datagram(d.to_vec()));
    }

    pub fn sound(&self, d: &[u8]) {
        let mut inner = self.inner.lock().unwrap();
        inner.joiners.retain(|j| !j.sound.load(Ordering::Relaxed) || j.tx.send(JMsg::Datagram(d.to_vec())).is_ok());
    }

    fn join(&self, conn: &Connection, fingerprint: &str, decoders: &[Mode], tx: UnboundedSender<JMsg>, queued: Arc<AtomicUsize>, sound: Arc<AtomicBool>) -> Result<u64, NotJoined> {
        let mut inner = self.inner.lock().unwrap();
        if inner.closed {
            return Err(NotJoined::Gone);
        }
        if let Message::Stream { mode, .. } = &inner.stream {
            if !decoders.contains(mode) {
                return Err(NotJoined::CannotShow);
            }
        }
        // One connection per device: a second of the same takes the first's place.
        inner.joiners.retain(|j| {
            if j.fingerprint == fingerprint {
                j.conn.close(0u32.into(), b"another device took over");
                false
            } else {
                true
            }
        });
        let _ = tx.send(JMsg::Control(inner.stream.clone()));
        for m in [&inner.sound_format, &inner.shape].into_iter().flatten() {
            let _ = tx.send(JMsg::Control(m.clone()));
        }
        inner.next_id += 1;
        let id = inner.next_id;
        inner.joiners.push(Joiner { id, fingerprint: fingerprint.to_owned(), conn: conn.clone(), tx, queued, sound });
        Ok(id)
    }

    fn leave(&self, id: u64) {
        self.inner.lock().unwrap().joiners.retain(|j| j.id != id);
    }

    /// A joiner needs a keyframe: the handler is asked, at most once in `KEYFRAME_EVERY` for
    /// all joiners. With `patient` the request waits for its turn instead of being dropped.
    async fn request_keyframe(&self, calls: &mpsc::Sender<Call>, patient: bool) {
        loop {
            let wait = {
                let mut last = self.last_key.lock().unwrap();
                match *last {
                    Some(t) if t.elapsed() < KEYFRAME_EVERY => Some(KEYFRAME_EVERY - t.elapsed()),
                    _ => {
                        *last = Some(std::time::Instant::now());
                        None
                    }
                }
            };
            match wait {
                None => {
                    let _ = calls.send(Call::Keyframe);
                    return;
                }
                Some(w) if patient => tokio::time::sleep(w).await,
                Some(_) => return,
            }
        }
    }
}

fn send_all(inner: &mut Inner, make: impl Fn() -> JMsg) {
    inner.joiners.retain(|j| j.tx.send(make()).is_ok());
}

/// Ends the lead's hold on the slot and on its joiners, however its session ends.
pub(crate) struct LeadEnd {
    pub lead: Arc<Lead>,
    pub slot: LeadSlot,
}

impl Drop for LeadEnd {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap();
        if slot.as_ref().is_some_and(|l| Arc::ptr_eq(l, &self.lead)) {
            slot.take();
        }
        drop(slot);
        self.lead.end();
    }
}

struct Leave(Arc<Lead>, u64);

impl Drop for Leave {
    fn drop(&mut self) {
        self.0.leave(self.1);
    }
}

/// What a joiner is offered: the lead's host features without what stays the lead's, and
/// without input unless the host lets joiners control and the device may.
pub(crate) fn features(mut features: Vec<String>, control: bool) -> Vec<String> {
    features.retain(|f| !matches!(f.as_str(), "clipboard" | "files" | "mic" | "camera" | "configure" | "placement" | "displays" | "mirror"));
    if !(control && features.iter().any(|f| f == "input")) {
        features.retain(|f| !matches!(f.as_str(), "input" | "pen" | "gamepad"));
    }
    features
}

/// One joined device's session, after its hello and second factor.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn serve(conn: &Connection, send: SendStream, mut recv: RecvStream, lead: &Arc<Lead>, control: bool, name: String, decoders: &[Mode], parity: bool, calls: &mpsc::Sender<Call>, gate: Gate, address: String, record: &(dyn Fn(ConnectionEvent) + Sync)) -> Result<bool, (bool, anyhow::Error)> {
    let no = |e: anyhow::Error| (false, e);
    let mut send = send;
    let (tx, mut rx): (UnboundedSender<JMsg>, UnboundedReceiver<JMsg>) = tokio::sync::mpsc::unbounded_channel();
    let queued = Arc::new(AtomicUsize::new(0));
    let sound = Arc::new(AtomicBool::new(false));
    let id = match lead.join(conn, &gate.fingerprint, decoders, tx, queued.clone(), sound.clone()) {
        Ok(id) => id,
        Err(why) => {
            let (code, message) = match why {
                NotJoined::CannotShow => ("busy", "Another device is using this computer in a way this device can't show"),
                NotJoined::Gone => ("failed", "The device that was using this computer left"),
            };
            let mut event = ConnectionEvent::new("refused", &name, &gate.fingerprint, &address);
            event.reason = message.into();
            event.joined = true;
            record(event);
            let _ = write_message(&mut send, &Message::Error { code: code.into(), message: message.into() }).await;
            let _ = send.finish();
            tokio::time::sleep(Duration::from_millis(200)).await;
            return Err(no(anyhow::anyhow!(message)));
        }
    };
    let _leave = Leave(lead.clone(), id);
    let mut event = ConnectionEvent::new("connected", &name, &gate.fingerprint, &address);
    event.joined = true;
    record(event);
    let _left = LeftLog::new(gate.state.clone(), conn.clone(), name, gate.fingerprint.clone(), address, true);
    // It starts from a keyframe, when its turn comes.
    let key = async {
        lead.request_keyframe(calls, true).await;
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    let clock = std::time::Instant::now();
    let send = tokio::sync::Mutex::new(send);
    let out = async {
        // The last frame sent, and when it goes once more if nothing follows it.
        let mut last: Option<(Arc<JFrame>, tokio::time::Instant, usize)> = None;
        // The size of the frame handed over last: the next waits until the queue is down to it.
        let mut behind = 0usize;
        loop {
            let msg = match &last {
                Some((_, due, _)) => match tokio::time::timeout_at(*due, rx.recv()).await {
                    Ok(m) => m,
                    Err(_) => {
                        let (f, _, cut) = last.take().unwrap();
                        let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
                        if cut <= max {
                            wait_to_send(conn, behind).await?;
                            behind = f.data.len();
                            for d in fragment(&f.data, f.key, false, f.epoch, f.number, f.capture_us, cut) {
                                conn.send_datagram_wait(d.into()).await?;
                            }
                        }
                        continue;
                    }
                },
                None => rx.recv().await,
            };
            let Some(msg) = msg else { return anyhow::Ok(()) };
            match msg {
                JMsg::Control(m) => {
                    if matches!(m, Message::Stream { .. }) {
                        last = None;
                    }
                    write_message(&mut *send.lock().await, &m).await?;
                }
                JMsg::Datagram(d) => {
                    let _ = conn.send_datagram(d.into());
                }
                JMsg::Frame(f) => {
                    let n = f.data.len();
                    wait_to_send(conn, behind).await?;
                    behind = n;
                    queued.fetch_sub(n, Ordering::Relaxed);
                    let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
                    // Never a recovery point: it may lean on frames this device missed and the
                    // lead's loss didn't; the device waits for the keyframe it asked for.
                    let (datagrams, cut) = if parity {
                        (fragment_with_parity(&f.data, f.key, false, f.epoch, f.number, f.capture_us, max, PARITY_GROUP), parity_cut(max))
                    } else {
                        (fragment(&f.data, f.key, false, f.epoch, f.number, f.capture_us, max), max)
                    };
                    for d in datagrams {
                        conn.send_datagram_wait(d.into()).await?;
                    }
                    last = Some((f, tokio::time::Instant::now() + REPEAT_AFTER, cut));
                }
            }
        }
    };
    let control_in = async {
        while let Some(m) = read_message(&mut recv).await? {
            match m {
                Message::Keyframe | Message::Lost { .. } => lead.request_keyframe(calls, false).await,
                Message::Ping { id, client_us } => write_message(&mut *send.lock().await, &Message::Pong { id, client_us, host_us: clock.elapsed().as_micros() as u64 }).await?,
                Message::Sound { on } => {
                    if !on || gate.get().sound {
                        sound.store(on, Ordering::Relaxed);
                    }
                }
                _ => {}
            }
        }
        anyhow::Ok(())
    };
    // Its input, when it may have any; anything else it opens is turned away.
    let streams = async {
        let mut readers = tokio::task::JoinSet::new();
        loop {
            let mut stream = conn.accept_uni().await?;
            let (calls, gate) = (calls.clone(), gate.clone());
            readers.spawn(async move {
                let mut first = [0u8; 1];
                if stream.read_exact(&mut first).await.is_err() {
                    return;
                }
                if first[0] >= media::BLOB_FIRST || !control {
                    let _ = stream.stop(2u32.into());
                } else {
                    let _ = read_input(stream, first[0], &calls, &gate).await;
                }
            });
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    };
    // `true`: the lead's session ended and so ends this.
    let ended = tokio::select! {
        r = out => r.context("video").map(|()| true),
        r = control_in => r.context("control").map(|()| false),
        r = streams => r.context("streams").map(|()| false),
        r = key => r.context("keyframe").map(|()| false),
        _ = conn.closed() => Ok(false),
    };
    match ended {
        Ok(true) if conn.close_reason().is_none() => {
            let mut send = send.lock().await;
            let _ = write_message(&mut send, &Message::Error { code: "failed".into(), message: "The device that was using this computer left".into() }).await;
            let _ = send.finish();
            drop(send);
            tokio::time::sleep(Duration::from_millis(200)).await;
            conn.close(0u32.into(), b"the device that was using this computer left");
            Ok(false)
        }
        Ok(_) => Ok(false),
        Err(_) if conn.close_reason().is_some() => Ok(false),
        Err(e) => Err((false, e)),
    }
}
