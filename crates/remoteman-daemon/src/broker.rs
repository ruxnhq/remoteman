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
use crate::users::{self, LoggedIn, Users, may_serve};

/// How long an agent has to answer for a client that is arriving.
const ANSWER: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 20 });
/// How long one write to an agent may make no headway before the agent is given up on: an
/// agent that has stopped reading (stopped, or stuck) must not hold up the thread every
/// client's messages pass through.
const TELL: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 10_000 });
/// How long whoever connects to the socket has to say what it is.
const HELLO: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 10_000 });
/// How long a client whose session's agent went away is kept, for another agent of its user
/// to attach (the agent restarting; a login screen giving way to a session), before it is
/// dropped rather than left looking at a picture that will never move.
const ORPHAN: Duration = Duration::from_millis(if cfg!(test) { 2_000 } else { 20_000 });

type Writer = Arc<Mutex<UnixStream>>;

struct Agent {
    writer: Writer,
    /// Who it runs as and its process, from the kernel.
    uid: u32,
    pid: i32,
    features: Vec<String>,
    /// Answers still to come from it for clients that have gone since it was asked. An agent
    /// answers every `Client` it is sent, in order, so that many answers are nobody's.
    owed: u32,
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
    /// Whose that client's device is (see `users.rs`).
    client_user: Option<u32>,
    /// The agent the next client goes to, if it was said.
    preferred: Option<u32>,
    /// The client's last word on each feature (sound on, microphone on, ...), for telling an
    /// agent that takes the client over.
    said: Vec<Message>,
    /// Counts clients, so that work started for one stops when it has gone.
    generation: u64,
    /// Clients dropped for want of a session when nothing is listening (tests of the broker).
    orphans_dropped: u32,
}

impl State {
    /// Stop waiting for an agent's answer. The answer still comes, and is then nobody's.
    fn abandon(&mut self) -> Option<Writer> {
        let agent = self.agents.get_mut(&self.waiting.take()?.agent)?;
        agent.owed += 1;
        Some(agent.writer.clone())
    }
}

/// Pairing as it was last opened over the socket.
struct Pairing {
    /// Who opened it: the pending device is theirs to see, accept and decline.
    opener: u32,
    /// Whose the device that comes will be: the opener, or for root and the daemon's own user
    /// the `--user` user (none).
    by: Option<u32>,
    /// The devices trusted when it was opened: one that isn't among them came through it.
    known: Vec<String>,
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
    /// Whose the paired devices are that have no user of their own: name and uid.
    owner: Option<(String, u32)>,
    /// Whose each of the others is, and whose sharing is off.
    users: Users,
    /// Held for the length of every `pair` request, and while a new device is given its user.
    pairing: Mutex<Option<Pairing>>,
    /// Who the daemon runs as: with root, the one who may see and change every user's devices.
    euid: u32,
    /// Users who may have devices of their own though the user database doesn't make them
    /// out to be people (`--users`).
    people: Vec<u32>,
    login: Option<Login>,
    /// The listening side, once it is up: the trust list and the client's connection.
    /// Not held: the daemon's main thread is the one that stops it.
    host: std::sync::OnceLock<std::sync::Weak<remoteman_core::host::Host>>,
}

impl Shared {
    /// `state`: the daemon's state folder, where a pause of sharing is remembered.
    pub fn new(owner: Option<(String, u32)>, login: Option<Login>, state: Option<&std::path::Path>, people: Vec<u32>) -> Arc<Self> {
        let users = Users::load(state, owner.as_ref().map(|o| o.1));
        // SAFETY: no arguments, no state.
        let euid = unsafe { libc::geteuid() };
        Arc::new(Self { sending: Mutex::new(()), state: Mutex::default(), owner, users, pairing: Mutex::default(), euid, people, login, host: Default::default() })
    }

    fn host(&self) -> Option<Arc<remoteman_core::host::Host>> {
        self.host.get().and_then(|h| h.upgrade())
    }

    /// Whose a device is: the user it was paired by, else the `--user` user, else nobody's.
    fn user_of(&self, fingerprint: &str) -> Option<u32> {
        self.users.get(fingerprint).or(self.owner.as_ref().map(|o| o.1))
    }

    /// Give every device that came through the pairing last opened to whoever opened it. (A
    /// device let in without being asked about is trusted by the core before anything here
    /// hears of it; one that was asked about got its user when it was accepted.)
    fn settle(&self, host: &remoteman_core::host::Host, pairing: &mut Option<Pairing>) {
        let Some(pairing) = pairing else { return };
        for device in host.devices() {
            if !pairing.known.contains(&device.fingerprint) {
                tracing::info!(name = %device.name, user = ?pairing.by, "a new device, and whose it is");
                self.users.set(&device.fingerprint, pairing.by);
                pairing.known.push(device.fingerprint);
            }
        }
    }

