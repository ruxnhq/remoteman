//! Which agent a client's picture comes from, and carrying everything between the two.
//!
//! Frame numbers are the thread that must not break: the client's loss reports, and what the
//! agent's encoder does about them, both count frames from 0 per picture. So an agent's frames
//! reach the client from the moment its `Accepted` is read, in order, with none left out and
//! none from another agent in between. `sending` is held around each of those steps.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::{BlobEvent, BlobSender, Event, Frame, Message, Packet};
use remoteman_core::relay::{ToAgent, ToDaemon};

type Writer = Arc<Mutex<UnixStream>>;

struct Agent {
    writer: Writer,
    /// Who it runs as and its process, from the kernel.
    uid: u32,
    pid: i32,
    features: Vec<String>,
}

/// An agent that was handed the client and hasn't answered yet.
struct Waiting {
    agent: u32,
    /// Where the answer goes when this is the client arriving; `None` when it is a client
    /// already connected being moved over.
    reply: Option<mpsc::Sender<Result<Accept, String>>>,
}

#[derive(Default)]
struct State {
    last_id: u32,
    agents: BTreeMap<u32, Agent>,
    /// The agent whose frames go to the client.
    current: Option<u32>,
    waiting: Option<Waiting>,
    link: Option<Link>,
    client: Option<ClientInfo>,
    /// The agent the next client goes to, if it was said.
    preferred: Option<u32>,
    /// The client's last word on each feature (sound on, microphone on, ...), for telling an
    /// agent that takes the client over.
    said: Vec<Message>,
}

pub struct Shared {
    /// Held while a frame goes to the client and while the agent in charge changes.
    sending: Mutex<()>,
    state: Mutex<State>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { sending: Mutex::new(()), state: Mutex::default() })
    }
}

fn tell(writer: &Writer, message: ToAgent) {
    // An agent that has gone is noticed by its own thread.
    let _ = message.write(&mut *writer.lock().unwrap());
}

pub struct Broker(pub Arc<Shared>);

impl Broker {
    fn to_current(&self, message: ToAgent) {
        let writer = {
            let st = self.0.state.lock().unwrap();
            st.current.and_then(|id| st.agents.get(&id)).map(|a| a.writer.clone())
        };
        if let Some(w) = writer {
            tell(&w, message);
        }
    }
}

impl HostHandler for Broker {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        let (tx, rx) = mpsc::channel();
        let (id, writer) = {
            let mut st = self.0.state.lock().unwrap();
            // Until sessions are told apart (whose they are, login screen or not): the one
            // that was asked for, else the newest.
            let id = st.preferred.filter(|id| st.agents.contains_key(id)).or_else(|| st.agents.keys().next_back().copied());
            let Some(id) = id else {
                return Err("nobody is logged in on this computer, and logging in from here isn't built yet".into());
            };
            st.link = Some(link.clone());
            st.client = Some(client.clone());
            st.waiting = Some(Waiting { agent: id, reply: Some(tx) });
            (id, st.agents[&id].writer.clone())
        };
        tracing::info!(name = %client.name, agent = id, "client");
        tell(&writer, ToAgent::Client { info: client.clone(), elapsed_us: link.now_us() });
        let answer = match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(answer) => answer,
            Err(_) => {
                tell(&writer, ToAgent::Left);
                Err("the desktop session didn't answer".into())
            }
        };
        if answer.is_err() {
            let mut st = self.0.state.lock().unwrap();
            (st.link, st.client, st.waiting) = (None, None, None);
        }
        answer
    }

    fn keyframe(&mut self) {
        self.to_current(ToAgent::Keyframe);
    }

    fn lost(&mut self, first: u32, last: u32) {
        self.to_current(ToAgent::Lost { first, last });
    }

    fn input(&mut self, event: Event) {
        self.to_current(ToAgent::Input(event));
    }

    fn message(&mut self, message: &Message) {
        {
            let mut st = self.0.state.lock().unwrap();
            // What the client wants holds across a move to another agent: remember it, and
            // have the next agent make the display as last asked for.
            if let (Message::Configure { display: Some(display), .. }, Some(client)) = (message, st.client.as_mut()) {
                client.display = *display;
            }
            if !matches!(message, Message::FileResult { .. }) {
                st.said.retain(|m| std::mem::discriminant(m) != std::mem::discriminant(message));
                st.said.push(message.clone());
            }
        }
        self.to_current(ToAgent::Message(message.clone()));
    }

    fn mic(&mut self, packet: &Packet) {
        self.to_current(ToAgent::Mic(packet.clone()));
    }

    fn camera(&mut self, frame: &Frame) {
        self.to_current(ToAgent::Camera(frame.clone()));
    }

    fn blob(&mut self, event: &BlobEvent) {
        tracing::debug!(id = event.id(), "a blob event from the client");
        self.to_current(ToAgent::Blob(event.clone()));
    }

    fn left(&mut self) {
        let _sending = self.0.sending.lock().unwrap();
        let writers: Vec<Writer> = {
            let mut st = self.0.state.lock().unwrap();
            let ids = [st.current.take(), st.waiting.take().map(|w| w.agent)];
            (st.link, st.client) = (None, None);
            st.said.clear();
            ids.into_iter().flatten().filter_map(|id| st.agents.get(&id)).map(|a| a.writer.clone()).collect()
        };
        for w in writers {
            tell(&w, ToAgent::Left);
        }
        tracing::info!("client gone");
    }
}

