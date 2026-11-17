//! The client role of the remoteman core. It owns the connection: pairing, the hello, putting
//! video back together, asking for keyframes after a loss, and the clock exchange. The app
//! around it decodes and shows the frames it is handed, and passes input in.

pub mod ffi;

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
pub use remoteman_proto::control::{Chroma, Codec, Display, Mode};
use remoteman_proto::control::Message;
pub use remoteman_proto::input::{Button, Event, TouchPhase};
pub use remoteman_proto::keys;
use remoteman_proto::video::Reassembler;
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
    /// The connection is over.
    fn closed(&mut self, reason: &str);
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Frames that never arrived whole.
    pub lost: u32,
    /// Keyframes asked for because of them.
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
        let (endpoint, conn, fingerprint) = connect(addr, &identity, expect).await?;
        if !known.exists() {
            std::fs::create_dir_all(known.parent().unwrap())?;
            std::fs::write(&known, &fingerprint)?;
        }

        let clock = Instant::now();
        let now_us = move || clock.elapsed().as_micros() as u64;
        let (mut send, mut recv) = conn.open_bi().await?;
        write_message(&mut send, &Message::ClientHello { proto: remoteman_proto::PROTO, name: o.name, display: o.display, decoders: o.decoders }).await?;
        let (host_name, host_os, host_features) = loop {
            match read_message(&mut recv).await?.context("the host closed the connection")? {
                Message::HostHello { name, os, features, .. } => break (name, os, features),
                Message::Error { code, message } => bail!("the host refused: {code}: {message}"),
                _ => {}
            }
        };

        let counters = Arc::new(Counters::default());
        let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::channel::<Message>(16);
        let (epoch_tx, mut epoch_rx) = tokio::sync::mpsc::unbounded_channel::<StreamInfo>();
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
                        let _ = epoch_tx.send(StreamInfo { epoch, mode, width, height, refresh, colour });
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
            let (mut started, mut waiting_key) = (false, true);
            let reason = loop {
                tokio::select! {
                    biased;
                    Some(reason) = closed_rx.recv() => break reason,
                    Some(info) = epoch_rx.recv() => {
                        r.reset(info.epoch);
                        (started, waiting_key) = (true, true);
                        handler.stream(&info);
                    }
                    d = video.read_datagram() => {
                        let d = match d {
                            Ok(d) => d,
                            Err(e) => break e.to_string(),
                        };
                        if !started {
                            continue;
                        }
                        let frame = match r.push(&d) {
                            Ok(Some(f)) => f,
                            Ok(None) => continue,
                            Err(e) => break format!("bad video datagram: {e}"),
                        };
                        let gone = r.take_lost();
                        if gone > 0 {
                            c.lost.fetch_add(gone, Ordering::Relaxed);
                            waiting_key = true;
                        }
                        if waiting_key && !frame.key {
                            // Later frames lean on the lost one: ask for a fresh start.
                            if ctl_tx.try_send(Message::Keyframe).is_ok() {
                                c.requests.fetch_add(1, Ordering::Relaxed);
                            }
                            continue;
                        }
                        waiting_key = false;
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
