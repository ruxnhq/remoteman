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

use crate::gdm::{self, Gdm};

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
    /// Counts clients, so that work started for one stops when it has gone.
    generation: u64,
}

/// Getting a device into a session it isn't in yet, through GDM.
pub struct Login {
    pub runtime: tokio::runtime::Runtime,
    pub gdm: Gdm,
    /// Straight into the owner's session by the device's key alone, with no login screen.
    pub by_key: bool,
}

pub struct Shared {
    /// Held while a frame goes to the client and while the agent in charge changes.
    sending: Mutex<()>,
    state: Mutex<State>,
    /// Whose the paired devices are: name and uid. (One user for every device, until each
    /// device has an owner of its own.)
    owner: Option<(String, u32)>,
    login: Option<Login>,
}

impl Shared {
    pub fn new(owner: Option<(String, u32)>, login: Option<Login>) -> Arc<Self> {
        Arc::new(Self { sending: Mutex::new(()), state: Mutex::default(), owner, login })
    }

    /// The newest agent running as `uid`.
    fn agent_of(&self, uid: u32) -> Option<u32> {
        let st = self.state.lock().unwrap();
        st.agents.iter().rev().find(|(_, a)| a.uid == uid).map(|(id, _)| *id)
    }

    /// Ask GDM for a display for this client and wait for the agent of the session behind it:
    /// the login screen's, or with `by_key` the owner's own.
    fn open_login(&self, login: &Login, remote_id: &str, client: &ClientInfo) -> Result<u32, String> {
        let user = if login.by_key { self.owner.as_ref().map(|o| o.0.as_str()) } else { None };
        login.runtime.block_on(login.gdm.create_display(remote_id, &client.name, user));
        let start = std::time::Instant::now();
        let mut seen = None;
        while start.elapsed() < Duration::from_secs(30) {
            if let Ok(sessions) = login.runtime.block_on(login.gdm.sessions(remote_id)) {
                // A user's session if there is one already, else the login screen's.
                if let Some(s) = sessions.iter().find(|s| s.class == "user").or(sessions.first()) {
                    seen = Some(s.clone());
                    if let Some(id) = self.agent_of(s.uid) {
                        tracing::info!(session = %s.id, class = %s.class, uid = s.uid, agent = id, "GDM made a session for the client");
                        return Ok(id);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Err(match seen {
            Some(s) => format!("GDM made a session ({}, {}), but no remoteman agent started in it: is remoteman-agent.service enabled for every user?", s.id, s.class),
            None => "GDM made no session for this device within 30 s".into(),
        })
    }
}

/// Follow a client that came in through GDM: when a user's session turns up behind its
/// display (someone logged in on the login screen), move the client to that session's agent.
async fn follow(shared: Arc<Shared>, remote_id: String, generation: u64) {
    let Some(login) = shared.login.as_ref() else { return };
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        {
            let st = shared.state.lock().unwrap();
            if st.generation != generation || st.client.is_none() {
                return;
            }
        }
        let Ok(sessions) = login.gdm.sessions(&remote_id).await else { continue };
        let Some(user) = sessions.iter().find(|s| s.class == "user") else { continue };
        // Its agent attaches a moment after the session starts.
        let Some(target) = shared.agent_of(user.uid) else { continue };
        if shared.state.lock().unwrap().current == Some(target) {
            return;
        }
        match begin_switch(&shared, target) {
            Ok(()) => tracing::info!(session = %user.id, agent = target, "someone logged in: moving the client to their session"),
            // A change is under way; look again.
            Err(_) => continue,
        }
    }
}

/// Hand the connected client to another agent. It takes over once it has answered.
fn begin_switch(shared: &Shared, id: u32) -> Result<(), &'static str> {
    let mut st = shared.state.lock().unwrap();
    let (Some(client), Some(link)) = (st.client.clone(), st.link.clone()) else { return Err("no client is connected") };
    let Some(writer) = st.agents.get(&id).map(|a| a.writer.clone()) else { return Err("no such agent") };
    if st.current == Some(id) {
        return Err("that agent is serving the client already");
    }
    if st.waiting.is_some() {
        return Err("a change is under way already");
    }
    st.waiting = Some(Waiting { agent: id, reply: None });
    drop(st);
    tell(&writer, ToAgent::Client { info: client, elapsed_us: link.now_us() });
    Ok(())
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
        let generation = {
            let mut st = self.0.state.lock().unwrap();
            st.generation += 1;
            st.generation
        };
        // Where the client goes: an agent that was asked for (tests), else the session its
        // owner already has; with no owner said, the newest agent.
        let found = {
            let st = self.0.state.lock().unwrap();
            st.preferred.filter(|id| st.agents.contains_key(id))
        };
        let found = found.or_else(|| match &self.0.owner {
            Some((_, uid)) => self.0.agent_of(*uid),
            None => self.0.state.lock().unwrap().agents.keys().next_back().copied(),
        });
        // Not logged in: ask GDM, and remember to follow the client to wherever it logs in.
        let mut following = None;
        let id = match (found, &self.0.login, &self.0.owner) {
            (Some(id), _, _) => id,
            (None, Some(login), Some(_)) => {
                let remote_id = gdm::new_remote_id();
                let id = self.0.open_login(login, &remote_id, client)?;
                following = Some(remote_id);
                id
            }
            (None, _, Some((name, _))) => return Err(format!("{name} isn't logged in on this computer, and it isn't set up to log in from a device")),
            (None, _, None) => return Err("nobody is logged in on this computer".into()),
        };
        let (id, writer) = {
            let mut st = self.0.state.lock().unwrap();
            let Some(agent) = st.agents.get(&id) else { return Err("the desktop session ended".into()) };
            let writer = agent.writer.clone();
            if let (Some(remote_id), Some(login)) = (following, &self.0.login) {
                login.runtime.spawn(follow(self.0.clone(), remote_id, generation));
            }
            let _ = &writer;
            st.link = Some(link.clone());
            st.client = Some(client.clone());
            st.waiting = Some(Waiting { agent: id, reply: Some(tx) });
            (id, writer)
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
            st.generation += 1;
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
            match begin_switch(shared, id) {
                Ok(()) => format!("moving the client to agent {id}\n"),
                Err(why) => format!("{why}\n"),
            }
        }
        Some(other) => format!("unknown command {other}: status, switch N, prefer N\n"),
    }
}
