//! Sound. The desktop's sound to the device: what the default output plays is recorded back
//! from it (WASAPI loopback) and sent as Opus, set up as the Linux host sets it up: 48 kHz,
//! stereo, 10 ms a packet, restricted low delay. And the device's microphone to the desktop:
//! its mono Opus packets are played into a virtual sound device whose other end programs can
//! record from. That device is not ours (a kernel driver would need a paid signature): it is
//! Valve's "Steam Streaming Microphone" if Steam is installed, or VB-Audio's cable.
//!
//! Loopback records nothing while nothing plays, and digital silence while a program holds
//! the output open without playing: neither is sent. The first packet after such a pause is
//! marked as a start, as the protocol asks.
//!
//! The default output is followed when the user picks another. Not done: making "Steam
//! Streaming Speakers" the output so that the PC itself stays quiet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use opusic_c::{Application, Bitrate, InbandFec, SampleRate};
use remoteman_core::host::Link;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_EXCLUSIVE, AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, STGM_READ};
use windows::core::BSTR;

pub const SAMPLE_RATE: u32 = 48_000;
/// Samples of one channel in a packet: 10 ms.
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_US: u64 = 10_000;
/// The largest packet Opus makes for one frame.
const MAX_PACKET: usize = 1275;
/// As the Linux host: what the encoder is told to expect to lose.
const EXPECTED_LOSS_PERCENT: u8 = 5;
const STEREO_BITRATE: u32 = 128_000;
/// This many silent frames in a row (half a second) and sending pauses.
const SILENCE_FRAMES: u32 = 50;

const STREAM_LOOPBACK: u32 = 0x0002_0000;
/// Windows converts between our format and the device's own.
const STREAM_CONVERT: u32 = 0x8000_0000 | 0x0800_0000;
const BUFFER_SILENT: u32 = 0x2;
/// `WAVE_FORMAT_IEEE_FLOAT`.
const FLOAT: u16 = 3;

fn float_format(channels: u16) -> WAVEFORMATEX {
    WAVEFORMATEX { wFormatTag: FLOAT, nChannels: channels, nSamplesPerSec: SAMPLE_RATE, nAvgBytesPerSec: SAMPLE_RATE * 4 * channels as u32, nBlockAlign: 4 * channels, wBitsPerSample: 32, cbSize: 0 }
}

fn com() {
    // SAFETY: plain call; a thread that already has COM set up is told so and that is fine.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: COM is set up on this thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("Windows' list of sound devices")
}

fn name_of(device: &IMMDevice) -> String {
    // SAFETY: plain COM calls on a live device.
    unsafe { device.OpenPropertyStore(STGM_READ).and_then(|store| store.GetValue(&PKEY_Device_FriendlyName)) }.ok().and_then(|v| BSTR::try_from(&v).ok()).map(|b| b.to_string()).unwrap_or_default()
}

