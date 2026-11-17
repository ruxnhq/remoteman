//! One client's session: hello, a virtual monitor made for its display, video out, input in.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use remoteman_desktop::mutter::VirtualDisplay;
use remoteman_proto::control::{Chroma, Codec, Message, Mode};
use remoteman_proto::input::Event;
use remoteman_proto::video::{CursorAt, fragment};
use remoteman_transport::{Connection, RecvStream, read_message, write_message};

use crate::pipeline::{Encoded, Pipeline, Settings};

/// What this host encodes.
const MODES: [Mode; 3] = [
    Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
];

pub async fn serve(conn: Connection, bitrate: u32) -> Result<()> {
    let clock = Instant::now();
    let (mut send, mut recv) = conn.accept_bi().await.context("control stream")?;
    let Some(Message::ClientHello { proto, name, display: screen, decoders, cursor: cursor_apart }) = read_message(&mut recv).await? else { bail!("the first message wasn't client_hello") };
    tracing::info!(%name, ?screen, ?decoders, "client");
    let host = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "linux".into());
    write_message(&mut send, &Message::HostHello { proto: remoteman_proto::PROTO, name: host, os: "linux".into(), features: vec!["video".into(), "input".into(), "cursor".into()] }).await?;
    let fail = |code: &str, message: String| Message::Error { code: code.into(), message };
    if proto != remoteman_proto::PROTO {
        write_message(&mut send, &fail("unsupported", format!("this host speaks protocol {}", remoteman_proto::PROTO))).await?;
        bail!("client speaks protocol {proto}");
    }
    // The client's order of preference decides.
    let Some(mode) = decoders.iter().copied().find(|m| MODES.contains(m)) else {
        write_message(&mut send, &fail("unsupported", "no video mode in common".into())).await?;
        bail!("no video mode in common");
    };
    if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
        write_message(&mut send, &fail("unsupported", "display sizes must be even, from 64 to 8192".into())).await?;
        bail!("unusable display size {}x{}", screen.width, screen.height);
    }
    let refresh = screen.refresh.clamp(1, 240);

    let desktop = match VirtualDisplay::create(cursor_apart).await {
        Ok(d) => d,
        Err(e) => {
            write_message(&mut send, &fail("failed", format!("{e:#}"))).await?;
            return Err(e.context("virtual monitor"));
        }
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Encoded>(4);
    let (cursor_tx, mut cursor_rx) = tokio::sync::mpsc::unbounded_channel();
    let settings = Settings { cursor: cursor_apart.then_some(cursor_tx), node: desktop.node, width: screen.width, height: screen.height, fps: refresh, scale: screen.scale.clamp(1.0, 4.0), mode, bitrate };
    let pipeline = match tokio::task::spawn_blocking(move || Pipeline::start(settings, clock, tx)).await? {
        Ok(p) => p,
        Err(e) => {
            write_message(&mut send, &fail("failed", format!("{e:#}"))).await?;
            desktop.close().await;
            return Err(e.context("pipeline"));
        }
    };
    const EPOCH: u16 = 1;
    write_message(&mut send, &Message::Stream { epoch: EPOCH, mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into() }).await?;

    let video = async {
        let mut number = 0u32;
        while let Some(f) = rx.recv().await {
            let max = conn.max_datagram_size().context("the client doesn't take datagrams")?;
            for d in fragment(&f.data, f.key, EPOCH, number, f.capture_us, max) {
                conn.send_datagram_wait(d.into()).await?;
            }
            number = number.wrapping_add(1);
        }
        anyhow::Ok(())
    };
    // Two things write to the control stream: answers to the client, and cursor shapes.
    let send = tokio::sync::Mutex::new(send);
    let control = async {
        while let Some(m) = read_message(&mut recv).await? {
            match m {
                Message::Keyframe => pipeline.request_keyframe(),
                Message::Ping { id, client_us } => write_message(&mut *send.lock().await, &Message::Pong { id, client_us, host_us: clock.elapsed().as_micros() as u64 }).await?,
                _ => {}
            }
        }
        anyhow::Ok(())
    };
    let cursor = async {
        // The shape goes on the control stream (it must arrive); the position goes as a
        // datagram (only the newest matters).
        let mut last = None;
        while let Some(c) = cursor_rx.recv().await {
            if let Some(s) = c.shape {
                let m = Message::Cursor { width: s.width, height: s.height, hot_x: s.hot_x, hot_y: s.hot_y, rgba: remoteman_proto::control::encode_bytes(&s.rgba) };
                write_message(&mut *send.lock().await, &m).await?;
            }
            let at = CursorAt { epoch: EPOCH, visible: c.on_screen, x: c.x as f32 / screen.width as f32, y: c.y as f32 / screen.height as f32 };
            if last != Some(at) {
                last = Some(at);
                tracing::debug!(?at, "cursor");
                let _ = conn.send_datagram(at.write().into());
            }
        }
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };
    let input = async {
        let stream = conn.accept_uni().await?;
        read_input(stream, &desktop, (screen.width, screen.height), &pipeline.scale).await
    };
    let result = tokio::select! {
        r = video => r.context("video"),
        r = control => r.context("control"),
        r = cursor => r.context("cursor"),
        r = input => r.context("input"),
        e = conn.closed() => { tracing::info!("client left: {e}"); Ok(()) }
    };
    // The client leaving ends whichever part was waiting on it; that isn't a failure.
    let result = if conn.close_reason().is_some() { Ok(()) } else { result };
    tracing::info!(skipped = pipeline.skipped.load(std::sync::atomic::Ordering::Relaxed), "session over");
    tokio::task::spawn_blocking(move || drop(pipeline)).await?;
    desktop.close().await;
    result
}

async fn read_input(mut stream: RecvStream, desktop: &VirtualDisplay, size: (u32, u32), scale: &std::sync::atomic::AtomicU32) -> Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Some(n) = stream.read(&mut chunk).await? {
        buf.extend_from_slice(&chunk[..n]);
        let mut at = 0;
        while let Some((event, used)) = Event::parse(&buf[at..])? {
            at += used;
            let scale = f32::from_bits(scale.load(std::sync::atomic::Ordering::Relaxed));
            if let Err(e) = desktop.inject(&event, size, scale).await {
                tracing::warn!("input: {e:#}");
            }
        }
        buf.drain(..at);
    }
    // The input stream closing isn't the session ending: wait for the connection to say so.
    std::future::pending::<()>().await;
    Ok(())
}
