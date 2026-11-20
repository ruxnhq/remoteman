//! This PC's microphone, lent to the computer (docs/PROTOCOL.md, "Microphone"): Windows' default
//! microphone recorded through WASAPI as mono 48 kHz floats (Windows converts from whatever the
//! device runs at), and made into 10 ms Opus packets by the Linux viewer's framer
//! (`remoteman_audio::Framer`: the same Opus settings, and silence left out after 100 ms of it).
//!
//! One thread records. Each packet is stamped with when its first sample was recorded, counted
//! from the samples themselves (WASAPI's clock only where the device says it skipped), so the
//! framer sees one continuous stream until the device really stops.
//!
//! A microphone Windows' privacy settings keep from apps, or one that is muted, records silence
//! rather than failing: [`Stats`] tells silence from sound, and the log says which it was.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_audio::{Channels, Framer, Packet, SAMPLE_RATE};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STGM_READ};
use windows::core::BSTR;

/// What is recorded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// Windows' default microphone.
    Microphone,
    /// For tests on a PC with no microphone: what this PC plays (WASAPI loopback of the
    /// default output).
    Loopback,
}

/// What was recorded, for the log and the tests.
#[derive(Default)]
pub struct Stats {
    /// Samples recorded, and those of them that weren't silence (at least one step of 16-bit
    /// sound).
    pub samples: AtomicU64,
    pub sound: AtomicU64,
    /// The loudest sample, as an f32's bits.
    pub peak: AtomicU32,
    /// Packets made.
    pub packets: AtomicU32,
}

impl Stats {
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.peak.load(Ordering::Relaxed))
    }

    pub fn said(&self) -> String {
        let (samples, sound) = (self.samples.load(Ordering::Relaxed), self.sound.load(Ordering::Relaxed));
        let what = if samples == 0 {
            "nothing recorded yet".to_owned()
        } else if sound == 0 {
            "only silence (a muted microphone, or one Windows keeps from apps, records silence)".to_owned()
        } else {
            format!("sound in {:.0} % of it, loudest {:.3}", sound as f64 * 100.0 / samples as f64, self.peak())
        };
        format!("{:.1} s recorded, {} packets, {what}", samples as f64 / SAMPLE_RATE as f64, self.packets.load(Ordering::Relaxed))
    }
}

/// The microphone, being recorded. Dropping it stops it and lets go of the device.
pub struct Capture {
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub stats: Arc<Stats>,
    /// The device's name, as Windows shows it.
    #[allow(dead_code)]
    pub device: String,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        say!("microphone: stopped, {}", self.stats.said());
    }
}

impl Capture {
    /// Start recording; `on_packet` gets every Opus packet, on the recording thread, with
    /// `capture_us` on a clock that starts now. Fails if there is no such device or Windows
    /// won't open it.
    pub fn open(source: Source, on_packet: impl FnMut(Packet) + Send + 'static) -> Result<Self> {
        let quit = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let (q, s, counted) = (quit.clone(), stats.clone(), stats.clone());
        let mut on_packet = on_packet;
        let framer = Framer::new(
            Channels::Mono,
            Box::new(move |packet| {
                counted.packets.fetch_add(1, Ordering::Relaxed);
                on_packet(packet);
            }),
        )?;
        let thread = std::thread::Builder::new().name("microphone".into()).spawn(move || {
            if let Err(e) = record(source, framer, &q, &s, &ready_tx) {
                // Before it was ready: the caller hears of it. After: only the log does.
                if ready_tx.send(Err(anyhow::anyhow!("{e:#}"))).is_err() {
                    say!("microphone: stopped: {e:#}");
                }
            }
        })?;
        let device = match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(device)) => device,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                quit.store(true, Ordering::Relaxed);
                anyhow::bail!("the microphone didn't answer");
            }
        };
        say!("microphone: recording {device}, mono 48 kHz, as 10 ms Opus packets");
        Ok(Self { quit, thread: Some(thread), stats, device })
    }
}

fn name_of(device: &IMMDevice) -> String {
    // SAFETY: plain COM calls on a live device.
    unsafe { device.OpenPropertyStore(STGM_READ).and_then(|store| store.GetValue(&PKEY_Device_FriendlyName)) }.ok().and_then(|v| BSTR::try_from(&v).ok()).map(|b| b.to_string()).unwrap_or_default()
}

