//! remoteman for Windows: the first working slice. A Win32 window, Direct3D 11, FFmpeg's D3D11VA
//! decoders on the same device, and input sent back. There is no interface yet beyond the
//! picture; what the designed app needs (a list of computers, pairing, the bar) comes on top.
//!
//!   remoteman-windows.exe HOST:PORT [--pair] [--state DIR] [--size WxH] [--refresh HZ]
//!                         [--decoders av1,hevc] [--seconds N] [--fullscreen]
//! and, for testing with nobody looking:
//!   --shot FILE.png   a few seconds in, save what is about to be presented
//!   --stats           one line a second
//!   --input-demo      move the pointer, click, type and scroll
//!   --log FILE        what is printed, in a file as well
//!
//! Who does what:
//! - the main thread owns the window: input, the cursor, and when to leave (`window.rs`);
//! - the core's threads hand frames to the decode thread (`stream.rs`, `decode.rs`);
//! - the draw thread waits for the display and presents the newest picture (`draw.rs`).
//!
//! Pictures stay on the GPU from the decoder to the screen.

#[cfg(not(windows))]
fn main() {
    eprintln!("remoteman-viewer-windows only runs on Windows; the Linux viewer is remoteman-viewer");
}

#[cfg(windows)]
#[macro_use]
mod log;
#[cfg(windows)]
mod args;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod decode;
#[cfg(windows)]
mod demo;
#[cfg(windows)]
mod draw;
#[cfg(windows)]
mod gpu;
#[cfg(windows)]
mod keys;
#[cfg(windows)]
mod shot;
#[cfg(windows)]
mod stream;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    let code = match run() {
        Ok(shown) if shown > 0 => 0,
        Ok(_) => {
            say!("no picture was shown");
            1
        }
        Err(e) => {
            say!("failed: {e:#}");
            2
        }
    };
    // Not a return: nothing still running (a stuck decoder, the core's threads) may keep the
    // process alive.
    std::process::exit(code);
}

/// Runs a session from start to end; returns how many pictures reached the screen.
#[cfg(windows)]
fn run() -> anyhow::Result<u32> {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    use remoteman_core::{Client, Display, Options, Placement};

    let args = args::parse()?;
    if let Some(path) = &args.log {
        log::to_file(path)?;
    }
    if let Some(seconds) = args.seconds {
        // The last resort: whatever hangs, the process ends a little after it was told to.
        std::thread::Builder::new().name("deadline".into()).spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds + 5));
            say!("still running 5 s after --seconds was up: leaving the hard way");
            std::process::exit(3);
        })?;
    }

    let gpu = gpu::Gpu::open()?;
    say!("gpu: {}", gpu.name);
    let mut decoders = Vec::new();
    for mode in &args.decoders {
        match gpu.decodes(*mode) {
            Ok(()) => decoders.push(*mode),
            Err(e) => say!("decoder {:?} left out: {e:#}", mode.codec),
        }
    }
    anyhow::ensure!(!decoders.is_empty(), "this card decodes none of what was asked for");
    say!("decoders offered: {}", decoders.iter().map(|m| format!("{:?}", m.codec).to_lowercase()).collect::<Vec<_>>().join(", "));

    let refresh = args.refresh.unwrap_or_else(window::monitor_refresh);
    let (shared, feed) = stream::start(&gpu)?;
    let options = Options {
        host: args.host.clone(),
        state: args.state.clone(),
        name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows".into()),
        pair: args.pair,
        display: Display { width: args.size.0, height: args.size.1, scale: 1.0, refresh, placement: Placement::default() },
        decoders,
        test_drop_one_in: None,
        test_delay_lost_ms: None,
        cursor: true,
        unlock: None,
    };
    say!("connecting to {} for {}x{} at {} Hz", args.host, args.size.0, args.size.1, refresh);
    let client = Arc::new(Client::connect(options, feed)?);
    say!("host: {} ({}), features {:?}", client.host_name, client.host_os, client.host_features);

    let hwnd = window::create(&shared, &client, args.size, args.fullscreen)?;
    let drawing = draw::start(gpu.clone(), shared.clone(), hwnd, args.shot.clone())?;
    if args.stats {
        stream::print_stats(shared.clone(), client.clone());
    }
    if args.input_demo {
        demo::start(shared.clone(), client.clone());
    }

    let began = Instant::now();
    window::run(&shared, args.seconds.map(|s| began + Duration::from_secs(s)));
    shared.quit.store(true, Ordering::Relaxed);
    let _ = drawing.join();

    let stats = client.stats();
    let seconds = began.elapsed().as_secs_f64();
    let (decoded, shown) = (shared.decoded_total.load(Ordering::Relaxed), shared.presented_total.load(Ordering::Relaxed));
    say!(
        "in {seconds:.1} s: decoded {decoded} ({:.1} a second), presented {shown} ({:.1} a second), replaced unseen {}, lost {}, keyframe requests {}, round trip {:.1} ms, {}",
        decoded as f64 / seconds,
        shown as f64 / seconds,
        shared.replaced_total.load(Ordering::Relaxed),
        stats.lost,
        stats.keyframe_requests,
        stats.round_trip_us as f64 / 1000.0,
        shared.decoder.lock().unwrap(),
    );
    if let Some(reason) = shared.closed.lock().unwrap().as_deref() {
        say!("the session ended: {reason}");
    }
    Ok(shown)
}
