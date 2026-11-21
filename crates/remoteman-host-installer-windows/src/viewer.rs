//! `remoteman-viewer-setup`: remoteman's (the viewer's) installer and uninstaller for Windows,
//! for the user who runs it, outside the Store. The program is the library's (src/lib.rs says
//! what it does and how), as the viewer's.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    std::process::exit(remoteman_setup::run(remoteman_setup::Product::Viewer));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-viewer-setup installs remoteman on Windows; it runs only there");
    std::process::exit(1);
}