/// The desktop's sound, going to the device. Dropping it stops it.
pub struct Speaker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Speaker {
    pub fn start(link: Link) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let s = stop.clone();
        let thread = std::thread::Builder::new().name("sound".into()).spawn(move || {
            com();
            let opened = open_loopback();
            let (client, capture, id) = match opened {
                Ok((client, capture, name, id)) => {
                    let _ = ready_tx.send(Ok(name));
                    (client, capture, id)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            match record(client, capture, id, &link, &s) {
                Ok((packets, bytes, pauses)) => tracing::info!(packets, kilobytes = bytes / 1000, pauses, "the desktop's sound no longer goes to the device"),
                Err(e) => tracing::warn!("the desktop's sound stopped: {e:#}"),
            }
        })?;
        let name = ready_rx.recv().context("the sound's thread died")??;
        tracing::info!(output = name, "the desktop's sound goes to the device");
        Ok(Self { stop, thread: Some(thread) })
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An output to record instead of the default one: set while the PC's speakers are switched to
/// Steam's for a device that listens (`speakers.rs`), so the recording doesn't depend on what
/// Windows says the default is for this process.
pub static RECORD_FROM: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn record_from() -> Option<String> {
    RECORD_FROM.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The default output's id, to notice when the user picks another.
fn default_output() -> Option<String> {
    if let Some(id) = record_from() {
        return Some(id);
    }
    // SAFETY: COM calls on this thread's own objects; the id is read and freed.
    unsafe {
        let device = enumerator().ok()?.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

/// The default output, recorded back: its client, its recording side, its name and its id.
fn open_loopback() -> Result<(IAudioClient, IAudioCaptureClient, String, String)> {
    // SAFETY: COM calls as documented, on this thread's own objects; the format outlives the
    // call that reads it.
    unsafe {
        let device = match record_from() {
            Some(id) => enumerator()?.GetDevice(&windows::core::HSTRING::from(id)).context("the sound output to record has gone")?,
            None => enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole).context("this computer has no sound output to record from")?,
        };
        let name = name_of(&device);
        let id = device.GetId().ok().and_then(|i| {
            let t = i.to_string().ok();
            CoTaskMemFree(Some(i.0 as *const _));
            t
        }).unwrap_or_default();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the sound output")?;
        let format = float_format(2);
        // A fifth of a second of room; it is emptied every few milliseconds.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, STREAM_LOOPBACK | STREAM_CONVERT, 2_000_000, 0, &format, None).context("recording the sound output back")?;
        let capture: IAudioCaptureClient = client.GetService().context("the recording side of the sound output")?;
        client.Start().context("starting to record the sound output")?;
        Ok((client, capture, name, id))
    }
}

/// Record until told to stop, following the default output when the user picks another (the
/// first packet from the new one is marked as a start). Says how many packets went, their
/// bytes, and how many pauses.
fn record(mut client: IAudioClient, mut capture: IAudioCaptureClient, mut id: String, link: &Link, stop: &AtomicBool) -> Result<(u64, u64, u32)> {
    let mut turns = 0u32;
    let mut encoder = opusic_c::Encoder::new(opusic_c::Channels::Stereo, SampleRate::Hz48000, Application::LowDelay).map_err(|e| anyhow!("opus encoder: {e:?}"))?;
    encoder.set_inband_fec(InbandFec::Mode1).map_err(|e| anyhow!("opus FEC: {e:?}"))?;
    encoder.set_packet_loss(EXPECTED_LOSS_PERCENT).map_err(|e| anyhow!("opus expected loss: {e:?}"))?;
    encoder.set_bitrate(Bitrate::Value(STEREO_BITRATE)).map_err(|e| anyhow!("opus bitrate: {e:?}"))?;
    let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2 * 8);
    let (mut seq, mut packets, mut bytes, mut pauses) = (0u32, 0u64, 0u64, 0u32);
    // When the next frame's first sample was captured, while the sound runs on unbroken.
    let mut next = None::<u64>;
    let mut silent = 0u32;
    let mut out = [0u8; MAX_PACKET];
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(5));
        // Four times a second: is the default output still the one being recorded?
        turns += 1;
        if turns % 50 == 0 && default_output().is_some_and(|now| now != id) {
            // SAFETY: a live client.
            let _ = unsafe { client.Stop() };
            match open_loopback() {
                Ok((c, k, name, i)) => {
                    tracing::info!(output = name, "the default output changed: its sound goes to the device now");
                    (client, capture, id) = (c, k, i);
                    pending.clear();
                    next = None;
                    silent = 0;
                }
                Err(e) => return Err(e.context("the default output changed and couldn't be recorded")),
            }
        }
        // SAFETY: the buffer is the capture client's between GetBuffer and ReleaseBuffer, and
        // holds `frames` stereo frames of f32.
        unsafe {
            while capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut::<u8>(), 0u32, 0u32);
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let count = frames as usize * 2;
                if flags & BUFFER_SILENT != 0 || data.is_null() {
                    pending.extend(std::iter::repeat_n(0.0, count));
                } else {
                    pending.extend_from_slice(std::slice::from_raw_parts(data as *const f32, count));
                }
                capture.ReleaseBuffer(frames)?;
            }
        }
        while pending.len() >= FRAME_SAMPLES * 2 {
            let waiting = (pending.len() / (FRAME_SAMPLES * 2)) as u64;
            let frame: Vec<f32> = pending.drain(..FRAME_SAMPLES * 2).collect();
            silent = if frame.iter().all(|s| s.abs() < 1e-6) { silent + 1 } else { 0 };
            if silent > SILENCE_FRAMES {
                if next.take().is_some() {
                    pauses += 1;
                }
                continue;
            }
            // This frame was captured about as long ago as there are frames waiting.
            let now = link.now_us().saturating_sub(waiting * FRAME_US);
            let (at, start) = match next {
                Some(at) if at.abs_diff(now) < 6 * FRAME_US => (at, false),
                _ => (now, true),
            };
            next = Some(at + FRAME_US);
            let n = encoder.encode_float_to_slice(&frame, &mut out).map_err(|e| anyhow!("opus encode: {e:?}"))?;
            link.send_sound(start, seq, at, &out[..n]);
            seq = seq.wrapping_add(1);
            packets += 1;
            bytes += n as u64;
        }
    }
    // SAFETY: a live client.
    let _ = unsafe { client.Stop() };
    Ok((packets, bytes, pauses))
}

