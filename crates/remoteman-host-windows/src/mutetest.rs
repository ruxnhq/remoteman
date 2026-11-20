//! An experiment, not a test of the host: does WASAPI loopback of the default output still get
//! the sound while that output is muted (`IAudioEndpointVolume::SetMute`)? And how late is the
//! loopback? Run by hand: `cargo test --release -p remoteman-host-windows mutetest -- --ignored --nocapture`.
//! Plays a quiet 440 Hz tone for about five seconds and mutes the endpoint for about a second;
//! the prior mute state is put back by a guard, also on a panic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{AUDCLNT_SHAREMODE_SHARED, IAudioCaptureClient, IAudioClient, IAudioClock, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

const RATE: u32 = 48_000;
const LOOPBACK: u32 = 0x0002_0000;
const CONVERT: u32 = 0x8000_0000 | 0x0800_0000;

fn format() -> WAVEFORMATEX {
    WAVEFORMATEX { wFormatTag: 3, nChannels: 2, nSamplesPerSec: RATE, nAvgBytesPerSec: RATE * 8, nBlockAlign: 8, wBitsPerSample: 32, cbSize: 0 }
}

/// QPC now, in 100 ns units.
fn qpc_now() -> f64 {
    let (mut c, mut f) = (0i64, 0i64);
    unsafe {
        let _ = QueryPerformanceCounter(&mut c);
        let _ = QueryPerformanceFrequency(&mut f);
    }
    c as f64 * 1e7 / f as f64
}

/// Puts the endpoint's mute state back, always.
struct MuteGuard {
    vol: IAudioEndpointVolume,
    prior: bool,
}
impl Drop for MuteGuard {
    fn drop(&mut self) {
        let r = unsafe { self.vol.SetMute(self.prior, std::ptr::null()) };
        eprintln!("mute restored to {} ({r:?})", self.prior);
    }
}

#[derive(Default)]
struct Click {
    /// QPC (100 ns) at which the click's first frame plays.
    plays_at: Option<f64>,
}

fn render(stop: Arc<AtomicBool>, click: Arc<Mutex<Click>>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
        let device = e.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
        let f = format();
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, CONVERT, 2_000_000, 0, &f, None).unwrap();
        let size = client.GetBufferSize().unwrap();
        let out: IAudioRenderClient = client.GetService().unwrap();
        let clock: IAudioClock = client.GetService().unwrap();
        let freq = clock.GetFrequency().unwrap() as f64;
        client.Start().unwrap();
        let mut written: u64 = 0;
        // 0.3 s silence, a 2 ms click, silence until 1.0 s, then the tone.
        let click_at = (RATE as u64) * 3 / 10;
        let click_len = (RATE / 500) as u64;
        let tone_from = RATE as u64;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
            let pad = client.GetCurrentPadding().unwrap();
            let room = size - pad;
            if room == 0 {
                continue;
            }
            let data = out.GetBuffer(room).unwrap() as *mut f32;
            for i in 0..room as u64 {
                let n = written + i;
                let v = if n >= click_at && n < click_at + click_len {
                    0.5
                } else if n >= tone_from {
                    0.1 * (2.0 * std::f32::consts::PI * 440.0 * (n - tone_from) as f32 / RATE as f32).sin()
                } else {
                    0.0
                };
                *data.add(i as usize * 2) = v;
                *data.add(i as usize * 2 + 1) = v;
            }
            out.ReleaseBuffer(room, 0).unwrap();
            let before = written;
            written += room as u64;
            let mut c = click.lock().unwrap();
            if c.plays_at.is_none() && written > click_at && before <= click_at {
                let (mut pos, mut qpc) = (0u64, 0u64);
                if clock.GetPosition(&mut pos, Some(&mut qpc)).is_ok() {
                    let played = pos as f64 * RATE as f64 / freq;
                    c.plays_at = Some(qpc as f64 + (click_at as f64 - played) * 1e7 / RATE as f64);
                }
            }
        }
        let _ = client.Stop();
    }
}

