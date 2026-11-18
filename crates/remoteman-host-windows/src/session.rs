//! What the host does for a client: the monitor brought to its size, the picture out, its input
//! in, the cursor apart if it asks. The connection itself is the shared core's. Built like the
//! Linux agent's session, with less: no sound, microphone, camera, clipboard or files yet.

use std::sync::{Arc, Mutex};

use remoteman_core::host::{Accept, ClientInfo, HostHandler, Link};
use remoteman_core::{Display, Event, Message, Mode};

use crate::cursor::Cursor;
use crate::display::{self, Area, Fit, Screen};
use crate::encode;
use crate::input::Injector;
use crate::pipeline::{self, Pipeline};

/// The lowest and highest bitrate a client may ask for, in bits a second.
const BITRATE: std::ops::RangeInclusive<u32> = 1_000_000..=150_000_000;

/// A client being served.
struct Live {
    pipeline: Pipeline,
    /// Reporting the cursor, if the client draws it itself.
    cursor: Option<Cursor>,
    injector: Injector,
    /// What the client asked for, to start the picture afresh from when it changes its mind.
    link: Link,
    screen: Display,
    decoders: Vec<Mode>,
    cursor_apart: bool,
    /// The modes the picture was chosen from: the client's and this host's in common. (Which
    /// of them it became is the encoder's doing, and not always the first.)
    common: Vec<Mode>,
    /// The most the picture may use, as the client last said (0: this host's own figure).
    ceiling: u32,
}

pub struct Host {
    screen: Screen,
    bitrate: u32,
    fit: Fit,
    live: Option<Live>,
}

impl Host {
    pub fn new(screen: Screen, bitrate: u32, fit: Fit) -> Self {
        Self { screen, bitrate, fit, live: None }
    }

    /// What the picture is encoded at when the client's ceiling is `ceiling`.
    fn rate(&self, ceiling: u32) -> u32 {
        if ceiling == 0 { self.bitrate } else { ceiling.clamp(*BITRATE.start(), *BITRATE.end()) }
    }

    /// The modes this host encodes among those the client decodes, the client's favourite first.
    fn common(decoders: &[Mode]) -> Vec<Mode> {
        decoders.iter().copied().filter(|m| encode::MODES.contains(m)).collect()
    }

    /// Bring the monitor to the client's display and start its picture. `held`: nothing is
    /// sent until the pipeline is released (see [`Self::restart`]).
    fn start(&mut self, screen: Display, decoders: Vec<Mode>, cursor_apart: bool, ceiling: u32, link: Link, held: bool) -> Result<Accept, String> {
        let modes = Self::common(&decoders);
        if modes.is_empty() {
            return Err("no video mode in common".into());
        }
        if screen.width % 2 != 0 || screen.height % 2 != 0 || screen.width < 64 || screen.height < 64 || screen.width > 8192 || screen.height > 8192 {
            return Err("display sizes must be even, from 64 to 8192".into());
        }
        // The client's scale (how large the desktop is drawn) isn't acted on: Windows has no
        // documented way to set a monitor's scale for the user.
        let want = display::Mode { width: screen.width, height: screen.height, refresh: screen.refresh.clamp(1, 240) };
        let got = self.screen.fit(want, self.fit).map_err(|e| format!("{e:#}"))?;
        let (_, at) = self.screen.now().map_err(|e| format!("{e:#}"))?;
        let area = Arc::new(Mutex::new(at));
        let refresh = got.refresh.clamp(1, 240);
        let (device, shared) = (self.screen.device().to_owned(), area.clone());
        let settings = pipeline::Settings {
            driver: self.screen.driver(),
            modes: modes.clone(),
            fps: refresh,
            bitrate: self.rate(ceiling),
            nudge_at: (at.left, at.top),
            expect: Some((at.width, at.height)),
            held,
            // The monitor changed under the stream: input and the cursor follow it.
            resized: Box::new(move |width, height| {
                let now = display::current(&device).map(|(_, a)| a).unwrap_or(Area { width, height, ..*shared.lock().unwrap() });
                *shared.lock().unwrap() = now;
            }),
        };
        let (pipeline, started) = match Pipeline::start(settings, link.clone()) {
            Ok(p) => p,
            Err(e) => {
                self.screen.release();
                return Err(format!("{e:#}"));
            }
        };
        // The picture is what the driver's channel holds, whatever Windows said of the mode.
        {
            let mut a = area.lock().unwrap();
            (a.width, a.height) = (started.width, started.height);
        }
        let cursor = if cursor_apart { Cursor::start(link.clone(), area.clone()).inspect_err(|e| tracing::warn!("couldn't start reporting the cursor: {e}")).ok() } else { None };
        if !cursor_apart {
            tracing::warn!("this client doesn't draw the cursor itself, and the driver keeps it out of the frames: it will see no cursor");
        }
        self.live = Some(Live { pipeline, cursor, injector: Injector::new(area), link, screen, decoders, cursor_apart, common: modes, ceiling });
        Ok(Accept { mode: started.mode, width: started.width, height: started.height, refresh, colour: "bt709".into() })
    }