/// What a virtual device to play a microphone into is called, lower case: Valve's, VB-Audio's.
const MICROPHONE_SINKS: [&str; 2] = ["steam streaming microphone", "cable input"];

fn microphone_sink() -> Result<Option<(IMMDevice, String)>> {
    // SAFETY: COM calls on this thread's own objects.
    unsafe {
        let all = enumerator()?.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..all.GetCount()? {
            let device = all.Item(i)?;
            let name = name_of(&device);
            // For tests where neither is installed: REMOTEMAN_TEST_MIC_SINK names another output
            // to play the microphone into (the guest's speakers, recorded back by loopback).
            let test = std::env::var("REMOTEMAN_TEST_MIC_SINK").ok().filter(|t| !t.is_empty()).map(|t| t.to_lowercase());
            if MICROPHONE_SINKS.iter().any(|s| name.to_lowercase().contains(s)) || test.is_some_and(|t| name.to_lowercase().contains(&t)) {
                return Ok(Some((device, name)));
            }
        }
    }
    Ok(None)
}

/// The name of the device a client's microphone would be played into, if this computer has
/// one. Every output's name comes back too, for the log.
pub fn find_microphone_sink() -> (Option<String>, Vec<String>) {
    com();
    let found = microphone_sink().ok().flatten().map(|(_, name)| name);
    // SAFETY: as above.
    let all = unsafe {
        enumerator().and_then(|e| Ok(e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?)).map(|all| (0..all.GetCount().unwrap_or(0)).filter_map(|i| all.Item(i).ok()).map(|d| name_of(&d)).collect()).unwrap_or_default()
    };
    (found, all)
}

