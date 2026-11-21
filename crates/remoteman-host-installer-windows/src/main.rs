//! `remoteman-host-setup`: remoteman host's installer and uninstaller for Windows. The program
//! is the library's (src/lib.rs says what it does and how), as the host's.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    std::process::exit(remoteman_setup::run(remoteman_setup::Product::Host));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-host-setup installs remoteman host on Windows; it runs only there");
    std::process::exit(1);
}
