//! `--probe`: open the display driver, say what is found (the driver, its frame channel, the
//! monitor and its modes, the adapter, the shared textures), and exit. It listens for no
//! client, switches no mode and sends no input. One thing it can't avoid: asking about the
//! frame channel is what makes the driver start copying frames into it, and the driver goes on
//! doing so until it restarts.
//!
//! `--probe-picture` goes on from there through the picture path: it takes one frame, converts
//! it, and encodes it once in each mode the host offers. That is a moment of GPU work, hence
//! the flag of its own.

use std::time::Duration;

use anyhow::{Context, Result};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::Threading::GetCurrentProcessId;

use crate::capture::{Gpu, Source, Take};
use crate::convert::{self, Converter};
use crate::display::{self, nudge};
use crate::encode;
use crate::vdd::Driver;

/// Said both on the console and in the log.
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        println!("{line}");
        tracing::info!("probe: {line}");
    }};
}

pub fn run(picture: bool) -> Result<()> {
    // Where this process is: the display calls and input need the desktop's session.
    let (mut session, pid) = (0u32, unsafe { GetCurrentProcessId() });
    // SAFETY: a live out-parameter.
    let known = unsafe { ProcessIdToSessionId(pid, &mut session) }.is_ok();
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    say!("process {pid} in session {} (the console's session is {console}){}", if known { session.to_string() } else { "?".into() }, if known && session == 0 { ": session 0 has no desktop, so no mode changes, input or cursor from here, and the desktop can't be made to draw" } else { "" });

    // 1. What Windows lists.
    let outputs = display::outputs();
    for o in &outputs {
        let mode = display::current(&o.device).map(|(m, a)| format!("{}x{} at {} Hz, at {},{}", m.width, m.height, m.refresh, a.left, a.top)).unwrap_or_else(|e| format!("{e:#}"));
        say!("output {}{}: {} [{}], {mode}", o.device, if o.primary { " (primary)" } else { "" }, o.description, o.id);
    }
    // (On a PC with screens of its own the driver's monitors are off the desktop while no device
    // is here: then its first monitor's output, which shows no frames until a device comes.)
    let output = match display::find_output().or_else(|e| display::driver_output(0).map(|o| (o, true)).ok_or(e)) {
        Ok((o, sure)) => {
            say!("the driver's monitor is taken to be {} ({})", o.device, if sure { "its adapter is the driver's" } else { "no adapter looks like the driver's; this is the primary display" });
            let modes = display::modes(&o.device);
            let mut sizes: Vec<(u32, u32)> = modes.iter().map(|m| (m.width, m.height)).collect();
            sizes.dedup();
            sizes.sort();
            sizes.dedup();
            let mut rates: Vec<u32> = modes.iter().map(|m| m.refresh).collect();
            rates.sort();
            rates.dedup();
            say!("it offers {} modes: sizes {}; rates {:?}", modes.len(), sizes.iter().map(|(w, h)| format!("{w}x{h}")).collect::<Vec<_>>().join(" "), rates);
            Some(o)
        }
        Err(e) => {
            say!("FAILED: {e:#}");
            None
        }
    };

    // 2. The driver.
    let driver = Driver::open().inspect_err(|e| say!("FAILED: {e:#}. Nothing further can be tried."))?;
    say!("driver: opened {}", driver.path);
    driver.ping().inspect_err(|e| say!("FAILED: {e:#}"))?;
    say!("driver: answers a ping");
    let caps = driver.caps().inspect_err(|e| say!("FAILED: {e:#}"))?;
    say!("frame channel: version {}, flags {:#x}, {} slots, metadata of {} bytes (asking is what starts the driver filling it)", caps.version, caps.flags, caps.max_slots, caps.metadata_size);

    // 3. The channel, which the driver makes at the next frame Windows shows.
    let at = output.as_ref().and_then(|o| display::current(&o.device).ok()).map_or((0, 0), |(_, a)| (a.left, a.top));
    let mut channel = None;
    for attempt in 0..40 {
        match driver.open_channel(0).inspect_err(|e| say!("FAILED: {e:#}"))? {
            Some(c) => {
                say!("frame channel: opened{}", if attempt > 0 { format!(" at try {}", attempt + 1) } else { String::new() });
                channel = Some(c);
                break;
            }
            None => {
                if attempt == 0 {
                    say!("frame channel: not there yet (no frame since the driver was first asked); making the desktop draw and trying for 6 s");
                }
                nudge(at);
                std::thread::sleep(Duration::from_millis(120));
            }
        }
    }
    let Some(channel) = channel else {
        say!("FAILED: the frame channel never appeared. From session 0 that is expected on a still desktop; run the probe in the desktop's session, or move something on the screen while it runs.");
        anyhow::bail!("no frame channel");
    };
    let meta = channel.snapshot().context("metadata").inspect_err(|e| say!("FAILED: the metadata isn't readable: {e:#}"))?;
    say!(
        "metadata: {}x{}, DXGI format {}, hdr {}, nits max {} min {} fall {}, frame {} in slot {} of {}, generation {}, adapter LUID {:08x}:{:08x}, driver's counts: replaced unread {}, dropped (held) {}, dropped (failed) {}",
        meta.width,
        meta.height,
        meta.format,
        meta.is_hdr,
        meta.max_nits,
        meta.min_nits,
        meta.max_fall,
        meta.frame_counter,
        meta.slot,
        meta.slot_count,
        meta.generation(),
        meta.luid_high,
        meta.luid_low,
        meta.replaced_unread,
        meta.dropped_held,
        meta.dropped_failed
    );
    say!("slot handles: {}", channel.slots.iter().map(|h| format!("{:#x}", h.0 as usize)).collect::<Vec<_>>().join(" "));

    // 4. A device on the driver's adapter, and the textures on it.
    let gpu = Gpu::on_adapter((meta.luid_low, meta.luid_high)).inspect_err(|e| say!("FAILED: {e:#}"))?;
    say!("adapter: {}", gpu.adapter);
    let mut source = Source::open(&gpu, channel).inspect_err(|e| say!("FAILED: {e:#}"))?;
    let desc = source.format()?;
    say!("textures: opened, {}x{} DXGI format {}, bind flags {:#x}, misc flags {:#x}", desc.Width, desc.Height, desc.Format.0, desc.BindFlags, desc.MiscFlags);

    if !picture {
        say!("probe done (--probe-picture goes on to take, convert and encode one frame)");
        return Ok(());
    }

    // 5. Convert one frame.
    let (input, note) = convert::input_for(meta.format, meta.is_hdr != 0, crate::hdr::USUAL_SDR_WHITE).inspect_err(|e| say!("FAILED: {e:#}"))?;
    say!("conversion: the surface is read as {input:?}{}", note.map(|n| format!(" ({n})")).unwrap_or_default());
    let converter = Converter::new(&gpu, meta.width, meta.height, input, convert::Output::Sdr, false).inspect_err(|e| say!("FAILED: {e:#}"))?;
    say!("conversion: shaders compiled, P010 target made");
    let mut converted = false;
    for attempt in 0..20 {
        match source.take(&gpu, true, |view, m| {
            converter.convert(&gpu, view);
            (m.frame_counter, m.slot)
        }) {
            Ok(Take::Frame((frame, slot))) => {
                say!("capture: took frame {frame} from slot {slot} (its keyed mutex was had) and converted it");
                converted = true;
                break;
            }
            Ok(Take::Stale) => {
                say!("capture: the driver re-made its textures just now (a mode change?); run the probe again");
                break;
            }
            Ok(Take::Nothing) => {
                if attempt == 0 {
                    say!("capture: the newest texture couldn't be taken at once; trying for 2 s");
                }
                nudge(at);
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                say!("FAILED: capture: {e:#}");
                break;
            }
        }
    }
    if !converted {
        say!("FAILED: no frame could be taken, so what is encoded below is an empty picture");
    }

    // 6. Encode it, in each mode this host offers.
    for name in crate::mf::names() {
        say!("Media Foundation hardware encoder: {name}");
    }
    match encode::library() {
        Ok(from) => say!("encoder library: {from}"),
        Err(e) => say!("FAILED: {e:#}"),
    }
    // Each mode, and HDR where the mode has it, with an input of its own layout (4:4:4: RGB
    // that NVENC converts).
    let tries: Vec<(remoteman_core::Mode, bool)> = encode::MODES.into_iter().flat_map(|m| if m.chroma == remoteman_core::Chroma::C444 { vec![(m, false)] } else { vec![(m, false), (m, true)] }).collect();
    for (mode, hdr) in tries {
        let layout = mode.chroma == remoteman_core::Chroma::C444;
        let settings = encode::Settings { mode, width: meta.width, height: meta.height, fps: 60, bitrate: 20_000_000, hdr };
        let output = if hdr { convert::Output::Pq { sdr_white: crate::hdr::USUAL_SDR_WHITE } } else { convert::Output::Sdr };
        let mode_name = format!("{:?} {:?}{}", mode.codec, mode.chroma, if hdr { " HDR" } else { "" });
        let converter = match Converter::new(&gpu, meta.width, meta.height, input, output, layout) {
            Ok(c) => {
                let _ = source.take(&gpu, true, |view, _| c.convert(&gpu, view));
                c
            }
            Err(e) => {
                say!("FAILED: conversion for {mode_name}: {e:#}");
                continue;
            }
        };
        match encode::open(&gpu, &converter.output, &settings) {
            Ok(mut encoder) => {
                let note = encoder.note().to_owned();
                let (short, long) = (encoder.short_term(), encoder.long_term());
                match encoder.encode_how(0, 0, remoteman_encode::How { idr: true, ..Default::default() }) {
                    Ok(made) => {
                        say!("encoder {mode_name}: opened ({note}; {short} recent and {long} long-term frames); one frame is {} bytes, keyframe {}", made.data.len(), made.key);
                        // The frame, for decoding elsewhere (REMOTEMAN_PROBE_DUMP=DIR).
                        if let Some(dir) = std::env::var_os("REMOTEMAN_PROBE_DUMP") {
                            let name = mode_name.replace([' ', '(', ')'], "_");
                            let _ = std::fs::write(std::path::Path::new(&dir).join(format!("{name}.{}", if mode.codec == remoteman_core::Codec::Av1 { "obu" } else { "h265" })), &made.data);
                        }
                    }
                    Err(e) => say!("FAILED: encoder {mode_name} opened ({note}) but didn't encode: {e:#}"),
                }
            }
            Err(e) => say!("FAILED: encoder {mode_name}: {e:#}"),
        }
    }
    say!("probe done");
    Ok(())
}