/// A device's microphone, played into the virtual device. Dropping it takes it away.
pub struct Microphone {
    packets: Option<mpsc::SyncSender<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl Microphone {
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(50);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        let thread = std::thread::Builder::new().name("microphone".into()).spawn(move || {
            com();
            let (client, render, room, sink) = match open_sink() {
                Ok((client, render, room, name, sink)) => {
                    let _ = ready_tx.send(Ok(name));
                    (client, render, room, sink)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            match play(&client, &render, room, sink, rx) {
                Ok((played, dropped, bad)) => tracing::info!(played, dropped_for_room = dropped, undecodable = bad, "the device's microphone is off the desktop"),
                Err(e) => tracing::warn!("the device's microphone stopped: {e:#}"),
            }
            // SAFETY: a live client.
            let _ = unsafe { client.Stop() };
        })?;
        let name = ready_rx.recv().context("the microphone's thread died")??;
        tracing::info!(into = name, "the device's microphone is on the desktop");
        Ok(Self { packets: Some(tx), thread: Some(thread) })
    }

    /// One Opus packet from the device. Never waits: with half a second of them unplayed the
    /// packet is left out.
    pub fn push(&self, packet: &[u8]) {
        if let Some(tx) = &self.packets {
            let _ = tx.try_send(packet.to_vec());
        }
    }
}

impl Drop for Microphone {
    fn drop(&mut self) {
        self.packets = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How the virtual device is played into: the rate and channels of what is written, and
/// whether the device is held in exclusive mode.
#[derive(Clone, Copy, Debug)]
struct Sink {
    rate: u32,
    channels: u16,
    exclusive: bool,
}

/// The recording side of a virtual microphone named like `name` ("Microphone (Steam Streaming
/// Microphone)" for "Speakers (Steam Streaming Microphone)"): its shared format, as Windows
/// holds it, if there is one.
fn recording_side(name: &str) -> Option<(Vec<u8>, u32, u16)> {
    let inner = name.split_once('(').map(|(_, rest)| rest.trim_end_matches(')').trim().to_lowercase())?;
    // SAFETY: COM calls on this thread's own objects; the format is read within its size and
    // freed.
    unsafe {
        let all = enumerator().ok()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE).ok()?;
        for i in 0..all.GetCount().ok()? {
            let device = all.Item(i).ok()?;
            if !name_of(&device).to_lowercase().contains(&inner) {
                continue;
            }
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).ok()?;
            let format = client.GetMixFormat().ok()?;
            let size = size_of::<WAVEFORMATEX>() + (*format).cbSize as usize;
            let bytes = std::slice::from_raw_parts(format as *const u8, size).to_vec();
            let w = std::ptr::read_unaligned(format);
            let (rate, channels) = (w.nSamplesPerSec, w.nChannels);
            CoTaskMemFree(Some(format as *const _));
            return Some((bytes, rate, channels));
        }
        None
    }
}

fn open_sink() -> Result<(IAudioClient, IAudioRenderClient, u32, String, Sink)> {
    let (device, name) = microphone_sink()?.context("no virtual microphone to play into: install Steam (its \"Steam Streaming Microphone\") or VB-Audio's VB-CABLE")?;
    // SAFETY: COM calls as documented; the formats outlive the calls that read them.
    unsafe {
        // Steam's device copies the samples written to its playing side straight to its
        // recording side, without converting: with the playing side at 48 kHz stereo and the
        // recording side at 44.1 kHz mono (as Windows sets them up), a tone came out at 0.459
        // times its pitch (2026-11-19). So it is played in exclusive mode at the recording
        // side's rate, and the copy is exact.
        if let Some((_, rate, _)) = recording_side(&name) {
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the virtual microphone")?;
            // Valve's device takes 16-bit PCM only in exclusive mode (seen with --probe-mic),
            // and copies what it is given as it is, as 16-bit stereo at the recording side's
            // rate: whatever Windows says of the recording side's channels (mono, in the
            // guest), mono written there came out at twice its pitch with gaps, and stereo
            // came out right (2026-11-19).
            let channels = 2u16;
            let pcm16 = WAVEFORMATEX { wFormatTag: 1, nChannels: channels, nSamplesPerSec: rate, nAvgBytesPerSec: rate * channels as u32 * 2, nBlockAlign: channels * 2, wBitsPerSample: 16, cbSize: 0 };
            let wanted = &pcm16 as *const WAVEFORMATEX;
            let (mut period, mut least) = (0i64, 0i64);
            let _ = client.GetDevicePeriod(Some(&mut period), Some(&mut least));
            let buffer = (period.max(100_000) * 4).max(400_000);
            match client.Initialize(AUDCLNT_SHAREMODE_EXCLUSIVE, 0, buffer, buffer, wanted, None) {
                Ok(()) => {
                    let room = client.GetBufferSize()?;
                    let render: IAudioRenderClient = client.GetService().context("the playing side of the virtual microphone")?;
                    // Silence in the whole buffer first, so it starts with no click.
                    let at = render.GetBuffer(room)?;
                    std::ptr::write_bytes(at, 0, room as usize * channels as usize * 2);
                    render.ReleaseBuffer(room, 0)?;
                    client.Start()?;
                    let sink = Sink { rate, channels, exclusive: true };
                    tracing::info!(?sink, "the virtual microphone is played at its recording side's rate, held by this host");
                    return Ok((client, render, room, name, sink));
                }
                Err(e) => tracing::warn!(rate, "the virtual microphone wouldn't be held at its recording side's rate ({e}); playing in shared mode, where its pitch may be off"),
            }
        }
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the virtual microphone")?;
        let format = float_format(1);
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, STREAM_CONVERT, 1_000_000, 0, &format, None).context("playing into the virtual microphone")?;
        let room = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService().context("the playing side of the virtual microphone")?;
        client.Start()?;
        Ok((client, render, room, name, Sink { rate: SAMPLE_RATE, channels: 1, exclusive: false }))
    }
}

/// From 48 kHz to the sink's rate, by straight lines between samples (enough for a voice),
/// carrying where it is between packets.
struct Resampler {
    step: f64,
    at: f64,
    last: f32,
}

impl Resampler {
    fn new(to: u32) -> Self {
        Self { step: SAMPLE_RATE as f64 / to as f64, at: 0.0, last: 0.0 }
    }

