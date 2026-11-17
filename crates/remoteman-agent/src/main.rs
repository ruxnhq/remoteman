//! `remoteman-agent`: the Linux host, standalone for now. Run it inside a GNOME session:
//!
//!     remoteman-agent [--listen ADDR:PORT] [--pair] [--state DIR] [--bitrate MBIT]
//!
//! `--pair` lets one unknown device in and remembers it. One client at a time; a new one takes
//! over.

mod pipeline;
mod session;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use remoteman_transport::{Identity, Trust, listen, peer_fingerprint};

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remoteman")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let mut addr: SocketAddr = format!("0.0.0.0:{}", remoteman_proto::PORT).parse()?;
    let (mut pair, mut state, mut bitrate) = (false, state_dir(), 40u32);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--listen" => addr = value()?.parse().context("--listen ADDR:PORT")?,
            "--state" => state = value()?.into(),
            "--bitrate" => bitrate = value()?.parse().context("--bitrate in Mbit/s")?,
            "--pair" => pair = true,
            other => anyhow::bail!("unknown option {other}"),
        }
    }
    let identity = Identity::load_or_create(&state.join("host"))?;
    let trust = Arc::new(Trust::load(&state.join("host/trusted")));
    trust.set_pairing(pair);
    let endpoint = listen(addr, &identity, trust.clone())?;
    tracing::info!("listening on {}; this host's fingerprint is {}", endpoint.local_addr()?, identity.fingerprint);
    if pair {
        tracing::info!("pairing: the next unknown device to connect will be trusted");
    }

    let mut current: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let incoming = tokio::select! {
            i = endpoint.accept() => match i { Some(i) => i, None => break },
            _ = tokio::signal::ctrl_c() => break,
        };
        let conn = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("refused a connection: {e}");
                continue;
            }
        };
        let Some(fp) = peer_fingerprint(&conn) else { continue };
        if !trust.contains(&fp) {
            // Only reachable while pairing: the handshake refuses strangers otherwise.
            trust.add(&fp, &conn.remote_address().ip().to_string())?;
            trust.set_pairing(false);
            tracing::info!("paired with {fp}");
        }
        tracing::info!("client {} connected from {}", &fp[..12], conn.remote_address());
        // The newest client takes the desktop over.
        if let Some(old) = current.take() {
            old.abort();
            let _ = old.await;
        }
        current = Some(tokio::spawn(async move {
            if let Err(e) = session::serve(conn.clone(), bitrate * 1_000_000).await {
                tracing::error!("session: {e:#}");
                conn.close(1u32.into(), b"failed");
            }
        }));
    }
    if let Some(c) = current {
        c.abort();
        let _ = c.await;
    }
    endpoint.close(0u32.into(), b"bye");
    Ok(())
}
