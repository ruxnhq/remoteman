//! The host role split over two processes. A system daemon owns the port and the client's
//! connection ([`crate::host::Host`], with a handler that passes everything on), and the host
//! program proper, the agent inside a desktop session, makes the picture. They talk over a Unix
//! socket in the messages here. The daemon can move a connected client from one agent to
//! another (the login screen's to the session's) without the client reconnecting.
//!
//! Every message is a `u32` length (little-endian, of what follows), a `u8` kind, and a body:
//! JSON for the rare ones, raw bytes for frames, cursor and input.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use remoteman_proto::input::Event;

use crate::CursorShape;
use crate::host::{Accept, ClientInfo, CursorMsg, HostHandler, Link, Out};

/// Where the daemon listens for agents: `/run/remoteman/agent.sock` for the system service,
/// and under the user's runtime folder for one run by hand.
pub fn socket_path(system: bool) -> std::path::PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !system => std::path::PathBuf::from(dir).join("remoteman/agent.sock"),
        _ => "/run/remoteman/agent.sock".into(),
    }
}

/// The largest message taken: a keyframe of a large display fits many times over.
const LIMIT: usize = 64 << 20;

/// Daemon to agent.
#[derive(Debug)]
pub enum ToAgent {
    /// A client is yours: make its display and answer with `Accepted` or `Refused`, then send
    /// frames. `elapsed_us` is the client's session clock at the moment of sending.
    Client { info: ClientInfo, elapsed_us: u64 },
    Keyframe,
    Lost { first: u32, last: u32 },
    Input(Event),
    /// The client is gone, or was given to another agent: stop and take the display away.
    Left,
}

/// Agent to daemon.
pub enum ToDaemon {
    /// The first message: what this agent can do (`video`, `input`, ...).
    Hello { features: Vec<String> },
    Accepted(Accept),
    Refused(String),
    Frame { key: bool, recovery: bool, capture_us: u64, data: Vec<u8> },
    CursorShape(CursorShape),
    CursorAt { x: f32, y: f32, visible: bool },
    /// For the daemon's own command (`remotemand ctl ...`), not from agents.
    Control(String),
}

fn put(w: &mut impl Write, kind: u8, parts: &[&[u8]]) -> io::Result<()> {
    let len: usize = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut head = [0u8; 5];
    head[..4].copy_from_slice(&(len as u32).to_le_bytes());
    head[4] = kind;
    w.write_all(&head)?;
    for p in parts {
        w.write_all(p)?;
    }
    w.flush()
}

/// The next message's kind and body; `None` when the other side has closed.
fn get(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad message length"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let kind = body.remove(0);
    Ok(Some((kind, body)))
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

impl ToAgent {
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            ToAgent::Client { info, elapsed_us } => {
                let json = serde_json::to_vec(info)?;
                put(w, 0x11, &[&elapsed_us.to_le_bytes(), &json])
            }
            ToAgent::Keyframe => put(w, 0x12, &[]),
            ToAgent::Lost { first, last } => put(w, 0x13, &[&first.to_le_bytes(), &last.to_le_bytes()]),
            ToAgent::Input(event) => {
                let mut bytes = Vec::new();
                event.write(&mut bytes);
                put(w, 0x14, &[&bytes])
            }
            ToAgent::Left => put(w, 0x15, &[]),
        }
    }

    pub fn read(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((kind, b)) = get(r)? else { return Ok(None) };
        Ok(Some(match kind {
            0x11 if b.len() > 8 => ToAgent::Client { elapsed_us: u64::from_le_bytes(b[..8].try_into().unwrap()), info: serde_json::from_slice(&b[8..])? },
            0x12 => ToAgent::Keyframe,
            0x13 if b.len() == 8 => ToAgent::Lost { first: u32::from_le_bytes(b[..4].try_into().unwrap()), last: u32::from_le_bytes(b[4..].try_into().unwrap()) },
            0x14 => match Event::parse(&b) {
                Ok(Some((event, _))) => ToAgent::Input(event),
                _ => return Err(bad("bad input event")),
            },
            0x15 => ToAgent::Left,
            _ => return Err(bad("unknown message for an agent")),
        }))
    }
}