fn rms(v: &[f32]) -> f64 {
    (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

/// Drain the loopback for `dur`; returns the samples (one channel) and, for each packet,
/// (qpc timestamp of its first frame, qpc when read, first index in samples).
fn drain(cap: &IAudioCaptureClient, dur: Duration, log: &mut Vec<(f64, f64, usize)>, base: usize) -> Vec<f32> {
    let end = Instant::now() + dur;
    let mut all = Vec::new();
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(2));
        unsafe {
            while cap.GetNextPacketSize().unwrap() > 0 {
                let (mut d, mut n, mut fl, mut qpc) = (std::ptr::null_mut::<u8>(), 0u32, 0u32, 0u64);
                cap.GetBuffer(&mut d, &mut n, &mut fl, None, Some(&mut qpc)).unwrap();
                log.push((qpc as f64, qpc_now(), base + all.len()));
                if fl & 2 != 0 || d.is_null() {
                    all.extend(std::iter::repeat_n(0.0, n as usize));
                } else {
                    let s = std::slice::from_raw_parts(d as *const f32, n as usize * 2);
                    all.extend(s.chunks(2).map(|c| c[0]));
                }
                cap.ReleaseBuffer(n).unwrap();
            }
        }
    }
    all
}

#[test]
#[ignore]
fn loopback_while_muted() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
        let device = e.GetDefaultAudioEndpoint(eRender, eConsole).unwrap();
        let vol: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).unwrap();
        let prior = vol.GetMute().unwrap().as_bool();
        eprintln!("endpoint mute before: {prior}; master volume {:?}", vol.GetMasterVolumeLevelScalar());
        let guard = MuteGuard { vol: vol.clone(), prior };

        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, LOOPBACK | CONVERT, 2_000_000, 0, &format(), None).unwrap();
        let cap: IAudioCaptureClient = client.GetService().unwrap();
        client.Start().unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        let click = Arc::new(Mutex::new(Click::default()));
        let th = {
            let (s, c) = (stop.clone(), click.clone());
            std::thread::spawn(move || render(s, c))
        };

        let mut log = Vec::new();
        let head = drain(&cap, Duration::from_millis(1500), &mut log, 0);
        let unmuted = drain(&cap, Duration::from_secs(1), &mut Vec::new(), 0);
        guard.vol.SetMute(true, std::ptr::null()).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let _ = drain(&cap, Duration::from_millis(10), &mut Vec::new(), 0);
        let muted = drain(&cap, Duration::from_secs(1), &mut Vec::new(), 0);
        guard.vol.SetMute(prior, std::ptr::null()).unwrap();
        let after = drain(&cap, Duration::from_millis(500), &mut Vec::new(), 0);
        stop.store(true, Ordering::Relaxed);
        th.join().unwrap();
        let _ = client.Stop();
        drop(guard);

        let peak = |v: &[f32]| v.iter().fold(0f32, |a, b| a.max(b.abs()));
        eprintln!("peaks: head {:.4} unmuted {:.4} muted {:.4}", peak(&head), peak(&unmuted), peak(&muted));
        eprintln!("RMS unmuted = {:.5}   muted = {:.5}   after restore = {:.5}   (tone amplitude 0.1 -> expected ~0.0707)", rms(&unmuted), rms(&muted), rms(&after));

        if let (Some(i), Some(plays)) = (head.iter().position(|s| s.abs() > 0.05), click.lock().unwrap().plays_at) {
            let p = log.iter().rev().find(|p| p.2 <= i).unwrap();
            let stamp = p.0 + (i - p.2) as f64 * 1e7 / RATE as f64;
            eprintln!("click: loopback timestamp - render play time = {:.1} ms; read by us - render play time = {:.1} ms", (stamp - plays) / 1e4, (p.1 - plays) / 1e4);
        } else {
            eprintln!("click not found in loopback (or no render position)");
        }
    }
}
