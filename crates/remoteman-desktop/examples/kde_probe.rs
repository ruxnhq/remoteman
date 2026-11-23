//! Make a KDE virtual monitor through the portal, place it, and report what KScreen then lists.
//! For a nested test session only (`scripts/kde-nested.sh`): it changes the monitor layout.
//!     cargo run -p remoteman-desktop --example kde_probe [-- SECONDS]

use std::time::Duration;

use remoteman_desktop::VirtualDisplay;
use remoteman_proto::control::Placement;
use remoteman_proto::input::Event;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(3);
    let display = VirtualDisplay::create(false, false).await?;
    let VirtualDisplay::Kde(_) = &display else { anyhow::bail!("not a KDE session") };
    println!("node {}", display.node());
    display.prepare(2752, 2064, 2.0, 120).await?;
    match display.place(Placement::Extend).await {
        Ok(()) => println!("placed"),
        Err(e) => println!("place failed: {e:#}"),
    }
    let out = std::process::Command::new("kscreen-doctor").arg("-o").output()?;
    println!("{}", String::from_utf8_lossy(&out.stdout));
    display.inject(&Event::PointerTo { x: 0.5, y: 0.5 }, (2752, 2064), 2.0).await?;
    println!("pointer moved");
    tokio::time::sleep(Duration::from_secs(secs)).await;
    display.close().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let out = std::process::Command::new("kscreen-doctor").arg("-o").output()?;
    println!("after close:\n{}", String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("Output:")).collect::<Vec<_>>().join("\n"));
    Ok(())
}
