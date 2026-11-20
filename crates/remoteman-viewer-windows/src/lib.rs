//! remoteman_engine.dll: the engine of the Windows viewer, behind the WinUI 3 app in
//! `windows/viewer`. Everything below the interface is here and in Rust: sessions with the
//! shared core, FFmpeg's decoders, the picture in a swap chain the app puts into its
//! `SwapChainPanel`, input, sound, clipboard, files, the second display, the figures and the
//! store of computers and settings. The app is the view: it shows the engine's state (one JSON
//! document) and hands it what people do (`engine.rs` has the C interface).
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
mod features;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod menus;
#[cfg(windows)]
mod monitor;
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
mod store;
#[cfg(windows)]
mod stream;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod walk;
#[cfg(windows)]
mod window;
