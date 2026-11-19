//! Computers on this network that say they run remoteman, and waking one that is asleep.
//!
//! Hosts advertise a DNS-SD service (`_remoteman._udp`, docs/PROTOCOL.md, "Finding a host");
//! this asks the system's Avahi daemon, over D-Bus, to browse for it. What is found is only
//! ever "seen on this network now": a paired computer that isn't found may be reached another
//! way (a VPN, another network), so not finding it says nothing.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

use crate::computers;
use crate::window::App;

const AVAHI: &str = "org.freedesktop.Avahi";
const SERVICE: &str = "_remoteman._udp";

/// One advertisement, on one network interface and protocol.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The interface and protocol it was seen on, as Avahi numbers them.
    on: (i32, i32),
    pub name: String,
    /// `address:port`, as the core is given it.
    pub address: String,
    /// The start of the host key's fingerprint, in lower case.
    pub key: String,
    pub os: String,
    /// The hardware addresses of its network cards.
    pub cards: Vec<String>,
}

/// What a host's text records say: its key's start, what it runs, its cards.
fn read_records(records: &[Vec<u8>]) -> (String, String, Vec<String>) {
    let (mut key, mut os, mut cards) = (String::new(), String::new(), Vec::new());
    for record in records {
        let text = String::from_utf8_lossy(record);
        match text.split_once('=') {
            Some(("fp", v)) => key = v.to_ascii_lowercase(),
            Some(("os", v)) => os = v.to_owned(),
            Some(("mac", v)) => cards = v.split(',').map(|c| c.trim().to_ascii_lowercase()).filter(|c| card_bytes(c).is_some()).collect(),
            _ => {}
        }
    }
    (key, os, cards)
}

/// A hardware address (`aa:bb:cc:dd:ee:ff`) as its six bytes.
fn card_bytes(card: &str) -> Option<[u8; 6]> {
    let mut bytes = [0u8; 6];
    let mut parts = card.split(':');
    for byte in &mut bytes {
        *byte = u8::from_str_radix(parts.next().filter(|p| p.len() == 2)?, 16).ok()?;
    }
    parts.next().is_none().then_some(bytes)
}

/// The packet that wakes a computer whose network card listens for it: six bytes of 0xFF,
/// then the card's address sixteen times.
fn magic(card: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0xFF; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&card);
    }
    packet
}

/// Send the waking packet for each of these cards to `to` (the network's broadcast address,
/// for real use). Returns how many cards it was sent for.
fn wake_at(cards: &[String], to: std::net::SocketAddr) -> usize {
    let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else { return 0 };
    let _ = socket.set_broadcast(true);
    cards.iter().filter_map(|c| card_bytes(c)).filter(|card| socket.send_to(&magic(*card), to).is_ok()).count()
}

/// Wake the computer with these network cards, if it is asleep on this network: the packet
/// goes to everything on the network, on the two ports such packets are sent to by custom.
pub fn wake(cards: &[String]) -> bool {
    let sent: usize = [9u16, 7].iter().map(|port| wake_at(cards, (std::net::Ipv4Addr::BROADCAST, *port).into())).sum();
    sent > 0
}

/// Which of several sightings of one computer to connect to: an IPv4 address before an IPv6
/// one, and never a link-local one (it needs an interface named with it).
fn better(a: &Found, b: &Found) -> bool {
    let rank = |f: &Found| if f.address.starts_with("[fe80") || f.address.starts_with("fe80") { 2 } else if f.on.1 == 0 { 0 } else { 1 };
    rank(a) < rank(b)
}

/// One entry for each computer found, the best address of each.
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

