//! The remoteman core, shared by every client and host.
//!
//! This file is the client role. It owns the connection: pairing, the hello, putting video back
//! together, asking for keyframes after a loss, and the clock exchange. The app around it
//! decodes and shows the frames it is handed, and passes input in. The host role is in
//! [`host`], and both as a C interface in [`ffi`].

pub mod ffi;
pub mod host;

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
pub use remoteman_proto::control::{Chroma, Codec, Display, Mode, Placement};
use remoteman_proto::control::Message;
pub use remoteman_proto::input::{Button, Event, TouchPhase};
pub use remoteman_proto::keys;
pub use remoteman_proto::{PORT, PROTO};
use remoteman_proto::video::{CursorAt, KIND_CURSOR, Reassembler};
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
    /// The connection is over.
    fn closed(&mut self, reason: &str);
}

enum FromHost {
    Stream(StreamInfo),
    Cursor(CursorShape),
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

pub struct Client {
    runtime: Option<tokio::runtime::Runtime>,
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
        let addr = o.host.to_socket_addrs().with_context(|| format!("look up {}", o.host))?.next().context("no such host")?;
        let identity = Identity::load_or_create(&o.state.join("client"))?;
        let known = o.state.join("client/hosts").join(o.host.replace([':', '/'], "_"));
        let expect = match std::fs::read_to_string(&known) {
            Ok(fp) => Expect::Fingerprint(fp.trim().to_owned()),
            Err(_) if o.pair => Expect::Anyone,
            Err(_) => bail!("{} isn't paired yet", o.host),
        };
        let o_drop = o.test_drop_one_in.filter(|n| *n > 1);
        let o_delay = o.test_delay_lost_ms.filter(|ms| *ms > 0);
        let (endpoint, conn, fingerprint) = connect(addr, &identity, expect).await?;
        if !known.exists() {
            std::fs::create_dir_all(known.parent().unwrap())?;
            std::fs::write(&known, &fingerprint)?;
        }

        let clock = Instant::now();
        let now_us = move || clock.elapsed().as_micros() as u64;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_message(&mut send, &Message::ClientHello { proto: remoteman_proto::PROTO, name: o.name, display: o.display, decoders: o.decoders, cursor: o.cursor }).await?;
        let (host_name, host_os, host_features) = loop {
            match read_message(&mut recv).await?.context("the host closed the connection")? {
                Message::HostHello { name, os, features, .. } => break (name, os, features),
                Message::Error { code, message } => bail!("the host refused: {code}: {message}"),
                _ => {}
            }
        };

        let counters = Arc::new(Counters::default());
        let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::channel::<Message>(16);
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
                    _ => {}
                }
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
            let reason = loop {
                tokio::select! {
                    biased;
                    Some(reason) = closed_rx.recv() => break reason,
                    Some(m) = epoch_rx.recv() => match m {
                        FromHost::Stream(info) => {
                            r.reset(info.epoch);
                            (started, need_key, missed) = (true, true, None);
                            handler.stream(&info);
                        }
                        FromHost::Cursor(shape) => handler.cursor_shape(&shape),
                    },
                    d = video.read_datagram() => {
                        let d = match d {
                            Ok(d) => d,
                            Err(e) => break e.to_string(),
                        };
                        if !started {
                            continue;
                        }
                        if d.first() == Some(&KIND_CURSOR) {
                            if let Ok(c) = CursorAt::parse(&d) {
                                handler.cursor_at(c.x, c.y, c.visible);
                            }
                            continue;
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
                                    Some(first) if !need_key => Message::Lost { first, last: frame.number },
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

        Ok(Self { runtime: None, conn, _endpoint: endpoint, input, counters, host_name, host_os, host_features, host_fingerprint: fingerprint })
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
