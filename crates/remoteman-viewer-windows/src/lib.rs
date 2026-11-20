//! The engine of the Windows viewer, behind the app (`crates/remoteman-viewer-app-windows`,
//! WinUI 3 through windows-reactor), which links it as a library. Everything below the
//! interface is here: sessions with the
//! shared core, FFmpeg's decoders, the picture in a swap chain the app puts into its
//! `SwapChainPanel`, input (touches too), sound, this PC's microphone and camera lent to the
//! computer, clipboard, files, the second display, the figures, finding and waking computers on
//! the network, and the store of computers and settings. The app is the view: it shows the engine's state (one JSON
//! document) and hands it what people do (`engine.rs` has the interface).
//!
//! For now this builds the old program's modules too, the hand-drawn kit among them: the
//! engine drives the same `App`, which the old program draws with the kit. The kit goes when
//! the WinUI 3 app does everything the old program did.

#![cfg_attr(not(windows), allow(unused))]
#![allow(dead_code)]

#[cfg(windows)]
#[macro_use]
mod log;
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod camera;
#[cfg(windows)]
mod args;
#[cfg(windows)]
mod creds;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod decode;
#[cfg(windows)]
mod demo;
#[cfg(windows)]
mod draw;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod full;
#[cfg(windows)]
mod features;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod media;
#[cfg(windows)]
mod menus;
#[cfg(windows)]
mod monitor;
#[cfg(windows)]
mod mic;
#[cfg(windows)]
mod nearby;
#[cfg(windows)]
mod pages;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod shot;
#[cfg(windows)]
mod sound;
#[cfg(windows)]
mod status;
#[cfg(windows)]
mod touch;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod stream;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod walk;
#[cfg(windows)]
mod window;

// The interface the app calls (ngine.rs).
#[cfg(windows)]
pub use engine::{Notify, PanelChain, attach, button, command, focus, key, panel, pointer, say, start, state, stop, system_dark, touch, wheel};
