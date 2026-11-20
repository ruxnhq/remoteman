//! remoteman for Windows: the app.
//!
//!     remoteman.exe [HOST:PORT] [the engine's words] [--window WxH] [--pos X,Y] [--dark|--light]
//!
//! Ordinary people start it with nothing; the rest is for tests. The engine takes the old
//! program's words (HOST:PORT, --pair, --state, --log, --seconds, --size, --play, ...); these are
//! the view's own: `--window` the window's size in the app's units, `--pos` where it goes in
//! pixels, `--dark` / `--light` the theme (the engine is told too).
//!
//! WinUI 3 through windows-reactor, the view only: what is on screen is the engine's state
//! (`app.rs`), and what people do goes back to the engine as its commands and typed calls.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod dialogs;
#[cfg(windows)]
mod kit;
#[cfg(windows)]
mod pages;
#[cfg(windows)]
mod picture;
#[cfg(windows)]
mod session;
#[cfg(windows)]
mod system;

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-app is remoteman for Windows; it runs only there (the Linux viewer is remoteman-viewer)");
    std::process::exit(1);
}

/// What the view takes from the command line; the rest is the engine's.
#[cfg(windows)]
#[derive(Clone, PartialEq)]
pub struct Options {
    pub window: Option<(f64, f64)>,
    pub pos: Option<(i32, i32)>,
    pub theme: windows_reactor::WindowTheme,
    pub engine: Vec<String>,
    /// This is the second window of full screen: only the session's bar (it has no engine of its own).
    pub bar: bool,
}

#[cfg(windows)]
fn options() -> Options {
    let mut o = Options { window: None, pos: None, theme: windows_reactor::WindowTheme::System, engine: Vec::new(), bar: false };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--window" => o.window = args.next().and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))),
            "--pos" => o.pos = args.next().and_then(|v| v.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?)))),
            "--dark" => {
                o.theme = windows_reactor::WindowTheme::Dark;
                o.engine.push(a);
            }
            "--light" => {
                o.theme = windows_reactor::WindowTheme::Light;
                o.engine.push(a);
            }
            _ => o.engine.push(a),
        }
    }
    // Packaged (the Store's MSIX): the state lives in the package's own data folder, which
    // Windows removes with the app, rather than in %APPDATA%.
    if let Some(folder) = system::package_folder().filter(|_| !o.engine.iter().any(|w| w == "--state")) {
        o.engine.push("--state".into());
        o.engine.push(folder.join("remoteman").display().to_string());
    }
    o
}

#[cfg(windows)]
fn main() {
    let options = options();
    let packaged = system::package_folder().is_some();
    if let Err(e) = windows_reactor::App::run_component::<app::Viewer>(options) {
        let line = format!("the app ended with an error: {e}");
        remoteman_engine::say(&line);
        // Without --log (as the Start menu starts it), still leave the reason somewhere.
        system::crash_note(&line, packaged);
        std::process::exit(2);
    }
    // Not a return: nothing still running (a stuck decoder, the core's threads) may keep the
    // process alive.
    std::process::exit(app::exit_code());
}