impl ToDaemon {
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        match self {
            ToDaemon::Hello { features } => put(w, 0x01, &[&serde_json::to_vec(features)?]),
            ToDaemon::Accepted(accept) => put(w, 0x02, &[&serde_json::to_vec(accept)?]),
            ToDaemon::Refused(why) => put(w, 0x03, &[why.as_bytes()]),
            ToDaemon::Frame { key, recovery, capture_us, data } => put(w, 0x04, &[&[*key as u8 | (*recovery as u8) << 1], &capture_us.to_le_bytes(), data]),
            ToDaemon::CursorShape(s) => put(w, 0x05, &[&s.width.to_le_bytes(), &s.height.to_le_bytes(), &s.hot_x.to_le_bytes(), &s.hot_y.to_le_bytes(), &s.rgba]),
            ToDaemon::CursorAt { x, y, visible } => put(w, 0x06, &[&x.to_le_bytes(), &y.to_le_bytes(), &[*visible as u8]]),
            ToDaemon::Control(text) => put(w, 0x20, &[text.as_bytes()]),
        }
    }

    pub fn read(r: &mut impl Read) -> io::Result<Option<Self>> {
        let Some((kind, b)) = get(r)? else { return Ok(None) };
        let text = |b: Vec<u8>| String::from_utf8(b).map_err(|_| bad("not text"));
        Ok(Some(match kind {
            0x01 => ToDaemon::Hello { features: serde_json::from_slice(&b)? },
            0x02 => ToDaemon::Accepted(serde_json::from_slice(&b)?),
            0x03 => ToDaemon::Refused(text(b)?),
            0x04 if b.len() >= 9 => ToDaemon::Frame { key: b[0] & 1 != 0, recovery: b[0] & 2 != 0, capture_us: u64::from_le_bytes(b[1..9].try_into().unwrap()), data: b[9..].to_vec() },
            0x05 if b.len() >= 16 => {
                let n = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
                ToDaemon::CursorShape(CursorShape { width: n(0), height: n(4), hot_x: n(8) as i32, hot_y: n(12) as i32, rgba: b[16..].to_vec() })
            }
            0x06 if b.len() == 9 => ToDaemon::CursorAt { x: f32::from_le_bytes(b[..4].try_into().unwrap()), y: f32::from_le_bytes(b[4..8].try_into().unwrap()), visible: b[8] != 0 },
            0x20 => ToDaemon::Control(text(b)?),
            _ => return Err(bad("unknown message for the daemon")),
        }))
    }
}