fn record(source: Source, mut framer: Framer, quit: &AtomicBool, stats: &Stats, ready: &mpsc::Sender<Result<String>>) -> Result<()> {
    const SILENT: f32 = 1.0 / 32768.0;
    // SAFETY: COM calls on this thread, which initialises COM for itself; the capture buffer is
    // read within the frames WASAPI says it holds, and given back before the next.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("no audio devices")?;
        let device = match source {
            Source::Microphone => devices.GetDefaultAudioEndpoint(eCapture, eConsole).context("this PC has no microphone")?,
            Source::Loopback => devices.GetDefaultAudioEndpoint(eRender, eConsole).context("this PC has no sound output")?,
        };
        let name = name_of(&device);
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("open the microphone")?;
        let format = WAVEFORMATEX {
            // IEEE float, one channel, as the protocol's microphone is.
            wFormatTag: 3,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        let mut flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        if source == Source::Loopback {
            flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
        }
        // A tenth of a second of buffer, in units of 100 ns.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &format, None).context("set the microphone up (Windows may be keeping it from apps)")?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start().context("start the microphone")?;
        let _ = ready.send(Ok(if name.is_empty() { "the default microphone".into() } else { name }));

        let began = Instant::now();
        // Where the samples so far end, in microseconds on the session's clock (from `began`);
        // `None` until the first packet, and after the device says it skipped.
        let mut at_us: Option<f64> = None;
        let mut said = Instant::now();
        let mut silence = Vec::new();
        while !quit.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
            loop {
                let waiting = capture.GetNextPacketSize()?;
                if waiting == 0 {
                    break;
                }
                let (mut data, mut frames, mut bits) = (std::ptr::null_mut(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut bits, None, None)?;
                let n = frames as usize;
                let samples: &[f32] = if bits & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    silence.resize(n, 0.0);
                    &silence
                } else {
                    std::slice::from_raw_parts(data as *const f32, n)
                };
                if at_us.is_none() || bits & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                    // The first packet, or the device skipped: from now, less what it holds.
                    let now_us = began.elapsed().as_micros() as f64;
                    let held_us = n as f64 * 1e6 / SAMPLE_RATE as f64;
                    at_us = Some((now_us - held_us).max(0.0));
                }
                let first = at_us.unwrap();
                stats.samples.fetch_add(n as u64, Ordering::Relaxed);
                let loud = samples.iter().filter(|s| s.abs() >= SILENT).count();
                stats.sound.fetch_add(loud as u64, Ordering::Relaxed);
                let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
                if peak > stats.peak() {
                    stats.peak.store(peak.to_bits(), Ordering::Relaxed);
                }
                framer.feed(samples, first as u64);
                at_us = Some(first + n as f64 * 1e6 / SAMPLE_RATE as f64);
                capture.ReleaseBuffer(frames)?;
            }
            if said.elapsed() >= Duration::from_secs(10) {
                said = Instant::now();
                say!("microphone: {}", stats.said());
            }
        }
        let _ = client.Stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use remoteman_audio::FRAME_US;
    use std::sync::Mutex;

    /// Record `seconds` from `source`, decode the packets again, and say what came out. If
    /// `REMOTEMAN_TEST_OUT` names a folder, the packets are written there as
    /// `mic-<what>.opus-packets` (a u16 length before each) and the decoded sound as a 48 kHz
    /// mono WAV beside it, for listening and for FFmpeg (it is a recording of the room).
    fn record_and_measure(source: Source, seconds: u64, what: &str) -> (Arc<Stats>, Vec<Packet>, Vec<f32>) {
        let packets = Arc::new(Mutex::new(Vec::new()));
        let p = packets.clone();
        let capture = match Capture::open(source, move |packet| p.lock().unwrap().push(packet)) {
            Ok(c) => c,
            Err(e) => panic!("{what}: can't record: {e:#}"),
        };
        println!("{what}: recording {}", capture.device);
        let stats = capture.stats.clone();
        std::thread::sleep(Duration::from_secs(seconds));
        drop(capture);
        let packets = std::mem::take(&mut *packets.lock().unwrap());
        // Decoded with the same crate's decoder, as the host decodes them (concealing a hole in
        // the sequence, which only a pause makes here).
        let mut decoder = remoteman_audio::Decoder::new(Channels::Mono).unwrap();
        let mut out = Vec::new();
        let mut frame = vec![0f32; remoteman_audio::FRAME_SAMPLES];
        for packet in &packets {
            decoder.decode(&packet.data, &mut frame).unwrap();
            out.extend_from_slice(&frame);
        }
        let dir = std::env::var_os("REMOTEMAN_TEST_OUT").map(std::path::PathBuf::from);
        if let Some(dir) = dir.filter(|d| std::fs::create_dir_all(d).is_ok()) {
            let mut raw = Vec::new();
            for packet in &packets {
                raw.extend_from_slice(&(packet.data.len() as u16).to_le_bytes());
                raw.extend_from_slice(&packet.data);
            }
            let _ = std::fs::write(dir.join(format!("mic-{what}.opus-packets")), raw);
            let _ = std::fs::write(dir.join(format!("mic-{what}-decoded.wav")), wav(&out));
        }
        (stats, packets, out)
    }

    fn wav(samples: &[f32]) -> Vec<u8> {
        let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }

    /// The real microphone, for five seconds: it opens, records, and its packets decode as
    /// 10 ms Opus. Whether there was sound depends on the room; the test says which.
    /// `cargo test -p remoteman-viewer-windows --lib microphone -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_real_microphone_records_and_encodes() {
        let (stats, packets, out) = record_and_measure(Source::Microphone, 5, "microphone");
        println!("microphone: {}", stats.said());
        println!("microphone: {} packets, {} samples decoded, loudest decoded {:.4}", packets.len(), out.len(), remoteman_audio::testing::peak(&out));
        assert!(stats.samples.load(Ordering::Relaxed) > SAMPLE_RATE as u64 * 4, "the microphone gave less than 4 s in 5");
        // Sequence numbers count up by one per packet.
        assert!(packets.windows(2).all(|w| w[1].seq == w[0].seq.wrapping_add(1)));
    }

    /// What this PC plays, recorded through the same path (for a PC with no microphone, and to
    /// measure a known tone: play one while it runs).
    #[test]
    #[ignore]
    fn loopback_records_and_encodes() {
        let (stats, packets, out) = record_and_measure(Source::Loopback, 4, "loopback");
        println!("loopback: {}", stats.said());
        println!("loopback: {} packets, tone at {:.1} Hz, loudest decoded {:.4}", packets.len(), remoteman_audio::testing::frequency(&out), remoteman_audio::testing::peak(&out));
    }

    /// The framer with a known tone, as the recording thread feeds it (pieces of uneven size,
    /// stamped from the sample count): packets of 10 ms that decode back to the tone.
    #[test]
    fn a_tone_fed_in_pieces_comes_back() {
        let packets = Arc::new(Mutex::new(Vec::<Packet>::new()));
        let p = packets.clone();
        let mut framer = Framer::new(Channels::Mono, Box::new(move |packet| p.lock().unwrap().push(packet))).unwrap();
        let tone = remoteman_audio::testing::sine(440.0, 0.5, SAMPLE_RATE as usize, 1);
        let mut at = 0usize;
        for (i, size) in [441usize, 96, 1000, 480, 7].iter().cycle().enumerate() {
            if at >= tone.len() || i > 10_000 {
                break;
            }
            let end = (at + size).min(tone.len());
            framer.feed(&tone[at..end], (at as u64 * 1_000_000) / SAMPLE_RATE as u64);
            at = end;
        }
        let packets = packets.lock().unwrap();
        assert_eq!(packets.len(), 100);
        let mut decoder = remoteman_audio::Decoder::new(Channels::Mono).unwrap();
        let mut out = Vec::new();
        let mut frame = vec![0f32; remoteman_audio::FRAME_SAMPLES];
        for (i, packet) in packets.iter().enumerate() {
            assert_eq!(packet.seq, i as u32);
            assert_eq!(packet.capture_us, i as u64 * FRAME_US);
            decoder.decode(&packet.data, &mut frame).unwrap();
            out.extend_from_slice(&frame);
        }
        let hz = remoteman_audio::testing::frequency(&out[4800..]);
        assert!((hz - 440.0).abs() < 2.0, "{hz} Hz");
    }
}
