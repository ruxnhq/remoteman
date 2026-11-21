//! Computers on this network that say they run remoteman, and waking one that is asleep. As the
//! Linux viewer's `nearby.rs`, with Windows' own DNS-SD (`DnsServiceBrowse` and
//! `DnsServiceResolve`, Windows 10 1809 and later) where that one asks Avahi.
//!
//! Hosts advertise `_remoteman._udp` (docs/PROTOCOL.md, "Finding a host"). What is found is only
//! ever "seen on this network now": a paired computer that isn't found may be reached another
//! way (a VPN, another network), so not finding it says nothing.
//!
//! Windows' browse is a query that goes on being answered for a while, not a subscription: it is
//! started again every [`ROUND`], every instance it reports is resolved (where it is, its text
//! records), and one that hasn't been reported for [`FORGET`] is taken off the list. A host that
//! says goodbye (a record with no time to live left) goes at once.
//!
//! Windows calls back on threads of its own, while the app lives on its window's thread in a
//! `RefCell`: what is found is kept behind a mutex here, a counter says when it changed, and the
//! app's window is woken to look ([`App::nearby_tick`]).

use std::collections::HashMap;
use std::ffi::c_void;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{DNS_REQUEST_PENDING, ERROR_CANCELLED};
use windows::Win32::NetworkManagement::Dns::*;
use windows::core::{PCWSTR, PWSTR};

use crate::app::{App, Screen};
use crate::features::FEATURES;
use crate::stream::Scene;

/// What is browsed for (`.local`: Windows' browse asks the multicast DNS of this network).
const SERVICE: &str = "_remoteman._udp.local";
/// How often the browse is started again.
const ROUND: Duration = Duration::from_secs(30);
/// An instance not reported for this long is taken to be gone.
const FORGET: Duration = Duration::from_secs(100);
/// An instance resolved this recently is not resolved again when it is reported.
const FRESH: Duration = Duration::from_secs(20);
/// How long a resolve may take.
const RESOLVE_WAIT: Duration = Duration::from_secs(5);
/// How long after the waking packet the computer is tried.
const WAKE_WAIT: Duration = Duration::from_secs(10);

/// One computer seen on this network.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// Its name, as it advertises itself.
    pub name: String,
    /// `address:port`, as the core is given it (empty if it gave no address that can be used).
    pub address: String,
    /// The start of the host key's fingerprint, in lower case.
    pub key: String,
    pub os: String,
    /// The hardware addresses of its network cards.
    pub cards: Vec<String>,
}

/// What a host's text records say: its key's start, what it runs, its cards.
pub fn read_records<'a>(records: impl IntoIterator<Item = (&'a str, &'a str)>) -> (String, String, Vec<String>) {
    let (mut key, mut os, mut cards) = (String::new(), String::new(), Vec::new());
    for (k, v) in records {
        match k.to_ascii_lowercase().as_str() {
            "fp" => key = v.trim().to_ascii_lowercase(),
            "os" => os = v.trim().to_owned(),
            "mac" => cards = v.split(',').map(|c| c.trim().to_ascii_lowercase()).filter(|c| card_bytes(c).is_some()).collect(),
            _ => {}
        }
    }
    (key, os, cards)
}

/// A hardware address (`aa:bb:cc:dd:ee:ff`) as its six bytes.
pub fn card_bytes(card: &str) -> Option<[u8; 6]> {
    let mut bytes = [0u8; 6];
    let mut parts = card.trim().split(':');
    for byte in &mut bytes {
        *byte = u8::from_str_radix(parts.next().filter(|p| p.len() == 2)?, 16).ok()?;
    }
    parts.next().is_none().then_some(bytes)
}

/// The packet that wakes a computer whose network card listens for it: six bytes of 0xFF,
/// then the card's address sixteen times.
pub fn magic(card: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&card);
    }
    packet
}

/// Send the waking packet for each of these cards to `to`, from `from` (a local address, so
/// that a broadcast leaves by that address's network) or from wherever Windows picks. Returns
/// how many were sent.
fn wake_at(cards: &[String], to: SocketAddr, from: Option<Ipv4Addr>) -> usize {
    let Ok(socket) = UdpSocket::bind((from.unwrap_or(Ipv4Addr::UNSPECIFIED), 0)) else { return 0 };
    let _ = socket.set_broadcast(true);
    cards.iter().filter_map(|c| card_bytes(c)).filter(|card| socket.send_to(&magic(*card), to).is_ok()).count()
}