    fn run(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        // `at` counts from the sample before this packet's first (`last`).
        let n = input.len() as f64;
        while self.at < n {
            let i = self.at.floor() as usize;
            let t = (self.at - i as f64) as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input[i];
            out.push(a + (b - a) * t);
            self.at += self.step;
        }
        self.at -= n;
        self.last = *input.last().unwrap_or(&0.0);
    }
}

/// Play packets as they come until the sender is dropped. Says how many frames were played,
/// left out because the device was more than 60 ms behind, and not decodable.
fn play(client: &IAudioClient, render: &IAudioRenderClient, room: u32, sink: Sink, packets: mpsc::Receiver<Vec<u8>>) -> Result<(u64, u64, u64)> {
    let mut decoder = opusic_c::Decoder::new(opusic_c::Channels::Mono, SampleRate::Hz48000).map_err(|e| anyhow!("opus decoder: {e:?}"))?;
    let (mut played, mut dropped, mut bad) = (0u64, 0u64, 0u64);
    let mut frame = [0f32; FRAME_SAMPLES];
    let mut resampler = Resampler::new(sink.rate);
    let mut mono: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 2);
    // What has been decoded and not yet found room for.
    let mut waiting: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
    let ch = sink.channels.max(1) as usize;
    // About 60 ms at the sink's rate.
    let most = sink.rate / 1000 * 60;
    loop {
        let packet = match packets.recv_timeout(Duration::from_millis(5)) {
            Ok(p) => Some(p),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(packet) = packet {
            if decoder.decode_float_to_slice(&packet, &mut frame, false).map_or(true, |n| n != FRAME_SAMPLES) {
                bad += 1;
            } else {
                mono.clear();
                resampler.run(&frame, &mut mono);
                // Behind by more than 60 ms: this one is left out, so the delay can't grow.
                if waiting.len() as u32 > most {
                    dropped += 1;
                } else {
                    waiting.extend(mono.iter().copied());
                    played += 1;
                }
            }
        }
        // SAFETY: the buffer is the render client's between GetBuffer and ReleaseBuffer, and
        // holds room for the frames asked for, `ch` f32 samples each.
        unsafe {
            let queued = client.GetCurrentPadding()?;
            let free = room.saturating_sub(queued);
            if sink.exclusive {
                // What has come, and silence only where the device would otherwise run dry
                // (less than 10 ms left in it): silence written whenever a packet is late
                // would cut holes in the sound.
                let low = sink.rate / 100;
                let n = (waiting.len() as u32).max(low.saturating_sub(queued)).min(free);
                if n == 0 {
                    continue;
                }
                // 16-bit PCM, as the device takes it in exclusive mode.
                let at = render.GetBuffer(n)? as *mut i16;
                for f in 0..n as usize {
                    let v = (waiting.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    for c in 0..ch {
                        *at.add(f * ch + c) = v;
                    }
                }
                render.ReleaseBuffer(n, 0)?;
            } else {
                let n = (free as usize).min(waiting.len());
                if n == 0 {
                    continue;
                }
                let at = render.GetBuffer(n as u32)? as *mut f32;
                for f in 0..n {
                    let v = waiting.pop_front().unwrap_or(0.0);
                    for c in 0..ch {
                        *at.add(f * ch + c) = v;
                    }
                }
                render.ReleaseBuffer(n as u32, 0)?;
            }
        }
    }
    Ok((played, dropped, bad))
}

/// `--probe-mic`: which formats the sides of the virtual microphone take in exclusive mode,
/// and their shared formats, for the log.
pub fn probe_formats() {
    com();
    let pcm = |rate: u32, channels: u16, bits: u16, float: bool| WAVEFORMATEX {
        wFormatTag: if float { FLOAT } else { 1 },
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * channels as u32 * bits as u32 / 8,
        nBlockAlign: channels * bits / 8,
        wBitsPerSample: bits,
        cbSize: 0,
    };
    // SAFETY: COM calls on this thread's own objects; formats live through each call.
    unsafe {
        let Ok(e) = enumerator() else { return };
        for (flow, side) in [(eRender, "playing"), (eCapture, "recording")] {
            let Ok(all) = e.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
            for i in 0..all.GetCount().unwrap_or(0) {
                let Ok(device) = all.Item(i) else { continue };
                let name = name_of(&device);
                if !name.to_lowercase().contains("steam streaming") {
                    continue;
                }
                let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) else { continue };
                let mix = client.GetMixFormat().ok().map(|f| {
                    let w = std::ptr::read_unaligned(f);
                    let (rate, channels, bits, tag) = (w.nSamplesPerSec, w.nChannels, w.wBitsPerSample, w.wFormatTag);
                    let s = format!("{rate} Hz {channels} ch {bits} bit tag {tag:#x}");
                    CoTaskMemFree(Some(f as *const _));
                    s
                });
                let mut taken = Vec::new();
                for rate in [44_100, 48_000, 96_000] {
                    for channels in [1u16, 2] {
                        for (bits, float) in [(16, false), (24, false), (32, false), (32, true)] {
                            let f = pcm(rate, channels, bits, float);
                            if client.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &f, None).is_ok() {
                                taken.push(format!("{rate}/{channels}/{bits}{}", if float { "f" } else { "" }));
                            }
                        }
                    }
                }
                tracing::info!(side, name, shared = ?mix, exclusive = taken.join(" "), "virtual microphone formats");
            }
        }
    }
}
