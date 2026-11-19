//! remoteman for Linux: a GTK 4 and libadwaita viewer.
//!
//!     remoteman [HOST:PORT] [--pair] [--size WxH] [--scale S] [--refresh HZ]
//!               [--decoders av1,hevc444,hevc] [--state DIR]
//!               [--placement extend|main|only] [--hover-bar yes|no]
//!
//! Without a host it opens on the list of paired computers. With one it connects at once;
//! `--pair` then trusts the computer if it isn't known yet (it must be in pairing mode).
//! `--placement`: this window as an extra display beside the other computer's own, as its main
//! display, or as its only one. `--hover-bar yes`: no title bar while connected; it comes out
//! when the pointer reaches the top edge. Both can be changed in the window, and are remembered.
//!
//! For testing: `--quit-after SECONDS` leaves by itself and prints what it saw, and
//! `--dump FILE.png` saves the 200th picture shown. F11 switches full screen; everything else
//! typed goes to the other computer.
//!
//! The parts: `window` (the window and what moves it between screens), `computers` (the list),
//! `pair`, `status` (connecting, and the ways it goes wrong), `session` (the bar, menus,
//! figures and input over the picture), `prefs`, `store` (what is remembered), `features`
//! (what isn't built yet), `video`, `stream` and `decode` (the picture), `shot` (pictures of
//! the window, for tests).

mod capture;
mod nearby;
mod computers;
mod decode;
mod features;
mod pair;
mod prefs;
mod session;
mod shot;
mod status;
mod store;
mod stream;
mod video;
mod window;

use std::path::PathBuf;

use adw::prelude::*;
use gtk::{gio, glib};
use remoteman_core::{Chroma, Codec, Mode, Placement};

#[derive(Clone)]
pub struct Args {
    host: Option<String>,
    pair: bool,
    size: Option<(u32, u32)>,
    scale: Option<f32>,
    refresh: Option<u32>,
    decoders: Vec<Mode>,
    state: PathBuf,
    quit_after: Option<u32>,
    dump: Option<PathBuf>,
    /// Where the display sits among the other computer's own, if said on the command line.
    placement: Option<Placement>,
    /// Hide the window's bar while connected, if said on the command line.
    hover_bar: Option<bool>,
    /// Steps to run by itself, for pictures of the window (see shot.rs).
    ui: Option<String>,
    /// Start with the computer's sound on.
    sound: bool,
    /// Play the sound on this PipeWire node only, instead of the default output (for tests).
    sound_to: Option<String>,
    /// Start with this device's microphone on.
    mic: bool,
    /// Record this PipeWire node only, instead of the default microphone (for tests).
    mic_from: Option<String>,
    /// Start with this device's camera on.
    camera: bool,
    /// Read this PipeWire camera node only, instead of the default camera (for tests).
    camera_from: Option<String>,
}

/// The placements, as they are written on the command line and in the state folder.
pub const PLACEMENTS: [(Placement, &str); 4] = [(Placement::Extend, "extend"), (Placement::Main, "main"), (Placement::Only, "only"), (Placement::Mirror, "mirror")];

fn parse_args() -> anyhow::Result<Args> {
    use anyhow::Context;
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/state")).join("remoteman");
    let mut a = Args { host: None, pair: false, size: None, scale: None, refresh: None, decoders: Vec::new(), state, quit_after: None, dump: None, placement: None, hover_bar: None, ui: None, sound: false, sound_to: None, mic: false, mic_from: None, camera: false, camera_from: None };
    let mut decoders = "av1,hevc444,hevc".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--pair" => a.pair = true,
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                a.size = Some((w.parse()?, h.parse()?));
            }
            "--scale" => a.scale = Some(value()?.parse()?),
            "--refresh" => a.refresh = Some(value()?.parse()?),
            "--decoders" => decoders = value()?,
            "--state" => a.state = value()?.into(),
            "--quit-after" => a.quit_after = Some(value()?.parse()?),
            "--dump" => a.dump = Some(value()?.into()),
            "--placement" => {
                let v = value()?;
                a.placement = Some(PLACEMENTS.iter().find(|p| p.1 == v).context("--placement extend, main, only or mirror")?.0);
            }
            "--hover-bar" => a.hover_bar = Some(value()? == "yes"),
            // For pictures of the window: see shot.rs.
            "--ui" => a.ui = Some(value()?),
            "--sound" => a.sound = true,
            "--sound-to" => a.sound_to = Some(value()?),
            "--mic" => a.mic = true,
            "--mic-from" => a.mic_from = Some(value()?),
            "--camera" => a.camera = true,
            "--camera-from" => a.camera_from = Some(value()?),
            other if other.starts_with("--") => anyhow::bail!("unknown option {other}"),
            host => a.host = Some(host.to_owned()),
        }
    }
    for d in decoders.split(',') {
        a.decoders.push(match d {
            "av1" => Mode { codec: Codec::Av1, chroma: Chroma::C420, depth: 10 },
            "hevc444" => Mode { codec: Codec::Hevc, chroma: Chroma::C444, depth: 10 },
            "hevc" => Mode { codec: Codec::Hevc, chroma: Chroma::C420, depth: 10 },
            other => anyhow::bail!("unknown decoder {other}"),
        });
    }
    Ok(a)
}

fn main() -> glib::ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("remoteman: {e:#}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder().application_id("dev.remoteman.Viewer").flags(gio::ApplicationFlags::NON_UNIQUE).build();
    // The desktop takes the window's icon from the launcher entry of the same name; this is for
    // the places that ask the window itself (the About dialog, X11).
    app.connect_startup(|_| gtk::Window::set_default_icon_name("dev.remoteman.Viewer"));
    app.connect_activate(move |app| window::build(app, args.clone()));
    app.run_with_args::<&str>(&[])
}
