//! QUIC between a remoteman client and host (docs/PROTOCOL.md): one connection carrying the
//! control stream, the input stream and video datagrams, with each side recognised by the
//! fingerprint of its certificate.

mod identity;
mod tls;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
pub use identity::{Identity, Trust, fingerprint, pair_code};
pub use quinn::{Connection, Endpoint, RecvStream, SendStream};
use remoteman_proto::control::{Message, body_len};

/// Room for datagrams waiting to be sent. The host keeps far less than this queued (see
/// `remoteman_core::host::wait_to_send`), so sound and the cursor never push video out of it.
pub const DATAGRAM_SEND_BUFFER: usize = 32 << 20;

fn transport() -> Result<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
    // Room for a few large frames in flight in either direction.
    t.datagram_receive_buffer_size(Some(32 << 20));
    t.datagram_send_buffer_size(DATAGRAM_SEND_BUFFER);
    t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    Ok(t)
}

/// A host endpoint listening on `addr`. Only clients in `trust` get past the handshake (or any
/// client, while `trust` is in pairing mode; the caller then adds it).
pub fn listen(addr: SocketAddr, identity: &Identity, trust: Arc<Trust>) -> Result<Endpoint> {
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(tls::server(identity, trust)?));
    config.transport_config(Arc::new(transport()?));
    Endpoint::server(config, addr).with_context(|| format!("listen on {addr}"))
}

/// Who the host is expected to be.
pub enum Expect {
    /// The host with this fingerprint, and no other.
    Fingerprint(String),
    /// Whoever answers (pairing). The caller stores the fingerprint returned.
    Anyone,
}

/// Connect to a host. Returns the endpoint (keep it alive), the connection and the host's
/// fingerprint.
pub async fn connect(addr: SocketAddr, identity: &Identity, expect: Expect) -> Result<(Endpoint, Connection, String)> {
    connect_any(&[addr], identity, expect).await
}

/// How long one address is given before the next is tried beside it.
const NEXT_ADDRESS: Duration = Duration::from_millis(250);

/// The order a name's addresses are tried in: IPv4 on a local network first (the hosts listen on
/// IPv4, and a `.local` name lists a computer's VPN and virtual-switch addresses too, often
/// before its LAN one), then other IPv4 (a VPN's), then IPv6, link-local last.
pub fn connect_order(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    use std::net::IpAddr;
    let rank = |a: &SocketAddr| match a.ip() {
        IpAddr::V4(ip) if ip.is_loopback() || ip.is_private() => 0,
        IpAddr::V4(ip) if ip.is_link_local() => 2,
        IpAddr::V4(_) => 1,
        IpAddr::V6(ip) if ip.is_loopback() => 0,
        IpAddr::V6(ip) if ip.segments()[0] & 0xffc0 == 0xfe80 => 4,
        IpAddr::V6(_) => 3,
    };
    let mut out: Vec<SocketAddr> = Vec::new();
    for a in addrs {
        if !out.contains(&a) {
            out.push(a);
        }
    }
    out.sort_by_key(rank);
    out
}

/// Connect to a host that may be at any of these addresses (all of one name's, in
/// [`connect_order`]): each is tried [`NEXT_ADDRESS`] after the one before it (at once if that
/// one has already failed), and the first that completes the handshake is kept. If none does,
/// the error is a refusal or a wrong key if any address gave one, or else the first address's.
pub async fn connect_any(addrs: &[SocketAddr], identity: &Identity, expect: Expect) -> Result<(Endpoint, Connection, String)> {
    anyhow::ensure!(!addrs.is_empty(), "no address to connect to");
    let mut config = quinn::ClientConfig::new(Arc::new(tls::client(identity, expect)?));
    config.transport_config(Arc::new(transport()?));
    let attempt = |addr: SocketAddr, config: quinn::ClientConfig| async move {
        let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
        let mut endpoint = Endpoint::client(bind)?;
        endpoint.set_default_client_config(config);
        let conn = endpoint.connect(addr, "remoteman")?.await.context("connect")?;
        let fp = peer_fingerprint(&conn).context("the host sent no certificate")?;
        anyhow::Ok((endpoint, conn, fp))
    };
    if let [addr] = addrs {
        return attempt(*addr, config).await;
    }
    let mut tries = tokio::task::JoinSet::new();
    let mut waiting = addrs.iter().copied().enumerate();
    let mut next = tokio::time::Instant::now();
    let mut errors: Vec<(usize, SocketAddr, anyhow::Error)> = Vec::new();
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next), if waiting.len() > 0 => {
                let (i, addr) = waiting.next().unwrap();
                let config = config.clone();
                tries.spawn(async move { (i, addr, attempt(addr, config).await) });
                next = tokio::time::Instant::now() + NEXT_ADDRESS;
            }
            Some(done) = tries.join_next() => match done {
                Ok((_, addr, Ok(won))) => {
                    tries.abort_all();
                    if addrs.len() > 1 {
                        tracing::info!(%addr, of = addrs.len(), "connected (of the name's addresses)");
                    }
                    return Ok(won);
                }
                Ok((i, addr, Err(e))) => {
                    errors.push((i, addr, e));
                    next = tokio::time::Instant::now();
                }
                Err(_) => next = tokio::time::Instant::now(),
            },
            else => break,
        }
    }
    let timed_out = |e: &anyhow::Error| matches!(e.downcast_ref::<quinn::ConnectionError>(), Some(quinn::ConnectionError::TimedOut));
    errors.sort_by_key(|(i, ..)| *i);
    let pick = errors.iter().position(|(.., e)| !timed_out(e)).unwrap_or(0);
    // (Not wrapped in more words: the apps tell the problems apart by what the error says.)
    let (_, addr, e) = errors.swap_remove(pick);
    tracing::info!(%addr, of = addrs.len(), "none of the name's addresses answered; the error is this one's");
    Err(e)
}

/// The fingerprint of the other side's certificate.
/// The code the other side closed this connection with, if it did.
pub fn closed_with(conn: &Connection) -> Option<u32> {
    match conn.close_reason()? {
        quinn::ConnectionError::ApplicationClosed(close) => u32::try_from(close.error_code.into_inner()).ok(),
        _ => None,
    }
}

pub fn peer_fingerprint(conn: &Connection) -> Option<String> {
    let certs = conn.peer_identity()?.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| fingerprint(c))
}

/// Write one control message.
pub async fn write_message(send: &mut SendStream, message: &Message) -> Result<()> {
    send.write_all(&message.frame()).await?;
    Ok(())
}

/// Read one control message; `None` when the other side closed the stream.
pub async fn read_message(recv: &mut RecvStream) -> Result<Option<Message>> {
    let mut prefix = [0u8; 4];
    match recv.read_exact(&mut prefix).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let mut body = vec![0u8; body_len(prefix)?];
    recv.read_exact(&mut body).await?;
    Ok(Some(Message::parse(&body)?))
}
