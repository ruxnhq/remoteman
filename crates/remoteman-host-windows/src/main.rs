#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-host is the Windows host; on Linux the host is remoteman-agent");
    std::process::exit(1);
}
#[cfg(windows)]
fn main() {}
