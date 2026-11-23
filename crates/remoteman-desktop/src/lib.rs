//! The desktop side of a host: a virtual display for the client, a way to put its input in,
//! and the clipboard shared with it.
//! Two backends: GNOME, through Mutter's ScreenCast and RemoteDesktop D-Bus interfaces (marked
//! private by Mutter, so every call to them lives in this crate), and KDE Plasma, through the
//! desktop portals and KScreen. [`VirtualDisplay`] picks the one this session has.

pub mod advertise;
pub mod clipboard;
mod eis;
pub mod kde;
pub mod logind;
pub mod mutter;

use std::future::Future;
use std::pin::Pin;

use anyhow::Result;
pub use mutter::Mirrored;
use remoteman_proto::control::{Placement, Side};
use remoteman_proto::input::Event;

/// A virtual monitor for the client and the session that takes its input, on whichever
/// desktop this is.
pub enum VirtualDisplay {
    Gnome(mutter::VirtualDisplay),
    Kde(kde::VirtualDisplay),
}

type Placing = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

impl VirtualDisplay {
    /// Make a virtual monitor (see [`mutter::VirtualDisplay::create`]).
    pub async fn create(cursor_apart: bool, mirror: bool) -> Result<Self> {
        Ok(if kde::present().await { Self::Kde(kde::VirtualDisplay::create(cursor_apart, mirror).await?) } else { Self::Gnome(mutter::VirtualDisplay::create(cursor_apart, mirror).await?) })
    }

    /// A second virtual monitor for the same client, beside its first.
    pub async fn create_beside(cursor_apart: bool) -> Result<Self> {
        Ok(if kde::present().await { Self::Kde(kde::VirtualDisplay::create_beside(cursor_apart).await?) } else { Self::Gnome(mutter::VirtualDisplay::create_beside(cursor_apart).await?) })
    }

    /// The PipeWire node carrying the monitor's picture.
    pub fn node(&self) -> u32 {
        match self {
            Self::Gnome(d) => d.node,
            Self::Kde(d) => d.node,
        }
    }

    /// Set when the client is shown one of the computer's own monitors.
    pub fn mirrored(&self) -> Option<Mirrored> {
        match self {
            Self::Gnome(d) => d.mirrored,
            Self::Kde(d) => d.mirrored,
        }
    }

    /// Get the monitor ready for a stream of this size, scale and refresh rate, before the
    /// stream is connected. Mutter takes all three from the stream; KDE needs them set.
    pub async fn prepare(&self, width: u32, height: u32, scale: f32, refresh: u32) -> Result<()> {
        match self {
            Self::Gnome(_) => Ok(()),
            Self::Kde(d) => d.prepare(width, height, scale, refresh).await,
        }
    }

    /// Put this display where the client wants it among the computer's own.
    pub fn place(&self, placement: Placement) -> Placing {
        match self {
            Self::Gnome(d) => Box::pin(d.place(placement)),
            Self::Kde(d) => Box::pin(d.place(placement)),
        }
    }

    /// Put a display that sits beside the others on the side the client names.
    pub fn place_on(&self, side: Side) -> Placing {
        match self {
            Self::Gnome(d) => Box::pin(d.place_on(side)),
            Self::Kde(d) => Box::pin(d.place_on(side)),
        }
    }

    /// Put one input event into the desktop.
    pub async fn inject(&self, event: &Event, size: (u32, u32), scale: f32) -> Result<()> {
        match self {
            Self::Gnome(d) => d.inject(event, size, scale).await,
            Self::Kde(d) => d.inject(event, size, scale).await,
        }
    }

    /// End the session; the virtual monitor goes with it.
    pub async fn close(self) {
        match self {
            Self::Gnome(d) => d.close().await,
            Self::Kde(d) => d.close().await,
        }
    }
}

/// What a client that asks to mirror would be shown now: the computer's main monitor.
pub async fn mirror_target() -> Result<Mirrored> {
    if kde::present().await {
        anyhow::bail!("mirroring isn't available on KDE yet");
    }
    mutter::mirror_target().await
}
