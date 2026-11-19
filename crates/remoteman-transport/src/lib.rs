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

fn transport() -> Result<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(1)));
    t.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
    // Room for a few large frames in flight in either direction.
    t.datagram_receive_buffer_size(Some(32 << 20));
    t.datagram_send_buffer_size(32 << 20);
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
    let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
    let mut endpoint = Endpoint::client(bind)?;
    let mut config = quinn::ClientConfig::new(Arc::new(tls::client(identity, expect)?));
    config.transport_config(Arc::new(transport()?));
    endpoint.set_default_client_config(config);
    let conn = endpoint.connect(addr, "remoteman")?.await.context("connect")?;
    let fp = peer_fingerprint(&conn).context("the host sent no certificate")?;
    Ok((endpoint, conn, fp))
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
