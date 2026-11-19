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
//! Not done: following the default output when the user changes it (the sound then stops
//! until the device switches sound off and on), and making "Steam Streaming Speakers" the
//! output so that the PC itself stays quiet.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use opusic_c::{Application, Bitrate, InbandFec, SampleRate};
use remoteman_core::host::Link;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STGM_READ};
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
            let (client, capture) = match opened {
                Ok((client, capture, name)) => {
                    let _ = ready_tx.send(Ok(name));
                    (client, capture)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            match record(&capture, &link, &s) {
                Ok((packets, bytes, pauses)) => tracing::info!(packets, kilobytes = bytes / 1000, pauses, "the desktop's sound no longer goes to the device"),
                Err(e) => tracing::warn!("the desktop's sound stopped: {e:#}"),
            }
            // SAFETY: a live client.
            let _ = unsafe { client.Stop() };
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

fn open_loopback() -> Result<(IAudioClient, IAudioCaptureClient, String)> {
    // SAFETY: COM calls as documented, on this thread's own objects; the format outlives the
    // call that reads it.
    unsafe {
        let device = enumerator()?.GetDefaultAudioEndpoint(eRender, eConsole).context("this computer has no sound output to record from")?;
        let name = name_of(&device);
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the sound output")?;
        let format = float_format(2);
        // A fifth of a second of room; it is emptied every few milliseconds.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, STREAM_LOOPBACK | STREAM_CONVERT, 2_000_000, 0, &format, None).context("recording the sound output back")?;
        let capture: IAudioCaptureClient = client.GetService().context("the recording side of the sound output")?;
        client.Start().context("starting to record the sound output")?;
        Ok((client, capture, name))
    }
}

/// Record until told to stop. Says how many packets went, their bytes, and how many pauses.
fn record(capture: &IAudioCaptureClient, link: &Link, stop: &AtomicBool) -> Result<(u64, u64, u32)> {
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
            if MICROPHONE_SINKS.iter().any(|s| name.to_lowercase().contains(s)) {
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
            let (client, render, room) = match open_sink() {
                Ok((client, render, room, name)) => {
                    let _ = ready_tx.send(Ok(name));
                    (client, render, room)
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            match play(&client, &render, room, rx) {
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

fn open_sink() -> Result<(IAudioClient, IAudioRenderClient, u32, String)> {
    let (device, name) = microphone_sink()?.context("no virtual microphone to play into: install Steam (its \"Steam Streaming Microphone\") or VB-Audio's VB-CABLE")?;
    // SAFETY: COM calls as documented; the format outlives the call that reads it.
    unsafe {
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("opening the virtual microphone")?;
        let format = float_format(1);
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, STREAM_CONVERT, 1_000_000, 0, &format, None).context("playing into the virtual microphone")?;
        let room = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService().context("the playing side of the virtual microphone")?;
        client.Start()?;
        Ok((client, render, room, name))
    }
}

/// Play packets as they come until the sender is dropped. Says how many frames were played,
/// left out because the device was more than 60 ms behind, and not decodable.
fn play(client: &IAudioClient, render: &IAudioRenderClient, room: u32, packets: mpsc::Receiver<Vec<u8>>) -> Result<(u64, u64, u64)> {
    let mut decoder = opusic_c::Decoder::new(opusic_c::Channels::Mono, SampleRate::Hz48000).map_err(|e| anyhow!("opus decoder: {e:?}"))?;
    let (mut played, mut dropped, mut bad) = (0u64, 0u64, 0u64);
    let mut frame = [0f32; FRAME_SAMPLES];
    while let Ok(packet) = packets.recv() {
        if decoder.decode_float_to_slice(&packet, &mut frame, false).map_or(true, |n| n != FRAME_SAMPLES) {
            bad += 1;
            continue;
        }
        // SAFETY: the buffer is the render client's between GetBuffer and ReleaseBuffer and
        // holds room for the frames asked for, in mono f32.
        unsafe {
            let queued = client.GetCurrentPadding()?;
            // Behind by more than six frames: this one is left out, so the delay can't grow.
            if queued > 6 * FRAME_SAMPLES as u32 || room - queued < FRAME_SAMPLES as u32 {
                dropped += 1;
                continue;
            }
            let at = render.GetBuffer(FRAME_SAMPLES as u32)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), at as *mut f32, FRAME_SAMPLES);
            render.ReleaseBuffer(FRAME_SAMPLES as u32, 0)?;
        }
        played += 1;
    }
    Ok((played, dropped, bad))
}