/// The agent's side: serve whatever clients the daemon at the other end of `socket` hands
/// over, with the same handler a standalone host would use. Returns when the daemon goes away.
pub fn run_agent(socket: UnixStream, features: Vec<String>, mut handler: impl HostHandler) -> Result<()> {
    let mut reader = socket.try_clone().context("the daemon's socket")?;
    let writer = Arc::new(Mutex::new(socket));
    ToDaemon::Hello { features }.write(&mut *writer.lock().unwrap()).context("say hello to the daemon")?;

    // While a client is being served: the threads carrying its frames and cursor to the daemon.
    let mut carrying: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut serving = false;
    let stop = |handler: &mut dyn HostHandler, serving: &mut bool, carrying: &mut Vec<std::thread::JoinHandle<()>>| {
        if std::mem::take(serving) {
            // Ends the picture, which drops every copy of the link, which ends the threads: so
            // nothing of this client can follow what the daemon is told next.
            handler.left();
            for t in carrying.drain(..) {
                let _ = t.join();
            }
        }
    };
    loop {
        let message = match ToAgent::read(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) => {
                stop(&mut handler, &mut serving, &mut carrying);
                bail!("the daemon's socket: {e}");
            }
        };
        match message {
            ToAgent::Client { info, elapsed_us } => {
                stop(&mut handler, &mut serving, &mut carrying);
                let (link, mut end) = Link::pipe(elapsed_us);
                match handler.connected(&info, link) {
                    Ok(accept) => {
                        // Told before any frame: frames wait in the link until now.
                        ToDaemon::Accepted(accept).write(&mut *writer.lock().unwrap())?;
                        serving = true;
                        let w = writer.clone();
                        carrying.push(std::thread::Builder::new().name("to daemon: frames".into()).spawn(move || {
                            while let Some(out) = end.frames.blocking_recv() {
                                let Out::Frame(f) = out else { continue };
                                let m = ToDaemon::Frame { key: f.key, recovery: f.recovery, capture_us: f.capture_us, data: f.data };
                                if m.write(&mut *w.lock().unwrap()).is_err() {
                                    break;
                                }
                            }
                        })?);
                        let w = writer.clone();
                        carrying.push(std::thread::Builder::new().name("to daemon: cursor".into()).spawn(move || {
                            while let Some(c) = end.cursor.blocking_recv() {
                                let m = match c {
                                    CursorMsg::Shape(s) => ToDaemon::CursorShape(s),
                                    CursorMsg::At { x, y, visible } => ToDaemon::CursorAt { x, y, visible },
                                };
                                if m.write(&mut *w.lock().unwrap()).is_err() {
                                    break;
                                }
                            }
                        })?);
                    }
                    Err(why) => ToDaemon::Refused(why).write(&mut *writer.lock().unwrap())?,
                }
            }
            ToAgent::Keyframe if serving => handler.keyframe(),
            ToAgent::Lost { first, last } if serving => handler.lost(first, last),
            ToAgent::Input(event) if serving => handler.input(event),
            ToAgent::Left => stop(&mut handler, &mut serving, &mut carrying),
            _ => {}
        }
    }
    stop(&mut handler, &mut serving, &mut carrying);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chroma, Codec, Display, Mode};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Sends numbered frames as fast as the link takes them, from inside `connected` on.
    struct Counting {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        inputs: Arc<Mutex<Vec<Event>>>,
    }

    impl HostHandler for Counting {
        fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
            if client.name == "unwelcome" {
                return Err("no".into());
            }
            self.stop = Arc::new(AtomicBool::new(false));
            let stop = self.stop.clone();
            self.thread = Some(std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(Ordering::Relaxed) && link.send_frame(&n.to_le_bytes(), n == 0, 7) {
                    n += 1;
                }
            }));
            // Frames are already waiting in the link by the time this returns.
            std::thread::sleep(std::time::Duration::from_millis(30));
            Ok(Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width: client.display.width, height: 2, refresh: 120, colour: "bt709".into() })
        }
        fn keyframe(&mut self) {}
        fn input(&mut self, event: Event) {
            self.inputs.lock().unwrap().push(event);
        }
        fn left(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn client(name: &str, width: u32) -> ToAgent {
        let display = Display { width, height: 2, scale: 1.0, refresh: 120, placement: Default::default() };
        ToAgent::Client { info: ClientInfo { name: name.into(), fingerprint: "f".into(), display, decoders: vec![], cursor: false }, elapsed_us: 5 }
    }

    /// The next `count` frames' numbers, having skipped nothing else.
    fn frames(daemon: &mut UnixStream, count: u32) -> Vec<u32> {
        (0..count)
            .map(|_| match ToDaemon::read(daemon).unwrap() {
                Some(ToDaemon::Frame { data, capture_us: 7, .. }) => u32::from_le_bytes(data[..4].try_into().unwrap()),
                _ => panic!("expected a frame"),
            })
            .collect()
    }

    #[test]
    fn an_agent_serves_clients_in_order() {
        let (agent, mut daemon) = UnixStream::pair().unwrap();
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let handler = Counting { stop: Arc::default(), thread: None, inputs: inputs.clone() };
        let running = std::thread::spawn(move || run_agent(agent, vec!["video".into()], handler));
        assert!(matches!(ToDaemon::read(&mut daemon).unwrap(), Some(ToDaemon::Hello { features }) if features == ["video"]));

        // A refusal comes back as one, and nothing else.
        client("unwelcome", 2).write(&mut daemon).unwrap();
        assert!(matches!(ToDaemon::read(&mut daemon).unwrap(), Some(ToDaemon::Refused(why)) if why == "no"));

        // Accepted first, then frames from 0 with none missing, though they were made before
        // the acceptance was written.
        client("one", 64).write(&mut daemon).unwrap();
        assert!(matches!(ToDaemon::read(&mut daemon).unwrap(), Some(ToDaemon::Accepted(a)) if a.width == 64));
        assert_eq!(frames(&mut daemon, 50), (0..50).collect::<Vec<_>>());
        ToAgent::Input(Event::Key { usage: 4, down: true }).write(&mut daemon).unwrap();

        // A second client takes the first one's place: after its acceptance, only its frames,
        // from 0 again. (Whatever the first still had in flight comes before the acceptance.)
        client("two", 128).write(&mut daemon).unwrap();
        loop {
            match ToDaemon::read(&mut daemon).unwrap() {
                Some(ToDaemon::Frame { .. }) => {}
                Some(ToDaemon::Accepted(a)) => {
                    assert_eq!(a.width, 128);
                    break;
                }
                _ => panic!("expected frames, then an acceptance"),
            }
        }
        assert_eq!(frames(&mut daemon, 50), (0..50).collect::<Vec<_>>());
        assert_eq!(*inputs.lock().unwrap(), [Event::Key { usage: 4, down: true }]);

        // The daemon going away ends it cleanly. Keep reading meanwhile, as the daemon would,
        // so the agent isn't stuck writing a frame nobody takes.
        ToAgent::Left.write(&mut daemon).unwrap();
        daemon.shutdown(std::net::Shutdown::Write).unwrap();
        while let Ok(Some(_)) = ToDaemon::read(&mut daemon) {}
        running.join().unwrap().unwrap();
    }
}