/// Take agents (and `remotemand ctl`) as they connect.
pub fn accept(listener: UnixListener, shared: Arc<Shared>) {
    for stream in listener.incoming().flatten() {
        let shared = shared.clone();
        let _ = std::thread::Builder::new().name("agent".into()).spawn(move || attached(shared, stream));
    }
}

/// Who is at the other end, as the kernel has it.
fn peer(stream: &UnixStream) -> (u32, i32) {
    // SAFETY: a plain struct filled in by the kernel, with its size passed along.
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len);
        (cred.uid, cred.pid)
    }
}

fn attached(shared: Arc<Shared>, stream: UnixStream) {
    let (uid, pid) = peer(&stream);
    let Ok(mut reader) = stream.try_clone() else { return };
    let writer: Writer = Arc::new(Mutex::new(stream));
    let features = match ToDaemon::read(&mut reader) {
        Ok(Some(ToDaemon::Hello { features })) => features,
        Ok(Some(ToDaemon::Control(words))) => {
            use std::io::Write;
            // SAFETY: no arguments, no state.
            let ours = uid == 0 || uid == unsafe { libc::geteuid() };
            // Looking is for any local user; changing anything is not.
            let looking = matches!(words.split_whitespace().next(), Some("status") | None);
            let answer = if ours || looking { control(&shared, &words) } else { "only root and the daemon's own user may do that\n".into() };
            let _ = writer.lock().unwrap().write_all(answer.as_bytes());
            return;
        }
        _ => return,
    };
    let id = {
        let mut st = shared.state.lock().unwrap();
        st.last_id += 1;
        let id = st.last_id;
        st.agents.insert(id, Agent { writer: writer.clone(), uid, pid, features });
        id
    };
    tracing::info!(agent = id, uid, pid, "an agent attached");
    // Blobs this agent is sending to the client, by the agent's number for each.
    let mut sending: BTreeMap<u32, BlobSender> = BTreeMap::new();

    while let Ok(Some(message)) = ToDaemon::read(&mut reader) {
        match message {
            ToDaemon::Accepted(accept) => {
                let _sending = shared.sending.lock().unwrap();
                let mut st = shared.state.lock().unwrap();
                if !st.waiting.as_ref().is_some_and(|w| w.agent == id) {
                    // Nobody is waiting for it any more.
                    drop(st);
                    tell(&writer, ToAgent::Left);
                    continue;
                }
                match st.waiting.take().and_then(|w| w.reply) {
                    // The client arriving: the core tells it about its stream.
                    Some(reply) => {
                        st.current = Some(id);
                        let _ = reply.send(Ok(accept));
                    }
                    // A client moving over: this agent's picture replaces the other's, from
                    // exactly here.
                    None => {
                        let old = st.current.replace(id).and_then(|o| st.agents.get(&o)).map(|a| a.writer.clone());
                        let link = st.link.clone();
                        let said = st.said.clone();
                        drop(st);
                        if let Some(link) = link {
                            link.restart(accept);
                        }
                        // What the client had switched on carries over.
                        for message in said {
                            if !matches!(message, Message::Configure { .. }) {
                                tell(&writer, ToAgent::Message(message));
                            }
                        }
                        if let Some(old) = old {
                            tell(&old, ToAgent::Left);
                        }
                        tracing::info!(agent = id, "the client's picture now comes from this agent");
                    }
                }
            }
            ToDaemon::Refused(why) => {
                let mut st = shared.state.lock().unwrap();
                if st.waiting.as_ref().is_some_and(|w| w.agent == id) {
                    match st.waiting.take().and_then(|w| w.reply) {
                        Some(reply) => {
                            let _ = reply.send(Err(why));
                        }
                        None => tracing::warn!(agent = id, "the agent wouldn't take the client: {why}"),
                    }
                }
            }
            ToDaemon::Frame { key, recovery, capture_us, data } => {
                let _sending = shared.sending.lock().unwrap();
                let link = {
                    let st = shared.state.lock().unwrap();
                    if st.current == Some(id) { st.link.clone() } else { None }
                };
                // Waits while the network is behind, which holds this agent's encoder back.
                if let Some(link) = link {
                    if recovery {
                        link.send_recovery_frame(&data, capture_us);
                    } else {
                        link.send_frame(&data, key, capture_us);
                    }
                }
            }
            // In order with this agent's frames, like them.
            ToDaemon::Restart(accept) => {
                let _sending = shared.sending.lock().unwrap();
                if let Some(link) = current_link(&shared, id) {
                    link.restart(accept);
                }
            }
            ToDaemon::CursorShape(shape) => {
                if let Some(link) = current_link(&shared, id) {
                    link.cursor_shape(shape);
                }
            }
            ToDaemon::CursorAt { x, y, visible } => {
                if let Some(link) = current_link(&shared, id) {
                    link.cursor_at(x, y, visible);
                }
            }
            ToDaemon::Message(message) => {
                if let Some(link) = current_link(&shared, id) {
                    link.send_message(message);
                }
            }
            ToDaemon::Sound(p) => {
                if let Some(link) = current_link(&shared, id) {
                    link.send_sound(p.start, p.seq, p.time_us, &p.data);
                }
            }
            // A blob for the client. Its content waits here while the network is behind,
            // which holds the agent back.
            ToDaemon::Blob(BlobEvent::Open { id: blob, kind, header }) => {
                tracing::debug!(agent = id, blob, kind, "a blob from the agent");
                if let Some(link) = current_link(&shared, id) {
                    sending.insert(blob, link.open_blob(kind, header));
                }
            }
            ToDaemon::Blob(BlobEvent::Data { id: blob, bytes }) => {
                if sending.get(&blob).is_some_and(|s| !s.write(&bytes)) {
                    sending.remove(&blob);
                }
            }
            ToDaemon::Blob(BlobEvent::Close { id: blob, complete }) => {
                if let (Some(sender), true) = (sending.remove(&blob), complete) {
                    sender.finish();
                }
            }
            ToDaemon::Hello { .. } | ToDaemon::Control(_) => {}
        }
    }

    let _sending = shared.sending.lock().unwrap();
    let mut st = shared.state.lock().unwrap();
    st.agents.remove(&id);
    if st.preferred == Some(id) {
        st.preferred = None;
    }
    if st.waiting.as_ref().is_some_and(|w| w.agent == id) {
        if let Some(reply) = st.waiting.take().and_then(|w| w.reply) {
            let _ = reply.send(Err("the desktop session ended".into()));
        }
    }
    if st.current == Some(id) {
        st.current = None;
        tracing::warn!(agent = id, "the agent serving the client went away");
    }
    tracing::info!(agent = id, "an agent detached");
}