/// Each network this PC is on, by IPv4: its own address there and that network's broadcast
/// address. Not the loopback, not one with no neighbours (a /31 or /32, as a VPN's often is).
fn networks() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    use windows::Win32::NetworkManagement::IpHelper::*;
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
    const LOOPBACK: u32 = 24;
    let mut out = Vec::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_FRIENDLY_NAME;
    let mut size = 16 * 1024u32;
    // SAFETY: the buffer is as large as said (u64s, so aligned for the structures), and the
    // list it holds is walked within it, as GetAdaptersAddresses lays it out.
    unsafe {
        let mut buffer: Vec<u64>;
        loop {
            buffer = vec![0u64; size as usize / 8 + 1];
            let first = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
            match GetAdaptersAddresses(AF_INET.0 as u32, flags, None, Some(first), &mut size) {
                0 => break,
                // ERROR_BUFFER_OVERFLOW: `size` is now what it needs.
                111 => continue,
                _ => return out,
            }
        }
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() {
            let a = &*adapter;
            if a.OperStatus == IfOperStatusUp && a.IfType != LOOPBACK {
                let mut unicast = a.FirstUnicastAddress;
                while !unicast.is_null() {
                    let u = &*unicast;
                    let sa = u.Address.lpSockaddr;
                    if !sa.is_null() && (*sa).sa_family == AF_INET && u.OnLinkPrefixLength <= 30 {
                        let sin = &*(sa as *const SOCKADDR_IN);
                        let ip = Ipv4Addr::from(*(std::ptr::addr_of!(sin.sin_addr) as *const [u8; 4]));
                        let mask = u32::MAX.checked_shl(32 - u.OnLinkPrefixLength as u32).unwrap_or(0);
                        if !ip.is_loopback() && !ip.is_link_local() {
                            out.push((ip, Ipv4Addr::from(u32::from(ip) | !mask)));
                        }
                    }
                    unicast = u.Next;
                }
            }
            adapter = a.Next;
        }
    }
    out
}

/// Wake the computer with these network cards, if it is asleep on a network this PC is on: the
/// packet goes to every computer on each of them (each network's broadcast address, sent from
/// this PC's address there, since one broadcast to 255.255.255.255 leaves by one network only;
/// and to that too), on the two ports such packets go to by custom. Returns how many packets
/// were sent.
pub fn wake(cards: &[String]) -> usize {
    let mut sent = 0;
    for port in [9u16, 7] {
        for (from, broadcast) in networks() {
            sent += wake_at(cards, (broadcast, port).into(), Some(from));
        }
        sent += wake_at(cards, (Ipv4Addr::BROADCAST, port).into(), None);
    }
    sent
}

/// Which of several sightings of one computer to connect to: one with an address before one
/// without, an IPv4 address before an IPv6 one.
fn better(a: &Found, b: &Found) -> bool {
    let rank = |f: &Found| if f.address.is_empty() { 2 } else if f.address.starts_with('[') { 1 } else { 0 };
    rank(a) < rank(b)
}

/// One entry for each computer found, the best address of each, by name.
pub fn computers_found(all: &[Found]) -> Vec<Found> {
    let mut best: Vec<Found> = Vec::new();
    for found in all {
        match best.iter_mut().find(|b| b.name == found.name && b.key == found.key) {
            Some(have) => {
                if better(found, have) {
                    *have = found.clone();
                }
            }
            None => best.push(found.clone()),
        }
    }
    best.sort_by(|a, b| a.name.cmp(&b.name));
    best
}