    /// Stop the picture, let go of the client's keys, and put the monitor back.
    fn stop(&mut self) -> Option<Live> {
        let mut l = self.live.take()?;
        l.injector.release_all();
        l.cursor = None;
        Some(l)
    }

    /// The client changed what it wants: a new ceiling for the bitrate takes effect in the
    /// running picture; another display or other decoders start the picture afresh.
    fn configure(&mut self, display: Option<Display>, bitrate: Option<u32>, decoders: Option<Vec<Mode>>) {
        let own = self.bitrate;
        let Some(l) = self.live.as_mut() else { return };
        if let Some(b) = bitrate {
            l.ceiling = b;
            l.pipeline.set_bitrate(if b == 0 { own } else { b.clamp(*BITRATE.start(), *BITRATE.end()) });
        }
        let screen = display.unwrap_or(l.screen);
        let decoders = decoders.unwrap_or_else(|| l.decoders.clone());
        // Another picture only if the display differs, or the modes to choose from do.
        if screen == l.screen && Self::common(&decoders) == l.common {
            l.decoders = decoders;
            return;
        }
        tracing::info!(?screen, ?decoders, "the client asked for another picture");
        // The old picture goes first; then the monitor changes, the client is told, and the
        // new picture starts.
        let Some(old) = self.stop() else { return };
        let (link, cursor_apart, ceiling) = (old.link.clone(), old.cursor_apart, old.ceiling);
        let (was_screen, was_decoders) = (old.screen, old.decoders.clone());
        drop(old);
        if let Err(e) = self.restart(screen, decoders, cursor_apart, ceiling, link.clone()) {
            // Back to what worked, rather than leave the client with nothing.
            tracing::warn!("couldn't start that picture ({e}); going back to the one before");
            self.screen.release();
            if let Err(e) = self.restart(was_screen, was_decoders, cursor_apart, ceiling, link) {
                tracing::error!("couldn't restore the picture: {e}");
            }
        }
    }

    /// Start a picture for a client that already has one. The core numbers frames from 0 again
    /// when the client is told of the new picture, and what the picture is (its real size) is
    /// only known once it has started; so it starts held back, the client is told, and then
    /// it is let go.
    fn restart(&mut self, screen: Display, decoders: Vec<Mode>, cursor_apart: bool, ceiling: u32, link: Link) -> Result<(), String> {
        let accept = self.start(screen, decoders, cursor_apart, ceiling, link.clone(), true)?;
        link.restart(accept);
        if let Some(l) = &self.live {
            l.pipeline.release();
        }
        Ok(())
    }
}

impl HostHandler for Host {
    fn connected(&mut self, client: &ClientInfo, link: Link) -> Result<Accept, String> {
        self.left();
        let screen = client.display;
        tracing::info!(name = %client.name, ?screen, decoders = ?client.decoders, cursor_apart = client.cursor, "client");
        self.start(screen, client.decoders.clone(), client.cursor, 0, link, false).inspect_err(|e| tracing::error!("couldn't start the picture: {e}"))
    }

    fn message(&mut self, message: &Message) {
        if let Message::Configure { display, bitrate, decoders } = message {
            self.configure(*display, *bitrate, decoders.clone());
        }
    }

    fn keyframe(&mut self) {
        if let Some(l) = &self.live {
            l.pipeline.request_keyframe();
        }
    }

    fn lost(&mut self, first: u32, last: u32) {
        if let Some(l) = &self.live {
            l.pipeline.report_lost(first, last);
        }
    }

    fn input(&mut self, event: Event) {
        if let Some(l) = self.live.as_mut() {
            l.injector.inject(&event);
        }
    }

    fn left(&mut self) {
        if let Some(l) = self.stop() {
            drop(l);
            self.screen.release();
            tracing::info!("the client is gone");
        }
    }
}