    pub fn listening(&self, host: &Arc<remoteman_core::host::Host>) {
        let _ = self.host.set(Arc::downgrade(host));
    }

    /// The newest agent running as `uid`.
    fn agent_of(&self, uid: u32) -> Option<u32> {
        let st = self.state.lock().unwrap();
        st.agents.iter().rev().find(|(_, a)| a.uid == uid).map(|(id, _)| *id)
    }

    /// Ask GDM for a display for this client and wait for the agent of the session behind it:
    /// the login screen's, or with `by_key` the session of `user`, whose device it is.
    fn open_login(&self, login: &Login, remote_id: &str, client: &ClientInfo, user: u32) -> Result<u32, String> {
        let name = match login.by_key {
            true => Some(users::name_of(user).ok_or_else(|| format!("this device belongs to user {user}, who is not a user of this computer any more"))?),
            false => None,
        };
        login.runtime.block_on(login.gdm.create_display(remote_id, &client.name, name.as_deref()));
        let start = std::time::Instant::now();
        let mut seen = None;
        while start.elapsed() < Duration::from_secs(30) {
            if let Ok(sessions) = login.runtime.block_on(login.gdm.sessions(remote_id)) {
                // Its user's session if there is one already, else the login screen's; never
                // a session of somebody else's.
                if let Some(s) = users::session_for(&sessions, user) {
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

/// Follow a client that came in through GDM: when its user's session turns up behind its
/// display (they logged in on the login screen), move the client to that session's agent.
/// Somebody else logging in there doesn't take the device with them: it is dropped.
async fn follow(shared: Arc<Shared>, remote_id: String, generation: u64, owner: u32) {
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
        let user = match users::logged_in(&sessions, owner) {
            LoggedIn::Nobody => continue,
            LoggedIn::Owner(session) => session,
            LoggedIn::Stranger(uid) => {
                tracing::warn!(uid, owner, "somebody else logged in on this device's login screen: the device is dropped, not taken into their session");
                if let Some(host) = shared.host() {
                    host.drop_client();
                }
                return;
            }
        };
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
    let Some((writer, uid)) = st.agents.get(&id).map(|a| (a.writer.clone(), a.uid)) else { return Err("no such agent") };
    // The one rule, whoever asks: a device goes to its own user's session only. (A login
    // screen is reached when the device arrives, never by a move.)
    if !may_serve(st.client_user, uid, false) {
        return Err("that session is another user's than the connected device's");
    }
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
    let mut stream = writer.lock().unwrap();
    // An agent that has gone is noticed by its own thread. One that doesn't take what it is
    // sent (the write gave up after `TELL`, perhaps part way through a message) can't be
    // talked to any more: closing its socket ends its thread here, which detaches it.
    if message.write(&mut *stream).is_err() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
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
        // A device that has just been paired is given its user before anything is decided.
        if let Some(host) = self.0.host() {
            self.0.settle(&host, &mut self.0.pairing.lock().unwrap());
        }
        let user = self.0.user_of(&client.fingerprint);
        if self.0.users.off(user) {
            return Err("sharing is switched off on this computer".into());
        }
        let (tx, rx) = mpsc::channel();
        let generation = {
            let mut st = self.0.state.lock().unwrap();
            st.generation += 1;
            st.generation
        };
        // Where the client goes: an agent that was asked for (tests) if it is its user's, else
        // the session its user already has; with no user at all, the newest agent.
        let found = {
            let st = self.0.state.lock().unwrap();
            st.preferred.filter(|id| st.agents.get(id).is_some_and(|a| may_serve(user, a.uid, false)))
        };
        let found = found.or_else(|| match user {
            Some(uid) => self.0.agent_of(uid),
            None => self.0.state.lock().unwrap().agents.keys().next_back().copied(),
        });
        // A device set to come only into a session that is already running is not shown a
        // login screen, nor logged in by its key.
        if found.is_none() && client.attach_only {
            return Err("nobody is logged in at this computer, and this device is set not to use its login screen".into());
        }
        // Not logged in: ask GDM, and remember to follow the client to wherever it logs in.
        let mut following = None;
        let id = match (found, &self.0.login, user) {
            (Some(id), _, _) => id,
            (None, Some(login), Some(uid)) => {
                let remote_id = gdm::new_remote_id();
                let id = self.0.open_login(login, &remote_id, client, uid)?;
                following = Some((remote_id, uid));
                id
            }
            (None, _, Some(uid)) => {
                let name = users::name_of(uid).unwrap_or_else(|| format!("user {uid}"));
                return Err(format!("{name} isn't logged in on this computer, and it isn't set up to log in from a device"));
            }
            (None, _, None) => return Err("nobody is logged in on this computer".into()),
        };
        let writer = {
            let mut st = self.0.state.lock().unwrap();
            let Some(agent) = st.agents.get(&id) else { return Err("the desktop session ended".into()) };
            let writer = agent.writer.clone();
            if let (Some((remote_id, uid)), Some(login)) = (following, &self.0.login) {
                login.runtime.spawn(follow(self.0.clone(), remote_id, generation, uid));
            }
            st.link = Some(link.clone());
            st.client = Some(client.clone());
            st.client_user = user;
            st.waiting = Some(Waiting { agent: id, reply: Some(tx) });
            writer
        };
        tracing::info!(name = %client.name, agent = id, user = ?user, "client");
        tell(&writer, ToAgent::Client { info: client.clone(), elapsed_us: link.now_us() });
        let answer = match rx.recv_timeout(ANSWER) {
            Ok(answer) => answer,
            Err(_) => {
                let mut st = self.0.state.lock().unwrap();
                match st.waiting.as_ref().is_some_and(|w| w.agent == id) {
                    // Its answer, when it comes, is nobody's.
                    true => {
                        st.abandon();
                        drop(st);
                        tell(&writer, ToAgent::Left);
                        Err("the desktop session didn't answer".into())
                    }
                    // It answered just as the wait ran out: the answer is there to take.
                    false => rx.try_recv().unwrap_or_else(|_| Err("the desktop session didn't answer".into())),
                }
            }
        };
        if answer.is_err() {
            let mut st = self.0.state.lock().unwrap();
            (st.link, st.client, st.client_user, st.waiting) = (None, None, None, None);
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
            if !matches!(message, Message::FileResult { .. } | Message::DisplayKeyframe { .. }) {
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
            // An agent the client was being moved to still owes its answer.
            let waiting = st.abandon();
            let current = st.current.take().and_then(|id| st.agents.get(&id)).map(|a| a.writer.clone());
            (st.link, st.client, st.client_user) = (None, None, None);
            st.said.clear();
            st.generation += 1;
            [current, waiting].into_iter().flatten().collect()
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

/// Who is at the other end, as the kernel has it. Nobody, if the kernel won't say: who an
/// agent is decides whose devices it is given, so it is never guessed (a struct left at zero
/// would read as root).
fn peer(stream: &UnixStream) -> Option<(u32, i32)> {
    // SAFETY: a plain struct filled in by the kernel, with its size passed along.
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let said = libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut cred).cast(), &mut len);
        (said == 0 && len as usize == std::mem::size_of::<libc::ucred>()).then_some((cred.uid, cred.pid))
    }
}

fn attached(shared: Arc<Shared>, stream: UnixStream) {
    match peer(&stream) {
        Some((uid, pid)) => serve(shared, stream, uid, pid),
        None => tracing::warn!("the kernel wouldn't say who connected to the socket: turned away"),
    }
}

/// One connection to the socket, from an agent or from `remotemand ctl`, made by `uid`.
fn serve(shared: Arc<Shared>, stream: UnixStream, uid: u32, pid: i32) {
    // Whoever connects says at once what it is; one that says nothing doesn't keep a thread.
    let _ = stream.set_read_timeout(Some(HELLO));
    let _ = stream.set_write_timeout(Some(TELL));
    let Ok(mut reader) = stream.try_clone() else { return };
    let writer: Writer = Arc::new(Mutex::new(stream));
    let features = match ToDaemon::read(&mut reader) {
        Ok(Some(ToDaemon::Hello { features })) => features,
        Ok(Some(ToDaemon::Control(words))) => {
            use std::io::Write;
            let answer = control(&shared, &words, uid);
            let _ = writer.lock().unwrap().write_all(answer.as_bytes());
            return;
        }
        _ => return,
    };
    let _ = reader.set_read_timeout(None);
    let (id, orphan) = {
        let mut st = shared.state.lock().unwrap();
        st.last_id += 1;
        let id = st.last_id;
        st.agents.insert(id, Agent { writer: writer.clone(), uid, pid, features, owed: 0 });
        // A client whose session's agent went away, and this is its user's: it is taken up.
        (id, st.client.is_some() && st.current.is_none() && st.waiting.is_none() && may_serve(st.client_user, uid, false))
    };
    tracing::info!(agent = id, uid, pid, "an agent attached");
    if orphan && begin_switch(&shared, id).is_ok() {
        tracing::info!(agent = id, "the client that was left without a session is given to this agent");
    }
    // Blobs this agent is sending to the client, by the agent's number for each.
    let mut sending: BTreeMap<u32, BlobSender> = BTreeMap::new();

    while let Ok(Some(message)) = ToDaemon::read(&mut reader) {
        match message {
            ToDaemon::Accepted(accept) => {
                let _sending = shared.sending.lock().unwrap();
                let mut st = shared.state.lock().unwrap();
                // The answer for a client that has gone since: it was told so already.
                if let Some(agent) = st.agents.get_mut(&id).filter(|a| a.owed > 0) {
                    agent.owed -= 1;
                    continue;
                }
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
                tracing::info!(agent = id, "the agent turned the client away: {}", why.strip_prefix("locked: ").unwrap_or(&why));
                let mut st = shared.state.lock().unwrap();
                if let Some(agent) = st.agents.get_mut(&id).filter(|a| a.owed > 0) {
                    agent.owed -= 1;
                    continue;
                }
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
            // The second display's picture, the same way.
            ToDaemon::Frame2 { key, recovery, capture_us, data } => {
                let _sending = shared.sending.lock().unwrap();
                if let Some(link) = current_link(&shared, id) {
                    let link = link.second();
                    if recovery {
                        link.send_recovery_frame(&data, capture_us);
                    } else {
                        link.send_frame(&data, key, capture_us);
                    }
                }
            }
            ToDaemon::Restart2(accept) => {
                let _sending = shared.sending.lock().unwrap();
                if let Some(link) = current_link(&shared, id) {
                    link.second().restart(accept);
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
        // The client is kept a while for another agent of its user's (see `serve`), and
        // dropped if none has taken it by then.
        if st.client.is_some() {
            let (shared, generation) = (shared.clone(), st.generation);
            let _ = std::thread::Builder::new().name("orphan".into()).spawn(move || {
                std::thread::sleep(ORPHAN);
                let st = shared.state.lock().unwrap();
                if st.generation == generation && st.client.is_some() && st.current.is_none() && st.waiting.is_none() {
                    drop(st);
                    tracing::warn!("no session took the client up after its agent went away: the client is dropped");
                    match shared.host() {
                        Some(host) => drop(host.drop_client()),
                        None => shared.state.lock().unwrap().orphans_dropped += 1,
                    }
                }
            });
        }
    }
    tracing::info!(agent = id, "an agent detached");
}

fn current_link(shared: &Shared, id: u32) -> Option<Link> {
    let st = shared.state.lock().unwrap();
    if st.current == Some(id) { st.link.clone() } else { None }
}

/// `remotemand ctl ...`, asked by `uid`. Root and the user the daemon runs as see and change
/// everything. The `--user` user, and any other person with a desktop session here, sees and
/// changes what is theirs: their devices, their pairing, their sharing. "A person with a
/// session": an agent of theirs is attached, and the user database gives them a shell to log
/// in with (or `--users` names them). An agent alone is not enough: the socket is open to
/// every local account, and the accounts of services and of the login screen are not to pair
/// devices for themselves.
fn control(shared: &Shared, words: &str, uid: u32) -> String {
    let admin = uid == 0 || uid == shared.euid;
    let owner = shared.owner.as_ref().map(|o| o.1);
    let mut words = words.split_whitespace();
    let number = |w: Option<&str>| w.and_then(|w| w.parse::<u32>().ok());
    let word = words.next().unwrap_or("status");
    // Looking at the agents is for any local user; the rest is not.
    let person = || shared.agent_of(uid).is_some() && (shared.people.contains(&uid) || users::is_person(uid));
    if word != "status" && !(admin || owner == Some(uid) || person()) {
        return "only root, the daemon's own user and people with a desktop session on this computer may do that\n".into();
    }
    let mine = |fingerprint: &str| admin || shared.user_of(fingerprint) == Some(uid);
    // The pairing last opened is the asker's to see and answer if they opened it.
    let theirs = |pairing: &Option<Pairing>| admin || pairing.as_ref().map_or(owner == Some(uid), |p| p.opener == uid);
    match word {
        "status" => {
            let st = shared.state.lock().unwrap();
            let mut out = match &st.client {
                // A device's name is its user's business.
                Some(_) if !(admin || st.client_user.is_none_or(|u| u == uid)) => "client: another user's device\n".to_owned(),
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
        "prefer" => {
            let mut st = shared.state.lock().unwrap();
            match number(words.next()).filter(|id| st.agents.get(id).is_some_and(|a| admin || a.uid == uid)) {
                Some(id) => {
                    st.preferred = Some(id);
                    format!("the next client goes to agent {id}\n")
                }
                None => "prefer N: no such agent\n".into(),
            }
        }
        "switch" => {
            let Some(id) = number(words.next()) else { return "switch N: which agent?\n".into() };
            if !admin && shared.state.lock().unwrap().client_user != Some(uid) {
                return "no client is connected\n".into();
            }
            match begin_switch(shared, id) {
                Ok(()) => format!("moving the client to agent {id}\n"),
                Err(why) => format!("{why}\n"),
            }
        }
        // For the owner's programs (the agent in their session, which Remoteman Host asks): how
        // things stand for the asker, as one line of JSON.
        "owner" => {
            let Some(host) = shared.host() else { return "the daemon isn't listening\n".into() };
            let mut pairing = shared.pairing.lock().unwrap();
            shared.settle(&host, &mut pairing);
            let theirs = theirs(&pairing);
            let devices: Vec<_> = host.devices().into_iter().filter(|d| mine(&d.fingerprint)).map(|d| serde_json::json!({ "fingerprint": d.fingerprint, "name": d.name, "connected": d.connected, "user": shared.user_of(&d.fingerprint) })).collect();
            let status = serde_json::json!({
                "address": host.address.to_string(),
                "fingerprint": host.fingerprint,
                "pairing": theirs && host.pairing(),
                "pending": if theirs { host.pending() } else { None },
                "devices": devices,
                "paused": shared.users.off(if admin { None } else { Some(uid) }),
            });
            format!("{status}\n")
        }
        // Switched off, the asker's devices are turned away (the connected one is dropped);
        // for root, every user's. The daemon keeps running, and it stays so across restarts
        // until switched on again.
        "sharing" => {
            let on = match words.next() {
                Some("on") => true,
                Some("off") => false,
                _ => return "sharing on|off\n".into(),
            };
            let whose = if admin { None } else { Some(uid) };
            shared.users.set_off(whose, !on, owner);
            if !on && (admin || shared.state.lock().unwrap().client_user == Some(uid)) {
                if let Some(host) = shared.host() {
                    host.drop_client();
                }
            }
            if on && shared.users.off(whose) {
                return "sharing is switched off for every user of this computer, which only root can undo\n".into();
            }
            "ok\n".into()
        }
        "devices" | "revoke" | "drop" | "pair" => {
            let Some(host) = shared.host() else { return "the daemon isn't listening\n".into() };
            // One `pair` request at a time, and none while a new device is given its user.
            let mut pairing = shared.pairing.lock().unwrap();
            shared.settle(&host, &mut pairing);
            let theirs = theirs(&pairing);
            let devices: Vec<_> = host.devices().into_iter().filter(|d| mine(&d.fingerprint)).collect();
            match word {
                "devices" => {
                    let mut out: String = devices.iter().map(|d| format!("{}  {}{}\n", &d.fingerprint[..16.min(d.fingerprint.len())], d.name, if d.connected { "  (connected)" } else { "" })).collect();
                    if devices.is_empty() {
                        out += "no device is trusted\n";
                    }
                    out + if theirs && host.pairing() { "pairing is on: the next unknown device is let in\n" } else { "" }
                }
                "revoke" => {
                    // A fingerprint, or enough of its start to mean one device.
                    let start = words.next().unwrap_or_default();
                    let found: Vec<_> = devices.into_iter().filter(|d| start.len() >= 6 && d.fingerprint.starts_with(start)).collect();
                    match found.as_slice() {
                        [d] => match host.revoke(&d.fingerprint) {
                            Ok(_) => {
                                // Paired again, it is whoever's pairs it then.
                                shared.users.set(&d.fingerprint, None);
                                if let Some(pairing) = pairing.as_mut() {
                                    pairing.known.retain(|known| *known != d.fingerprint);
                                }
                                format!("{} is no longer trusted{}\n", d.name, if d.connected { " and was dropped" } else { "" })
                            }
                            Err(e) => format!("the list couldn't be saved: {e:#}\n"),
                        },
                        [] => "revoke FINGERPRINT: no trusted device starts with that (six characters or more; see devices)\n".into(),
                        _ => "revoke FINGERPRINT: more than one device starts with that\n".into(),
                    }
                }
                "drop" => {
                    let may = admin || shared.state.lock().unwrap().client_user == Some(uid);
                    if may && host.drop_client() { "the client was dropped\n".into() } else { "no client is connected\n".into() }
                }
                _ => match words.next() {
                    // The device that comes is its opener's; root's and the daemon's own
                    // user's get no user of their own, which makes them the `--user` user's.
                    Some(how @ ("on" | "ask")) => {
                        if !theirs && (host.pairing() || host.pending().is_some()) {
                            return "another user is pairing a device with this computer right now\n".into();
                        }
                        *pairing = Some(Pairing { opener: uid, by: (!admin).then_some(uid), known: host.devices().into_iter().map(|d| d.fingerprint).collect() });
                        // `ask`: the device that comes is held until its owner has said yes
                        // (`pair accept`); who it is and its code are in `owner`.
                        host.set_confirm(how == "ask");
                        host.set_pairing(true);
                        if how == "ask" { "ok\n".into() } else { "pairing is on: the next unknown device is let in and remembered\n".into() }
                    }
                    Some("off") => {
                        if theirs {
                            host.set_pairing(false);
                            host.decide(false);
                        }
                        "pairing is off\n".into()
                    }
                    Some(answer @ ("accept" | "decline")) => {
                        let Some(device) = host.pending().filter(|_| theirs) else { return "no device is waiting\n".into() };
                        if answer == "decline" {
                            return if host.decide(false) { "ok\n".into() } else { "no device is waiting\n".into() };
                        }
                        // Whose it is is written down before it is trusted, so there is no
                        // moment at which it is trusted and anybody else's.
                        let by = pairing.as_ref().and_then(|p| p.by);
                        let was = shared.users.set(&device.fingerprint, by);
                        if host.decide(true) {
                            "ok\n".into()
                        } else {
                            shared.users.set(&device.fingerprint, was);
                            "no device is waiting\n".into()
                        }
                    }
                    _ => "pair on|ask|off|accept|decline\n".into(),
                },
            }
        }
        other => format!("unknown command {other}: status, owner, sharing on|off, switch N, prefer N, devices, revoke FINGERPRINT, drop, pair on|ask|off|accept|decline\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteman_core::host::{Host, HostOptions};
    use remoteman_core::{Chroma, Codec, Mode};
    use std::time::Instant;

    const ALICE: u32 = 2001;
    const BOB: u32 = 2002;

    fn fp(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn device(c: char) -> ClientInfo {
        serde_json::from_value(serde_json::json!({ "name": format!("device {c}"), "fingerprint": fp(c), "display": { "width": 1280, "height": 720, "scale": 1.0, "refresh": 60 }, "decoders": [], "cursor": false })).unwrap()
    }

    fn accept(width: u32) -> Accept {
        Accept { mode: Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 }, width, height: 720, refresh: 60, colour: "bt709".into(), sdr_white: 0, peak: 0 }
    }

    /// An agent's end of the socket, attached as `uid` (which only the kernel can say for a
    /// real one). Reads from it give up after two seconds.
    fn attach(shared: &Arc<Shared>, uid: u32) -> UnixStream {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let before = shared.state.lock().unwrap().last_id;
        let s = shared.clone();
        std::thread::spawn(move || serve(s, ours, uid, 1));
        ToDaemon::Hello { features: vec![] }.write(&mut &theirs).unwrap();
        until(|| shared.state.lock().unwrap().last_id > before);
        theirs.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        theirs
    }

    fn until(mut what: impl FnMut() -> bool) {
        let start = Instant::now();
        while !what() {
            assert!(start.elapsed() < Duration::from_secs(5), "it never happened");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A device arriving, on a thread as the core's handler thread would be.
    fn arrive(shared: &Arc<Shared>, c: char) -> std::thread::JoinHandle<Result<Accept, String>> {
        let shared = shared.clone();
        std::thread::spawn(move || Broker(shared).connected(&device(c), Link::pipe(0).0))
    }

    /// The device an agent is asked to serve next; `None` if it is asked nothing for 300 ms.
    fn asked(agent: &UnixStream) -> Option<String> {
        agent.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let got = match ToAgent::read(&mut &*agent) {
            Ok(Some(ToAgent::Client { info, .. })) => Some(info.name),
            Ok(Some(other)) => panic!("an agent was told {other:?}"),
            _ => None,
        };
        agent.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        got
    }

    fn told_left(agent: &UnixStream) {
        assert!(matches!(ToAgent::read(&mut &*agent), Ok(Some(ToAgent::Left))));
    }

    /// The device arrives and the agent that should get it serves it.
    fn served(shared: &Arc<Shared>, c: char, agent: &UnixStream) {
        let arriving = arrive(shared, c);
        assert_eq!(asked(agent), Some(format!("device {c}")));
        ToDaemon::Accepted(accept(1280)).write(&mut &*agent).unwrap();
        assert_eq!(arriving.join().unwrap().unwrap().width, 1280);
    }

    #[test]
    fn a_device_is_handed_only_to_its_own_users_session() {
        // The `--user` user is alice: a device with no user kept is hers.
        let shared = Shared::new(Some(("alice".into(), ALICE)), None, None, vec![]);
        shared.users.set(&fp('b'), Some(BOB));
        let alice = attach(&shared, ALICE);
        let bob = attach(&shared, BOB);

        served(&shared, 'a', &alice);
        // Moving it to bob's session is refused, whoever asks; so is preferring bob's.
        assert_eq!(begin_switch(&shared, 2), Err("that session is another user's than the connected device's"));
        Broker(shared.clone()).left();
        told_left(&alice);
        assert!(control(&shared, "prefer 2", shared.euid).starts_with("the next client goes to agent 2"));
        served(&shared, 'a', &alice);
        Broker(shared.clone()).left();
        told_left(&alice);
        // Bob's device goes to bob, though alice's agent is there too.
        served(&shared, 'b', &bob);
        Broker(shared.clone()).left();
        told_left(&bob);
        assert_eq!(asked(&bob), None);

        // Bob logs out. His device is turned away; alice's session never hears of it.
        drop(bob);
        until(|| shared.agent_of(BOB).is_none());
        let refused = arrive(&shared, 'b').join().unwrap().unwrap_err();
        assert!(refused.contains("isn't logged in on this computer"), "{refused}");
        assert_eq!(asked(&alice), None);
    }

    #[test]
    fn an_answer_for_a_device_that_left_is_not_taken_for_the_next_devices() {
        let shared = Shared::new(None, None, None, vec![]);
        let first = attach(&shared, ALICE);
        served(&shared, 'a', &first);
        // The device is being moved to a second session of its user's when it goes.
        let second = attach(&shared, ALICE);
        begin_switch(&shared, 2).unwrap();
        assert_eq!(asked(&second), Some("device a".into()));
        Broker(shared.clone()).left();
        told_left(&second);
        // The next device arrives before that session has answered for the first. Its
        // answers come in the order it was asked.
        let arriving = arrive(&shared, 'b');
        assert_eq!(asked(&second), Some("device b".into()));
        ToDaemon::Accepted(accept(111)).write(&mut &second).unwrap();
        ToDaemon::Accepted(accept(222)).write(&mut &second).unwrap();
        assert_eq!(arriving.join().unwrap().unwrap().width, 222);
        // And the session serving it was not told that it had gone.
        assert_eq!(asked(&second), None);
        assert_eq!(shared.state.lock().unwrap().current, Some(2));
    }

    #[test]
    fn an_agent_that_answers_too_late_is_not_left_marked_as_serving() {
        let shared = Shared::new(None, None, None, vec![]);
        let agent = attach(&shared, ALICE);
        let arriving = arrive(&shared, 'a');
        assert_eq!(asked(&agent), Some("device a".into()));
        assert!(arriving.join().unwrap().unwrap_err().contains("didn't answer"));
        told_left(&agent);
        ToDaemon::Accepted(accept(111)).write(&mut &agent).unwrap();
        // The next device gets the answer meant for it.
        let arriving = arrive(&shared, 'b');
        assert_eq!(asked(&agent), Some("device b".into()));
        ToDaemon::Accepted(accept(222)).write(&mut &agent).unwrap();
        assert_eq!(arriving.join().unwrap().unwrap().width, 222);
    }

    #[test]
    fn an_agent_that_stops_reading_is_given_up_on() {
        let shared = Shared::new(None, None, None, vec![]);
        let agent = attach(&shared, ALICE);
        served(&shared, 'a', &agent);
        // Far more than its socket holds, on the thread everything of the client's passes
        // through; the agent reads none of it.
        let (done, finished) = mpsc::channel();
        let s = shared.clone();
        std::thread::spawn(move || {
            let mut broker = Broker(s);
            for _ in 0..200 {
                broker.blob(&BlobEvent::Data { id: 1, bytes: vec![0; 100_000] });
            }
            let _ = done.send(());
        });
        assert!(finished.recv_timeout(Duration::from_secs(5)).is_ok(), "the client's thread is stuck behind an agent that doesn't read");
        until(|| shared.state.lock().unwrap().agents.is_empty());
        drop(agent);
    }

    #[test]
    fn a_connection_that_says_nothing_does_not_keep_its_thread() {
        let shared = Shared::new(None, None, None, vec![]);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            serve(shared, ours, ALICE, 1);
            let _ = done.send(());
        });
        assert!(finished.recv_timeout(Duration::from_secs(3)).is_ok());
        drop(theirs);
    }

    #[test]
    fn a_client_whose_agent_went_is_taken_up_by_its_users_next_agent_or_dropped() {
        let shared = Shared::new(None, None, None, vec![]);
        shared.users.set(&fp('a'), Some(ALICE));
        let agent = attach(&shared, ALICE);
        served(&shared, 'a', &agent);
        drop(agent);
        until(|| shared.state.lock().unwrap().agents.is_empty());
        // Somebody else's agent attaching is not given it; alice's next one is.
        let bob = attach(&shared, BOB);
        assert_eq!(asked(&bob), None);
        let again = attach(&shared, ALICE);
        assert_eq!(asked(&again), Some("device a".into()));
        ToDaemon::Accepted(accept(1280)).write(&mut &again).unwrap();
        until(|| shared.state.lock().unwrap().current == Some(3));
        std::thread::sleep(ORPHAN + Duration::from_millis(500));
        assert_eq!(shared.state.lock().unwrap().orphans_dropped, 0);
        // With nobody to take it up, it is dropped after a while.
        drop(again);
        until(|| shared.state.lock().unwrap().orphans_dropped == 1);
    }

    #[test]
    fn each_user_sees_and_changes_only_their_own_devices() {
        let state = std::env::temp_dir().join(format!("remotemand-ctl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(state.join("host")).unwrap();
        // Three paired devices; `c` from before users were kept.
        std::fs::write(state.join("host/trusted"), format!("{} one\n{} two\n{} three\n", fp('a'), fp('b'), fp('c'))).unwrap();
        std::fs::write(state.join("device-users"), format!("{} {ALICE}\n{} {BOB}\n", fp('a'), fp('b'))).unwrap();
        let shared = Shared::new(Some(("carol".into(), 2003)), None, Some(&state), vec![ALICE, BOB]);
        let options = HostOptions { listen: "127.0.0.1:0".parse().unwrap(), state: state.clone(), name: "test".into(), os: "linux".into(), features: vec![], pair: false, test_drop_one_in: None };
        let host = Arc::new(Host::start(options, Broker(shared.clone())).unwrap());
        shared.listening(&host);
        let _alice = attach(&shared, ALICE);
        let _bob = attach(&shared, BOB);
        let names = |uid: u32| -> Vec<String> {
            let said: serde_json::Value = serde_json::from_str(&control(&shared, "owner", uid)).unwrap();
            said["devices"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_owned()).collect()
        };
        assert_eq!(names(ALICE), ["one"]);
        assert_eq!(names(BOB), ["two"]);
        // The `--user` user has the devices nobody else has; root has them all.
        assert_eq!(names(2003), ["three"]);
        assert_eq!(names(shared.euid), ["one", "two", "three"]);
        assert_eq!(control(&shared, "devices", ALICE), format!("{}  one\n", &fp('a')[..16]));
        // Somebody with no session here is told nothing; nor is an account that has an agent
        // attached (anything may connect and call itself one) but is no person.
        assert!(control(&shared, "owner", 2009).starts_with("only root"));
        assert!(control(&shared, "devices", 2009).starts_with("only root"));
        let _service = attach(&shared, 2009);
        assert!(control(&shared, "owner", 2009).starts_with("only root"));
        assert!(control(&shared, "pair on", 2009).starts_with("only root"));
        assert!(!host.pairing());

        // Alice can't revoke bob's device, and can her own.
        assert!(control(&shared, &format!("revoke {}", fp('b')), ALICE).starts_with("revoke FINGERPRINT: no trusted device"));
        assert_eq!(names(BOB), ["two"]);
        assert_eq!(control(&shared, &format!("revoke {}", fp('a')), ALICE), "one is no longer trusted\n");
        assert_eq!(names(ALICE), [""; 0]);
        assert_eq!(shared.users.get(&fp('a')), None);

        // Sharing is each user's own.
        assert_eq!(control(&shared, "sharing off", BOB), "ok\n");
        assert!(shared.users.off(Some(BOB)) && !shared.users.off(Some(ALICE)) && !shared.users.off(Some(2003)));
        assert!(control(&shared, "owner", BOB).contains("\"paused\":true"));
        assert!(control(&shared, "owner", ALICE).contains("\"paused\":false"));
        let refused = Broker(shared.clone()).connected(&device('b'), Link::pipe(0).0).unwrap_err();
        assert!(refused.contains("sharing is switched off"), "{refused}");
        assert_eq!(control(&shared, "sharing on", BOB), "ok\n");

        // Pairing that alice opened is hers: bob sees none, can't answer it, open his own
        // over it or close it.
        assert_eq!(control(&shared, "pair ask", ALICE), "ok\n");
        assert!(control(&shared, "owner", ALICE).contains("\"pairing\":true"));
        assert!(control(&shared, "owner", BOB).contains("\"pairing\":false"));
        assert!(control(&shared, "pair on", BOB).starts_with("another user is pairing"));
        assert_eq!(control(&shared, "pair accept", BOB), "no device is waiting\n");
        assert_eq!(control(&shared, "pair off", BOB), "pairing is off\n");
        assert!(host.pairing());
        assert_eq!(control(&shared, "pair off", ALICE), "pairing is off\n");
        assert!(!host.pairing());
        drop(host);
        let _ = std::fs::remove_dir_all(&state);
    }
}