/// A DNS name's label as people write it: `\032` is a space, `\.` a dot.
fn unescape(label: &str) -> String {
    let mut out = Vec::with_capacity(label.len());
    let bytes = label.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() && bytes[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            let n = (bytes[i + 1] - b'0') as u32 * 100 + (bytes[i + 2] - b'0') as u32 * 10 + (bytes[i + 3] - b'0') as u32;
            out.push(n.min(255) as u8);
            i += 4;
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() {
            out.push(bytes[i + 1]);
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The computer's name in a service instance's full name (`Workshop\032PC._remoteman._udp.local`):
/// its first label, up to the first dot that isn't escaped.
fn instance_name(full: &str) -> String {
    let mut end = full.len();
    let mut escaped = false;
    for (i, c) in full.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '.' => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    unescape(&full[..end])
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A string Windows handed over.
///
/// # Safety
/// `p` is null or a string that ends with a 0.
unsafe fn text(p: PWSTR) -> String {
    if p.is_null() { String::new() } else { unsafe { p.to_string() }.unwrap_or_default() }
}

/// What is known of one service instance.
struct Sighting {
    found: Option<Found>,
    resolved: Option<Instant>,
    seen: Instant,
}

enum Job {
    Resolve(String),
    Gone(String),
}

struct Inner {
    /// By the instance's full name.
    seen: Mutex<HashMap<String, Sighting>>,
    /// Counts the changes to what is found.
    serial: AtomicU32,
    /// From the browse callback to the thread.
    jobs: Mutex<mpsc::Sender<Job>>,
    scene: Arc<Scene>,
    /// Browse failures already said, so each is said once.
    said: Mutex<Vec<u32>>,
    /// What is browsed for ([`SERVICE`], or another service in the tests).
    service: &'static str,
}

/// Looking for computers, for as long as the program runs.
#[derive(Clone, Copy)]
pub struct Nearby {
    inner: &'static Inner,
}

impl Nearby {
    /// Start looking. The app's window is woken whenever what is found changes.
    pub fn start(scene: Arc<Scene>) -> Option<Self> {
        Self::start_for(scene, SERVICE)
    }

    /// Start looking for `service` (`_name._udp.local`).
    fn start_for(scene: Arc<Scene>, service: &'static str) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        // For as long as the program runs: Windows may call back with it until then.
        let inner: &'static Inner = Box::leak(Box::new(Inner { seen: Mutex::default(), serial: AtomicU32::new(0), jobs: Mutex::new(tx), scene, said: Mutex::default(), service }));
        std::thread::Builder::new().name("nearby".into()).spawn(move || run(inner, rx)).ok()?;
        Some(Self { inner })
    }

    /// Counts the changes to what is found.
    pub fn serial(&self) -> u32 {
        self.inner.serial.load(Ordering::Relaxed)
    }

    /// Every computer found and resolved, once each.
    pub fn found(&self) -> Vec<Found> {
        let seen = self.inner.seen.lock().unwrap();
        computers_found(&seen.values().filter_map(|s| s.found.clone()).collect::<Vec<_>>())
    }
}

/// Windows' answer to the browse: the instances that are there (and those saying goodbye).
unsafe extern "system" fn browsed(status: u32, context: *const c_void, records: *const DNS_RECORDW) {
    // SAFETY: the context is the leaked `Inner`; the records are Windows' list, read and then
    // given back to it.
    unsafe {
        let inner = &*(context as *const Inner);
        let mut r = records;
        while !r.is_null() {
            let record = &*r;
            if record.wType == DNS_TYPE_PTR.0 && text(record.pName).trim_end_matches('.').eq_ignore_ascii_case(inner.service) {
                let instance = text(record.Data.PTR.pNameHost);
                if !instance.is_empty() {
                    let job = if record.dwTtl == 0 { Job::Gone(instance) } else { Job::Resolve(instance) };
                    let _ = inner.jobs.lock().unwrap().send(job);
                }
            }
            r = record.pNext;
        }
        if !records.is_null() {
            DnsFree(Some(records as *const c_void), DnsFreeRecordList);
        }
        if status != 0 && status != ERROR_CANCELLED.0 {
            let mut said = inner.said.lock().unwrap();
            if !said.contains(&status) {
                said.push(status);
                say!("nearby: the browse ended with {status}");
            }
        }
    }
}

/// One resolve under way: where its answer goes. Its memory belongs to the resolve until
/// Windows has called back once.
struct Resolving {
    name: Vec<u16>,
    answer: Mutex<Option<mpsc::Sender<Option<Found>>>>,
}

unsafe extern "system" fn resolved(status: u32, context: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    // SAFETY: the context is the `Resolving` handed to Windows with this resolve, taken back
    // here (Windows calls back once); the instance is Windows', read and given back.
    unsafe {
        let resolving = Arc::from_raw(context as *const Resolving);
        let found = (status == 0 && !instance.is_null()).then(|| read_instance(&*instance));
        if !instance.is_null() {
            DnsServiceFreeInstance(instance);
        }
        if let Some(answer) = resolving.answer.lock().unwrap().take() {
            let _ = answer.send(found);
        }
    }
}

/// What a resolved instance says.
///
/// # Safety
/// `i` is an instance as Windows hands it to a resolve's callback.
unsafe fn read_instance(i: &DNS_SERVICE_INSTANCE) -> Found {
    // SAFETY: the instance's own pointers and counts.
    unsafe {
        let name = instance_name(&text(i.pszInstanceName));
        let mut pairs = Vec::new();
        for n in 0..i.dwPropertyCount as usize {
            let key = if i.keys.is_null() { String::new() } else { text(*i.keys.add(n)) };
            let value = if i.values.is_null() { String::new() } else { text(*i.values.add(n)) };
            pairs.push((key, value));
        }
        let (key, os, cards) = read_records(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let port = i.wPort;
        let v4 = (!i.ip4Address.is_null()).then(|| Ipv4Addr::from((*i.ip4Address).to_ne_bytes())).filter(|a| !a.is_unspecified());
        let v6 = (!i.ip6Address.is_null()).then(|| Ipv6Addr::from((*i.ip6Address).IP6Byte)).filter(|a| !a.is_unspecified() && !a.is_unicast_link_local());
        let host = text(i.pszHostName);
        // The host's name before any one address: the address the resolve gives is whichever of
        // the computer's Windows picked (seen here: a Windows host's Tailscale address, not the
        // LAN one it advertised), while the core, given the name (`.local` too), tries all of
        // them, the LAN's first.
        let address = match (v4, v6) {
            _ if !host.is_empty() && port != 0 => format!("{}:{port}", host.trim_end_matches('.')),
            (Some(a), _) => format!("{a}:{port}"),
            (None, Some(a)) => format!("[{a}]:{port}"),
            _ => String::new(),
        };
        Found { name, address, key, os, cards }
    }
}

/// Where the instance is and what it says; `None` if it doesn't answer in time.
fn resolve(instance: &str) -> Option<Found> {
    let (tx, rx) = mpsc::channel();
    let resolving = Arc::new(Resolving { name: wide(instance), answer: Mutex::new(Some(tx)) });
    let mut request = DNS_SERVICE_RESOLVE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1.0,
        InterfaceIndex: 0,
        QueryName: PWSTR(resolving.name.as_ptr() as *mut u16),
        pResolveCompletionCallback: Some(resolved),
        pQueryContext: Arc::into_raw(resolving.clone()) as *mut c_void,
    };
    let mut cancel = DNS_SERVICE_CANCEL::default();
    // SAFETY: the request's name and context are the `Resolving`, kept alive by the reference
    // handed over in the context until Windows calls back.
    let status = unsafe { DnsServiceResolve(&mut request, &mut cancel) };
    if status != DNS_REQUEST_PENDING {
        // Not started: Windows won't call back, so the reference handed over comes back here.
        // SAFETY: the context made above, never used by Windows.
        drop(unsafe { Arc::from_raw(request.pQueryContext as *const Resolving) });
        say!("nearby: {instance} can't be resolved ({status})");
        return None;
    }
    match rx.recv_timeout(RESOLVE_WAIT) {
        Ok(found) => found,
        Err(_) => {
            // SAFETY: the resolve this handle is for; it calls back (cancelled) afterwards.
            unsafe { DnsServiceResolveCancel(&cancel) };
            None
        }
    }
}

fn run(inner: &'static Inner, rx: mpsc::Receiver<Job>) {
    let query = wide(inner.service);
    let request = DNS_SERVICE_BROWSE_REQUEST {
        Version: DNS_QUERY_REQUEST_VERSION1.0,
        InterfaceIndex: 0,
        QueryName: PCWSTR(query.as_ptr()),
        Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 { pBrowseCallback: Some(browsed) },
        pQueryContext: inner as *const Inner as *mut c_void,
    };
    let mut cancel: Option<DNS_SERVICE_CANCEL> = None;
    let mut round = Instant::now() - ROUND;
    let changed = || {
        inner.serial.fetch_add(1, Ordering::Relaxed);
        inner.scene.wake_window();
    };
    loop {
        if round.elapsed() >= ROUND {
            round = Instant::now();
            if let Some(cancel) = cancel.take() {
                // SAFETY: the browse this handle is for.
                unsafe { DnsServiceBrowseCancel(&cancel) };
            }
            let mut handle = DNS_SERVICE_CANCEL::default();
            // SAFETY: the request's name lives in this function, which never returns; its
            // context is the leaked `Inner`.
            let status = unsafe { DnsServiceBrowse(&request, &mut handle) };
            if status == DNS_REQUEST_PENDING {
                cancel = Some(handle);
            } else {
                let mut said = inner.said.lock().unwrap();
                if !said.contains(&(status as u32)) {
                    said.push(status as u32);
                    say!("nearby: Windows won't browse for {} ({status})", inner.service);
                }
            }
            // Gone quietly.
            let mut seen = inner.seen.lock().unwrap();
            let before = seen.len();
            seen.retain(|instance, s| {
                let keep = s.seen.elapsed() < FORGET;
                if !keep {
                    say!("nearby: {} not seen for {} s: gone", instance_name(instance), FORGET.as_secs());
                }
                keep
            });
            if seen.len() != before {
                drop(seen);
                changed();
            }
        }
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Job::Resolve(instance)) => {
                // Only an answer to a resolve says it is there: a browse may be answered from
                // what Windows remembers of a computer that has gone without a word.
                let fresh = {
                    let mut seen = inner.seen.lock().unwrap();
                    let s = seen.entry(instance.clone()).or_insert(Sighting { found: None, resolved: None, seen: Instant::now() });
                    s.resolved.is_some_and(|at| at.elapsed() < FRESH)
                };
                if fresh {
                    continue;
                }
                let found = resolve(&instance);
                let mut seen = inner.seen.lock().unwrap();
                let Some(s) = seen.get_mut(&instance) else { continue };
                if let Some(found) = found {
                    s.resolved = Some(Instant::now());
                    s.seen = Instant::now();
                    if s.found.as_ref() != Some(&found) {
                        say!("nearby: {} at {} (key {}, {}, {} network cards)", found.name, if found.address.is_empty() { "no usable address" } else { &found.address }, if found.key.is_empty() { "not said" } else { &found.key }, if found.os.is_empty() { "system not said" } else { &found.os }, found.cards.len());
                        s.found = Some(found);
                        drop(seen);
                        changed();
                    }
                }
            }
            Ok(Job::Gone(instance)) => {
                if inner.seen.lock().unwrap().remove(&instance).is_some() {
                    say!("nearby: {} said goodbye", instance_name(&instance));
                    changed();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// What the app knows of the computers around it, kept up to date by [`App::nearby_tick`].
#[derive(Default)]
pub struct Sightings {
    browser: Option<Nearby>,
    /// The browser's counter when last looked at, and when that was.
    serial: u32,
    looked: Option<Instant>,
    /// Paired computers seen now, by their address in the list.
    pub paired: HashMap<String, Found>,
    /// Computers seen now that this PC isn't paired with.
    pub unpaired: Vec<Found>,
    /// A computer that was sent the waking packet, and when to try it.
    waking: Option<(String, Instant)>,
}

impl Sightings {
    /// Start looking for computers (`None`: for pictures of the window, where nothing real is
    /// to be shown); `service`: another service than remoteman's, for tests (`--browse`).
    pub fn start(scene: Option<Arc<Scene>>, service: Option<&str>) -> Self {
        let browser = match (FEATURES.discovery, scene, service) {
            (true, Some(scene), None) => Nearby::start(scene),
            (true, Some(scene), Some(service)) => {
                say!("nearby: browsing for {service}, not remoteman's service (a test)");
                Nearby::start_for(scene, Box::leak(service.to_owned().into_boxed_str()))
            }
            _ => None,
        };
        Self { browser, ..Self::default() }
    }
}

impl App {
    /// A paired computer seen on this network now.
    pub fn seen(&self, address: &str) -> Option<&Found> {
        self.sightings.paired.get(address)
    }

    /// The computer can be sent the waking packet: its cards are known and that is allowed.
    pub fn wakeable(&self, address: &str) -> bool {
        FEATURES.wake && self.store.find(address).is_some_and(|c| c.wake && !c.cards.is_empty())
    }

    /// Look at what has been found (when it changed, and every few seconds, since pairing and
    /// forgetting change which are paired), remember the cards of paired computers in sight,
    /// and try a computer that was woken once it has had time to come up.
    pub fn nearby_tick(&mut self) {
        let now = Instant::now();
        if let Some((address, at)) = self.sightings.waking.clone() {
            if now >= at {
                self.sightings.waking = None;
                if self.session.is_none() && self.attempt.is_none() {
                    say!("wake: trying {address}");
                    let target = self.target_of(&address);
                    self.connect(target, false);
                }
            }
        }
        let Some(browser) = self.sightings.browser else { return };
        let serial = browser.serial();
        if serial == self.sightings.serial && self.sightings.looked.is_some_and(|at| now - at < Duration::from_secs(3)) {
            return;
        }
        self.sightings.serial = serial;
        self.sightings.looked = Some(now);
        let found = browser.found();
        let keys: Vec<(String, String)> = self.store.computers.iter().filter_map(|c| self.store.key(&c.address).map(|k| (c.address.clone(), k.to_ascii_lowercase()))).collect();
        let mut paired = HashMap::new();
        let mut unpaired = Vec::new();
        for f in found {
            match keys.iter().find(|(_, k)| !f.key.is_empty() && k.starts_with(&f.key)) {
                Some((address, _)) => {
                    paired.insert(address.clone(), f);
                }
                None if !f.address.is_empty() => unpaired.push(f),
                None => {}
            }
        }
        // Remember the cards of those in sight, to wake them another day.
        for (address, f) in &paired {
            let cards = f.cards.join(",");
            if !cards.is_empty() && self.store.find(address).is_some_and(|c| c.cards != cards) {
                say!("wake: {} has the network cards {cards}", f.name);
                self.store.update(address, |c| c.cards = cards.clone());
            }
        }
        if paired != self.sightings.paired || unpaired != self.sightings.unpaired {
            self.sightings.paired = paired;
            self.sightings.unpaired = unpaired;
            self.dirty = true;
        }
    }

    /// Send the computer the waking packet, then try it once it has had time to come up.
    pub fn wake(&mut self, address: &str) {
        let Some(c) = self.store.find(address) else { return };
        let (name, cards, allowed) = (c.name.clone(), c.card_list(), c.wake);
        if !FEATURES.wake || !allowed {
            return self.say(&format!("Waking {name} over the network is switched off in its settings"));
        }
        if cards.is_empty() {
            return self.say(&format!("{name} can't be woken yet: it has to be seen on this network once, for its network card to be known"));
        }
        let sent = wake(&cards);
        say!("wake: {sent} waking packets sent for {}", cards.join(","));
        if sent == 0 {
            return self.say(&format!("{name} can't be woken from here: the waking packet couldn't be sent"));
        }
        self.say(&format!("Waking {name}: connecting in ten seconds"));
        self.reach.remove(address);
        if matches!(self.screen, Screen::Problem(..)) {
            self.screen = Screen::Computers;
        }
        self.sightings.waking = Some((address.to_owned(), Instant::now() + WAKE_WAIT));
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_waking_packet_is_as_cards_expect_it() {
        let card = card_bytes("00:1a:2b:3c:4d:5e").unwrap();
        let packet = magic(card);
        assert_eq!(packet.len(), 102);
        assert_eq!(&packet[..6], [0xFF; 6]);
        assert!(packet[6..].chunks(6).all(|c| c == card));
        assert_eq!(card_bytes("00:1a:2b:3c:4d"), None);
        assert_eq!(card_bytes("00:1a:2b:3c:4d:5e:6f"), None);
        assert_eq!(card_bytes("zz:1a:2b:3c:4d:5e"), None);
        // Sent, and received whole, on this PC alone (a made-up card; the loopback only).
        let listener = UdpSocket::bind("127.0.0.1:0").unwrap();
        listener.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        assert_eq!(wake_at(&["00:1a:2b:3c:4d:5e".into(), "not a card".into()], listener.local_addr().unwrap(), Some(Ipv4Addr::LOCALHOST)), 1);
        let mut got = [0u8; 200];
        let (n, _) = listener.recv_from(&mut got).unwrap();
        assert_eq!(&got[..n], &packet[..]);
    }

    #[test]
    fn records_names_and_the_best_address() {
        let (key, os, cards) = read_records([("v", "0"), ("fp", "841C275C001DB54D"), ("os", "linux"), ("mac", "AA:BB:CC:DD:EE:FF,junk,00:11:22:33:44:55")]);
        assert_eq!((key.as_str(), os.as_str()), ("841c275c001db54d", "linux"));
        assert_eq!(cards, ["aa:bb:cc:dd:ee:ff", "00:11:22:33:44:55"]);
        assert_eq!(instance_name("Workshop\\032PC._remoteman._udp.local"), "Workshop PC");
        assert_eq!(instance_name("Anna\\'s\\.mac._remoteman._UDP.local."), "Anna's.mac");
        assert_eq!(instance_name("blackbox._remoteman._udp.local"), "blackbox");
        let at = |address: &str| Found { name: "desk".into(), address: address.into(), key: key.clone(), os: os.clone(), cards: cards.clone() };
        let found = computers_found(&[at(""), at("[fd00::2]:7791"), at("192.168.1.5:7791")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].address, "192.168.1.5:7791");
        assert_eq!(computers_found(&[at(""), at("[fd00::2]:7791")])[0].address, "[fd00::2]:7791");
    }

    /// This PC's networks, as waking sees them (printed; nothing is sent).
    #[test]
    fn the_networks_have_broadcast_addresses() {
        for (ip, broadcast) in networks() {
            println!("network: {ip} broadcast {broadcast}");
            assert!(!ip.is_loopback());
            assert!(u32::from(broadcast) > u32::from(ip));
        }
    }

    /// A test service type: not remoteman's, so no viewer on the network takes what is
    /// advertised under it for a computer.
    const TEST: &str = "_remoteman-test._udp.local";

    /// An instance advertised through Windows' own DNS-SD until dropped.
    struct Advert {
        request: DNS_SERVICE_REGISTER_REQUEST,
        instance: *mut DNS_SERVICE_INSTANCE,
    }

    impl Drop for Advert {
        fn drop(&mut self) {
            unsafe {
                DnsServiceDeRegister(&self.request, None);
                std::thread::sleep(Duration::from_millis(500));
                DnsServiceFreeInstance(self.instance);
            }
        }
    }

    /// Advertise `name` under [`TEST`] at this PC's first network address, with these text
    /// records.
    fn advertise(name: &str, port: u16, records: &[(&str, &str)]) -> Advert {
        unsafe extern "system" fn registered(status: u32, _: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
            println!("registered: {status}");
            if !instance.is_null() {
                unsafe { DnsServiceFreeInstance(instance) };
            }
        }
        let (ip, _) = networks().into_iter().next().expect("a network");
        let ip4 = u32::from_ne_bytes(ip.octets());
        let host = format!("{}.local", std::env::var("COMPUTERNAME").unwrap_or_else(|_| "test".into()));
        let keys: Vec<Vec<u16>> = records.iter().map(|r| wide(r.0)).collect();
        let values: Vec<Vec<u16>> = records.iter().map(|r| wide(r.1)).collect();
        let key_ptrs: Vec<PCWSTR> = keys.iter().map(|k| PCWSTR(k.as_ptr())).collect();
        let value_ptrs: Vec<PCWSTR> = values.iter().map(|v| PCWSTR(v.as_ptr())).collect();
        let full = wide(&format!("{name}.{TEST}"));
        let host_w = wide(&host);
        unsafe {
            let instance = DnsServiceConstructInstance(PCWSTR(full.as_ptr()), PCWSTR(host_w.as_ptr()), Some(&ip4), None, port, 0, 0, records.len() as u32, key_ptrs.as_ptr(), value_ptrs.as_ptr());
            assert!(!instance.is_null());
            let request = DNS_SERVICE_REGISTER_REQUEST { Version: 1, InterfaceIndex: 0, pServiceInstance: instance, pRegisterCompletionCallback: Some(registered), pQueryContext: std::ptr::null_mut(), ..Default::default() };
            let status = DnsServiceRegister(&request, None);
            println!("register {name}: {status}");
            assert_eq!(status as i32, DNS_REQUEST_PENDING);
            Advert { request, instance }
        }
    }

    /// For the app's own check (`--browse _remoteman-test._udp.local`): advertise, for 60
    /// seconds, one computer that the test state's "Test PC" is paired with (its key starts
    /// with this fp; a made-up card) and one that nothing is paired with.
    /// `cargo test -p remoteman-viewer-windows --lib advertise_for_the_app -- --ignored`
    #[test]
    #[ignore]
    fn advertise_for_the_app() {
        let _paired = advertise("Test PC", 65002, &[("v", "0"), ("fp", "00112233AABBCCDD"), ("os", "linux"), ("mac", "02:00:00:00:00:02")]);
        let _other = advertise("Den laptop", 65003, &[("v", "0"), ("fp", "FFEEDDCCBBAA0099"), ("os", "linux")]);
        std::thread::sleep(Duration::from_secs(60));
    }

    /// Browse and resolve end to end on this PC: an instance of [`TEST`] is advertised through
    /// Windows' own `DnsServiceRegister`, with a name that has a space and the text records a
    /// host gives, and found by the same browser the app runs; then withdrawn.
    /// `cargo test -p remoteman-viewer-windows --lib advertised -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn an_advertised_instance_is_found_and_resolved() {
        let (ip, _) = networks().into_iter().next().expect("a network");
        let advert = advertise("remoteman test PC", 65001, &[("v", "0"), ("fp", "00112233AABBCCDD"), ("os", "windows"), ("mac", "02:00:00:00:00:01,not-a-card")]);
        {
            let nearby = Nearby::start_for(Arc::new(Scene::default()), TEST).unwrap();
            let mut found = Vec::new();
            for _ in 0..30 {
                std::thread::sleep(Duration::from_millis(500));
                found = nearby.found();
                if !found.is_empty() {
                    break;
                }
            }
            drop(advert);
            println!("found: {found:?}");
            assert_eq!(found.len(), 1, "the advertised instance wasn't found");
            let f = &found[0];
            assert_eq!(f.name, "remoteman test PC");
            assert_eq!((f.key.as_str(), f.os.as_str()), ("00112233aabbccdd", "windows"));
            assert_eq!(f.cards, ["02:00:00:00:00:01"]);
            // The advertised host name (this PC's), which the core looks up to all its addresses.
            let at = std::net::ToSocketAddrs::to_socket_addrs(&f.address).expect("a name the core can look up").collect::<Vec<_>>();
            assert!(f.address.ends_with(".local:65001"), "{}", f.address);
            assert!(at.iter().any(|a| a.ip() == ip), "the advertised address {ip} isn't among {at:?}");
            println!("advertised at {ip}, found as {} = {at:?}", f.address);
        }
    }

    /// Through several of the browser's rounds (it browses again every 30 s): an instance
    /// advertised for 160 s stays found the whole time, and is gone soon after it is withdrawn.
    /// `cargo test -p remoteman-viewer-windows --lib through_rounds -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn found_through_rounds_and_gone_after() {
        let advert = advertise("remoteman rounds test", 65004, &[("v", "0"), ("fp", "0123456789ABCDEF"), ("os", "windows")]);
        let nearby = Nearby::start_for(Arc::new(Scene::default()), TEST).unwrap();
        let began = Instant::now();
        let mut first = None;
        let mut missing = Vec::new();
        while began.elapsed() < Duration::from_secs(160) {
            std::thread::sleep(Duration::from_secs(5));
            let there = !nearby.found().is_empty();
            let at = began.elapsed().as_secs();
            match (there, first) {
                (true, None) => first = Some(at),
                (false, Some(_)) => missing.push(at),
                _ => {}
            }
            println!("{at:>4} s: {}", if there { "found" } else { "not found" });
        }
        drop(advert);
        let withdrawn = Instant::now();
        let mut gone = None;
        while withdrawn.elapsed() < Duration::from_secs(120) {
            std::thread::sleep(Duration::from_secs(1));
            if nearby.found().is_empty() {
                gone = Some(withdrawn.elapsed().as_secs());
                break;
            }
        }
        println!("first found at {first:?} s; missing at {missing:?}; gone {gone:?} s after it was withdrawn");
        assert!(first.is_some_and(|s| s <= 10), "not found in its first 10 s");
        assert!(missing.is_empty(), "lost while advertised");
        assert!(gone.is_some(), "still found 2 minutes after it was withdrawn");
    }

    /// The real network: browse for 12 seconds and print what answers, for remoteman and for
    /// every service (so that "none found" can be told from "browsing doesn't work here").
    /// `cargo test -p remoteman-viewer-windows --lib browse -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn browse_this_network() {
        let scene = Arc::new(Scene::default());
        let nearby = Nearby::start(scene).unwrap();
        std::thread::sleep(Duration::from_secs(12));
        let found = nearby.found();
        for f in &found {
            println!("FOUND {} at {} key {} os {} cards {:?}", f.name, f.address, f.key, f.os, f.cards);
        }
        println!("remoteman hosts found: {}", found.len());
        // Every service type on the network, as a check that browsing works at all.
        let (tx, rx) = mpsc::channel::<String>();
        struct Ctx(Mutex<mpsc::Sender<String>>);
        unsafe extern "system" fn any(_: u32, context: *const c_void, records: *const DNS_RECORDW) {
            unsafe {
                let ctx = &*(context as *const Ctx);
                let mut r = records;
                while !r.is_null() {
                    let record = &*r;
                    if record.wType == DNS_TYPE_PTR.0 {
                        let _ = ctx.0.lock().unwrap().send(text(record.Data.PTR.pNameHost));
                    }
                    r = record.pNext;
                }
                if !records.is_null() {
                    DnsFree(Some(records as *const c_void), DnsFreeRecordList);
                }
            }
        }
        let ctx: &'static Ctx = Box::leak(Box::new(Ctx(Mutex::new(tx))));
        let query = wide("_services._dns-sd._udp.local");
        let request = DNS_SERVICE_BROWSE_REQUEST { Version: 1, InterfaceIndex: 0, QueryName: PCWSTR(query.as_ptr()), Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 { pBrowseCallback: Some(any) }, pQueryContext: ctx as *const Ctx as *mut c_void };
        let mut cancel = DNS_SERVICE_CANCEL::default();
        let status = unsafe { DnsServiceBrowse(&request, &mut cancel) };
        println!("service-type browse: {status}");
        std::thread::sleep(Duration::from_secs(6));
        unsafe { DnsServiceBrowseCancel(&cancel) };
        let mut types: Vec<String> = rx.try_iter().collect();
        types.sort();
        types.dedup();
        println!("service types answering on this network: {}", types.len());
        for t in types {
            println!("TYPE {t}");
        }
    }
}
