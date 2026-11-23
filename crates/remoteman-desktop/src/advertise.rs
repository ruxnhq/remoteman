//! Telling the local network that this computer runs remoteman, so viewers can list it without
//! being given an address: a DNS-SD service (`_remoteman._udp`) published through the system's
//! Avahi daemon over D-Bus. Avahi answers the queries, renames on a clash of names with another
//! computer, and withdraws the service when this program's connection to it ends, so a host
//! that stops or dies is gone from viewers' lists by itself.
//!
//! What is said (docs/PROTOCOL.md, "Finding a host"): the computer's name, the port, and text
//! records with the protocol version, the start of the host key's fingerprint (so a viewer
//! recognises a computer it is paired with), the operating system, and the hardware addresses
//! of this computer's network cards, which is what a viewer needs to wake it over the network.

use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

pub const SERVICE: &str = "_remoteman._udp";

/// The advertisement, for as long as this lives.
pub struct Advert {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// The hardware addresses of this computer's own network cards (not bridges, tunnels or the
/// virtual ones containers make), for waking it.
pub fn hardware_addresses() -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        // Only a real card has a device behind it.
        .filter(|e| e.path().join("device").exists())
        .filter_map(|e| std::fs::read_to_string(e.path().join("address")).ok())
        .map(|a| a.trim().to_lowercase())
        .filter(|a| a.len() == 17 && a != "00:00:00:00:00:00")
        // An address someone made up (the "locally administered" bit: bridges, virtual cards,
        // randomised Wi-Fi addresses) is no card a waking packet could reach.
        .filter(|a| u8::from_str_radix(&a[..2], 16).is_ok_and(|first| first & 0x02 == 0))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The text records for a host.
pub fn records(fingerprint: &str, os: &str) -> Vec<String> {
    let mut records = vec!["v=0".to_owned(), format!("fp={}", fingerprint.chars().take(16).collect::<String>()), format!("os={os}")];
    let cards = hardware_addresses();
    if !cards.is_empty() {
        records.push(format!("mac={}", cards.join(",")));
    }
    records
}

impl Advert {
    /// Advertise `name` at `port` with these text records: through Avahi where it runs, else
    /// by answering the network's queries ourselves (a fresh Arch or Plasma install has Avahi
    /// but doesn't start it), handing over to Avahi if it starts later, so that the computer
    /// can be found without anyone opening a terminal. Fails only if neither can be done.
    pub fn start(name: &str, port: u16, records: Vec<String>) -> Result<Advert> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<&'static str>>();
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let name = name.to_owned();
        let thread = std::thread::Builder::new()
            .name("advertise".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(r) => r,
                    Err(e) => return drop(ready_tx.send(Err(e.into()))),
                };
                runtime.block_on(async move {
                    let mut told = Some(ready_tx);
                    loop {
                        // The connection is what keeps the service published: it is held
                        // until told to stop.
                        // (For tests: REMOTEMAN_TEST_NO_AVAHI acts as if Avahi weren't there.)
                        let avahi = if std::env::var_os("REMOTEMAN_TEST_NO_AVAHI").is_some() { Err(anyhow::anyhow!("left out for a test")) } else { publish(&name, port, &records).await };
                        if let Ok(_connection) = avahi {
                            if let Some(t) = told.take() {
                                let _ = t.send(Ok("Avahi"));
                            } else {
                                tracing::info!("Avahi has started: it advertises this host from now on");
                            }
                            let _ = (&mut stopped).await;
                            return;
                        }
                        let own = match OwnResponder::start(&name, port, &records) {
                            Ok(own) => own,
                            Err(e) => {
                                if let Some(t) = told.take() {
                                    let _ = t.send(Err(e));
                                }
                                return;
                            }
                        };
                        if let Some(t) = told.take() {
                            let _ = t.send(Ok("this program itself (Avahi isn't running)"));
                        }
                        // Until stopped, or until Avahi turns up to take over.
                        loop {
                            tokio::select! {
                                _ = &mut stopped => return drop(own),
                                _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
                            }
                            if std::env::var_os("REMOTEMAN_TEST_NO_AVAHI").is_none() && avahi_running().await {
                                drop(own);
                                break;
                            }
                        }
                    }
                });
            })
            .context("start the advertising thread")?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(by)) => {
                tracing::debug!("advertised by {by}");
                Ok(Advert { stop: Some(stop), thread: Some(thread) })
            }
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("the advertisement didn't start"),
        }
    }
}

/// Whether Avahi is on the system bus now.
async fn avahi_running() -> bool {
    let Ok(conn) = zbus::Connection::system().await else { return false };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return false };
    let Ok(name) = "org.freedesktop.Avahi".try_into() else { return false };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// The same service answered by this program itself over multicast DNS, for as long as this
/// lives.
struct OwnResponder {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl OwnResponder {
    fn start(name: &str, port: u16, records: &[String]) -> Result<Self> {
        let daemon = mdns_sd::ServiceDaemon::new().context("multicast DNS")?;
        // A name of its own for the address records, never the computer's: Avahi starting
        // later would find its own name taken and rename the computer ("NAME-2").
        let host = format!("{}-remoteman", hostname());
        let properties: Vec<(&str, &str)> = records.iter().filter_map(|r| r.split_once('=')).collect();
        let info = mdns_sd::ServiceInfo::new("_remoteman._udp.local.", name, &format!("{host}.local."), "", port, &properties[..])
            .context("the service's description")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).context("answer for this host on the network")?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for OwnResponder {
    fn drop(&mut self) {
        // Said goodbye to, so viewers drop it at once rather than when it times out.
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(std::time::Duration::from_secs(1));
        }
        let _ = self.daemon.shutdown();
    }
}

/// This computer's name as the network knows it, without a domain.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().split('.').next().unwrap_or("").to_owned()).ok().filter(|h| !h.is_empty()).unwrap_or_else(|| "remoteman-host".into())
}

impl Drop for Advert {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn publish(name: &str, port: u16, records: &[String]) -> Result<Connection> {
    const AVAHI: &str = "org.freedesktop.Avahi";
    let connection = Connection::system().await.context("the system bus")?;
    let server = Proxy::new(&connection, AVAHI, "/", "org.freedesktop.Avahi.Server").await?;
    let group: OwnedObjectPath = server.call("EntryGroupNew", &()).await.context("Avahi isn't running")?;
    let group = Proxy::new(&connection, AVAHI, group, "org.freedesktop.Avahi.EntryGroup").await?;
    let text: Vec<Vec<u8>> = records.iter().map(|r| r.as_bytes().to_vec()).collect();
    // Every interface and protocol (-1, -1), no flags, this computer's own host name ("").
    // A name another computer already uses is refused: try again with a number after it.
    let mut said = name.to_owned();
    for n in 2..=9 {
        match group.call::<_, _, ()>("AddService", &(-1i32, -1i32, 0u32, said.as_str(), SERVICE, "", "", port, &text)).await {
            Ok(()) => break,
            Err(e) if n < 9 && e.to_string().contains("Collision") => said = format!("{name} ({n})"),
            Err(e) => return Err(e).context("advertise the host"),
        }
    }
    group.call::<_, _, ()>("Commit", &()).await.context("advertise the host")?;
    Ok(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_say_what_a_viewer_needs() {
        let records = records("841c275c001db54d76a30f819f6310b0e4f9c839", "linux");
        assert_eq!(&records[..3], ["v=0", "fp=841c275c001db54d", "os=linux"]);
        // Every hardware address, if there are any, in the usual form.
        for card in hardware_addresses() {
            assert_eq!(card.split(':').count(), 6, "{card}");
        }
    }
}