/// Start looking, for as long as the program runs. The list is `App::nearby`; the window's
/// list of computers is redrawn when it changes. Without Avahi nothing is ever found.
#[allow(deprecated)]
pub fn watch(app: &Rc<App>) {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::System, gio::Cancellable::NONE) else { return };
    // Listening comes first: the first sightings are sent as soon as browsing starts.
    let weak = Rc::downgrade(app);
    let resolver = bus.clone();
    bus.signal_subscribe(Some(AVAHI), Some("org.freedesktop.Avahi.ServiceBrowser"), None, None, None, gio::DBusSignalFlags::NONE, move |_, _, _, _, signal, said| {
        let Some(app) = weak.upgrade() else { return };
        let Some((interface, protocol, name, kind, domain, _)) = said.get::<(i32, i32, String, String, String, u32)>() else { return };
        if kind != SERVICE {
            return;
        }
        match signal {
            "ItemNew" => {
                // Where it is and what it says of itself.
                let app = app.clone();
                let ask = (interface, protocol, name.as_str(), kind.as_str(), domain.as_str(), -1i32, 0u32).to_variant();
                resolver.call(Some(AVAHI), "/", "org.freedesktop.Avahi.Server", "ResolveService", Some(&ask), None, gio::DBusCallFlags::NONE, 5000, gio::Cancellable::NONE, move |answer| {
                    type Resolved = (i32, i32, String, String, String, String, i32, String, u16, Vec<Vec<u8>>, u32);
                    let Some((interface, protocol, name, _, _, _, _, address, port, records, _)) = answer.ok().and_then(|a| a.get::<Resolved>()) else { return };
                    let (key, os, cards) = read_records(&records);
                    let address = if address.contains(':') { format!("[{address}]:{port}") } else { format!("{address}:{port}") };
                    let found = Found { on: (interface, protocol), name, address, key, os, cards };
                    {
                        let mut nearby = app.nearby.borrow_mut();
                        nearby.retain(|f| !(f.on == found.on && f.name == found.name));
                        nearby.push(found);
                    }
                    app.nearby_changed();
                });
            }
            "ItemRemove" => {
                app.nearby.borrow_mut().retain(|f| !(f.on == (interface, protocol) && f.name == name));
                app.nearby_changed();
            }
            _ => {}
        }
    });
    let browse = (-1i32, -1i32, SERVICE, "", 0u32).to_variant();
    // The browser lives as long as this program's connection to the bus.
    if let Err(e) = bus.call_sync(Some(AVAHI), "/", "org.freedesktop.Avahi.Server", "ServiceBrowserNew", Some(&browse), None, gio::DBusCallFlags::NONE, 3000, gio::Cancellable::NONE) {
        eprintln!("remoteman: not looking for computers on this network ({})", e.message());
    }
}

impl App {
    /// A paired computer seen on this network now: the sighting whose key is the start of
    /// the key this device has for it.
    pub fn seen(&self, address: &str) -> Option<Found> {
        let key = self.store.borrow().key(address)?.to_ascii_lowercase();
        self.nearby.borrow().iter().find(|f| !f.key.is_empty() && key.starts_with(&f.key)).cloned()
    }

    /// Remember the network cards of paired computers that are in sight, to wake them
    /// another day.
    pub fn remember_cards(&self) {
        let paired: Vec<String> = self.store.borrow().computers.iter().map(|c| c.address.clone()).collect();
        for address in paired {
            if let Some(found) = self.seen(&address).filter(|f| !f.cards.is_empty()) {
                let cards = found.cards.join(",");
                if self.store.borrow().find(&address).is_some_and(|c| c.cards != cards) {
                    self.store.borrow_mut().update(&address, |c| c.cards = cards);
                }
            }
        }
    }

    /// What was found changed: show the list anew.
    pub fn nearby_changed(self: &Rc<Self>) {
        computers::refresh(self);
    }

    /// Try to wake a computer, then connect to it once it has had time to come up.
    pub fn wake(self: &Rc<Self>, address: &str) {
        let cards: Vec<String> = self.store.borrow().find(address).map(|c| c.cards.split(',').map(str::to_owned).collect()).unwrap_or_default();
        let name = self.name_of(address);
        if !wake(&cards) {
            self.toast(&format!("{name} can't be woken from here: the waking packet couldn't be sent"));
            return;
        }
        self.toast(&format!("Waking {name}: connecting in ten seconds"));
        self.reach.borrow_mut().remove(address);
        computers::refresh(self);
        let (app, target) = (self.clone(), self.target_of(address));
        glib::timeout_add_seconds_local_once(10, move || {
            if !app.streaming() {
                app.connect(target, false);
            }
        });
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
        // Sent, and received whole, on this computer alone.
        let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        listener.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        assert_eq!(wake_at(&["00:1a:2b:3c:4d:5e".into(), "not a card".into()], listener.local_addr().unwrap()), 1);
        let mut got = [0u8; 200];
        let (n, _) = listener.recv_from(&mut got).unwrap();
        assert_eq!(&got[..n], &packet[..]);
    }

    #[test]
    fn records_and_the_best_address() {
        let (key, os, cards) = read_records(&[b"v=0".to_vec(), b"fp=841C275C001DB54D".to_vec(), b"os=linux".to_vec(), b"mac=AA:BB:CC:DD:EE:FF,junk,00:11:22:33:44:55".to_vec()]);
        assert_eq!((key.as_str(), os.as_str()), ("841c275c001db54d", "linux"));
        assert_eq!(cards, ["aa:bb:cc:dd:ee:ff", "00:11:22:33:44:55"]);
        let at = |on: (i32, i32), address: &str| Found { on, name: "desk".into(), address: address.into(), key: key.clone(), os: os.clone(), cards: cards.clone() };
        // Protocol 0 is IPv4, 1 is IPv6.
        let all = [at((2, 1), "[fe80::1]:7791"), at((2, 1), "[fd00::2]:7791"), at((2, 0), "192.168.1.5:7791"), at((3, 1), "[fd7a::9]:7791")];
        let found = computers_found(&all);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].address, "192.168.1.5:7791");
        assert_eq!(computers_found(&all[..2])[0].address, "[fd00::2]:7791");
    }
}
