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
    /// Advertise `name` at `port` with these text records. Fails if there is no Avahi to ask.
    pub fn start(name: &str, port: u16, records: Vec<String>) -> Result<Advert> {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let name = name.to_owned();
        let thread = std::thread::Builder::new()
            .name("advertise".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(r) => r,
                    Err(e) => return drop(ready_tx.send(Err(e.into()))),
                };
                runtime.block_on(async move {
                    // The connection is what keeps the service published: it is held until told to stop.
                    match publish(&name, port, &records).await {
                        Ok(_connection) => {
                            let _ = ready_tx.send(Ok(()));
                            let _ = stopped.await;
                        }
                        Err(e) => drop(ready_tx.send(Err(e))),
                    }
                });
            })
            .context("start the advertising thread")?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Advert { stop: Some(stop), thread: Some(thread) }),
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("Avahi did not answer"),
        }
    }
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
