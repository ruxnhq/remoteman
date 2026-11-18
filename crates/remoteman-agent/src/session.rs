//! What the agent does for a client: a virtual monitor made for its display, the picture out,
//! its input in. The connection itself is the shared core's.

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::{Chroma, Codec, Event, Mode};
use remoteman_desktop::mutter::VirtualDisplay;

use crate::pipeline::{Pipeline, Settings};

/// What this host encodes.
const MODES: [Mode; 3] = [
    Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
    Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
];

/// A client being served.
struct Live {
    desktop: VirtualDisplay,
    pipeline: Pipeline,
    size: (u32, u32),
}

pub struct Agent {
    /// For talking to Mutter over D-Bus.
    runtime: tokio::runtime::Runtime,
    bitrate: u32,
    live: Option<Live>,
}

impl Agent {
    pub fn new(bitrate: u32) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("desktop").enable_all().build()?;
        Ok(Self { runtime, bitrate, live: None })
    }
}

impl HostHandler for Agent {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let screen = client.display;
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        // The client's order of preference decides.
        let mode = client.decoders.iter().copied().find(|m| MODES.contains(m)).ok_or("no video mode in common")?;
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return Err("display sizes must be even, from 64 to 8192".into());
        }
        let refresh = screen.refresh.clamp(1, 240);
        let desktop = self.runtime.block_on(VirtualDisplay::create(client.cursor)).map_err(|e| format!("{e:#}"))?;
        let settings = Settings {
            node: desktop.node,
            width: screen.width,
            height: screen.height,
            fps: refresh,
            scale: screen.scale.clamp(1.0, 4.0),
            mode,
            bitrate: self.bitrate,
            cursor: client.cursor,
        };
        let pipeline = match Pipeline::start(settings, link) {
            Ok(p) => p,
            Err(e) => {
                self.runtime.block_on(desktop.close());
                return Err(format!("{e:#}"));
            }
        };
        self.live = Some(Live { desktop, pipeline, size: (screen.width, screen.height) });
        Ok(Accept { mode, width: screen.width, height: screen.height, refresh, colour: "bt709".into() })
    }

    fn keyframe(&mut self) {
        if let Some(l) = &self.live {
            l.pipeline.request_keyframe();
        }
    }

    fn input(&mut self, event: Event) {
        let Some(l) = &self.live else { return };
        let scale = f32::from_bits(l.pipeline.scale.load(std::sync::atomic::Ordering::Relaxed));
        if let Err(e) = self.runtime.block_on(l.desktop.inject(&event, l.size, scale)) {
            tracing::warn!("input: {e:#}");
        }
    }

    fn left(&mut self) {
        if let Some(l) = self.live.take() {
            tracing::info!(skipped = l.pipeline.skipped.load(std::sync::atomic::Ordering::Relaxed), "session over");
            drop(l.pipeline);
            self.runtime.block_on(l.desktop.close());
        }
    }
}
