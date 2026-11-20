//! Remoteman Host, the owner's app on Windows:
//!
//!     remoteman-host-app [--background] [--page sharing|devices|about] [--port PORT]
//!                        [--log FILE] [--theme light|dark]
//!
//! `--background` starts with the icon in the notification area only (as at sign-in); otherwise
//! the window opens too. One app a user (and port): a second start opens the first one's window
//! and ends. `--port` is the host's port (7791 unless the host was told otherwise), which names
//! its control pipe. `--theme` overrides Windows' light or dark.
//!
//! WinUI 3 through Microsoft's windows-reactor. The app holds no rule of the product: it shows
//! what the running host (remoteman-host.exe) says over its control pipe and tells it what the
//! owner chose. See app.rs for the application, window.rs for the window.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod bridge;
#[cfg(windows)]
mod host;
#[cfg(windows)]
mod log;
#[cfg(windows)]
mod system;
#[cfg(windows)]
mod window;

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-host-app is Remoteman Host for Windows; it runs only there");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    use app::{HostApp, Page, Start};
    use windows_reactor::{App, WindowTheme};

    let mut port = host::DEFAULT_PORT;
    let mut page = None;
    let mut background = false;
    let mut theme = WindowTheme::System;
    let mut log_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--background" => background = true,
            "--page" => page = args.next().as_deref().and_then(Page::from_tag),
            "--port" => port = args.next().and_then(|p| p.parse().ok()).unwrap_or(host::DEFAULT_PORT),
            "--log" => log_file = args.next().map(std::path::PathBuf::from),
            "--theme" => {
                theme = match args.next().as_deref() {
                    Some("dark") => WindowTheme::Dark,
                    Some("light") => WindowTheme::Light,
                    _ => WindowTheme::System,
                }
            }
            _ => {}
        }
    }
    log::to(log_file);
    // With no console, a panic would otherwise leave nothing behind.
    std::panic::set_hook(Box::new(|info| log::say(&format!("panic: {info}"))));

    if !bridge::first(port, page, background) {
        return;
    }
    // The host's first answer before anything is shown, so the window and the icon don't start
    // out saying the host isn't running.
    let status = host::read(port);
    let start = Start { port, page: page.unwrap_or(Page::Sharing), background, theme, status };
    if let Err(e) = App::run_application::<HostApp>(start) {
        log::say(&format!("the app ended with an error: {e}"));
        std::process::exit(1);
    }
    log::say("ended");
}
