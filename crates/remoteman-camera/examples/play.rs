//! Plays an Annex-B H.264 file as a camera, over and over, at 30 frames a second: for trying the
//! camera in real programs (a browser, GNOME's Camera) and for trying the v4l2loopback output,
//! which the tests cannot reach.
//!
//! ```sh
//! ffmpeg -f lavfi -i testsrc2=size=1280x720:rate=30 -t 4 -c:v libx264 -bf 0 -g 60 -pix_fmt yuv420p -f h264 clip.h264
//! cargo run -p remoteman-camera --example play -- clip.h264 "remoteman-test-camera" 60
//! ```
//!
//! With a loopback device labelled for remoteman (`sudo modprobe v4l2loopback
//! card_label="remoteman camera" exclusive_caps=1`) the same pictures go to it; this prints
//! which device it found.

#[path = "../tests/common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use remoteman_camera::{Camera, Pushed};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(file) = args.next() else {
        eprintln!("usage: play <file.h264> [camera name] [seconds]");
        std::process::exit(2);
    };
    let name = args.next().unwrap_or_else(|| "remoteman-test-camera".to_owned());
    let seconds = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let frames = common::split_access_units(&std::fs::read(&file)?);
    anyhow::ensure!(frames.first().is_some_and(|f| f.key), "{file}: no H.264 frames that start with a keyframe");

    let mut camera = Camera::start(&name)?;
    println!("camera \"{name}\": PipeWire node {} ({}), {}x{}", camera.node_id(), camera.node_name(), camera.size().0, camera.size().1);
    match camera.loopback_device() {
        Some(device) => println!("also writing to {}", device.display()),
        None => println!("no loopback device labelled for remoteman: PipeWire only"),
    }

    let (start, frame) = (Instant::now(), Duration::from_nanos(1_000_000_000 / 30));
    let (mut pushed, mut lost) = (0u32, 0u32);
    while start.elapsed() < Duration::from_secs(seconds) {
        for f in &frames {
            if let Some(wait) = (frame * pushed).checked_sub(start.elapsed()) {
                std::thread::sleep(wait);
            }
            if camera.push(&f.data, f.key, pushed as u64 * 33_333) != Pushed::Shown {
                lost += 1;
            }
            pushed += 1;
        }
    }
    println!("pushed {pushed} frames, {lost} not shown");
    Ok(())
}