fn current_link(shared: &Shared, id: u32) -> Option<Link> {
    let st = shared.state.lock().unwrap();
    if st.current == Some(id) { st.link.clone() } else { None }
}

/// `remotemand ctl ...`
fn control(shared: &Shared, words: &str) -> String {
    let mut words = words.split_whitespace();
    let number = |w: Option<&str>| w.and_then(|w| w.parse::<u32>().ok());
    match words.next() {
        Some("status") | None => {
            let st = shared.state.lock().unwrap();
            let mut out = match &st.client {
                Some(c) => format!("client: {} ({}x{} at {} Hz)\n", c.name, c.display.width, c.display.height, c.display.refresh),
                None => "client: none\n".to_owned(),
            };
            for (id, a) in &st.agents {
                let mark = if st.current == Some(*id) { "serving the client" } else if st.preferred == Some(*id) { "gets the next client" } else { "" };
                out += &format!("agent {id}: uid {} pid {} [{}] {mark}\n", a.uid, a.pid, a.features.join(","));
            }
            if st.agents.is_empty() {
                out += "no agents attached\n";
            }
            out
        }
        Some("prefer") => match number(words.next()) {
            Some(id) if shared.state.lock().unwrap().agents.contains_key(&id) => {
                shared.state.lock().unwrap().preferred = Some(id);
                format!("the next client goes to agent {id}\n")
            }
            _ => "prefer N: no such agent\n".into(),
        },
        Some("switch") => {
            let Some(id) = number(words.next()) else { return "switch N: which agent?\n".into() };
            let mut st = shared.state.lock().unwrap();
            let (Some(client), Some(link)) = (st.client.clone(), st.link.clone()) else { return "no client is connected\n".into() };
            let Some(writer) = st.agents.get(&id).map(|a| a.writer.clone()) else { return "no such agent\n".into() };
            if st.current == Some(id) {
                return "that agent is serving the client already\n".into();
            }
            if st.waiting.is_some() {
                return "a change is under way already\n".into();
            }
            st.waiting = Some(Waiting { agent: id, reply: None });
            drop(st);
            tell(&writer, ToAgent::Client { info: client, elapsed_us: link.now_us() });
            format!("moving the client to agent {id}\n")
        }
        Some(other) => format!("unknown command {other}: status, switch N, prefer N\n"),
    }
}
