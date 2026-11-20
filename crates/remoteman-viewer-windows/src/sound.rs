//! The computer's sound played here: Opus packets decoded with libopus (the copy the microphone
//! already links) and written to Windows' default output through WASAPI, which converts to
//! whatever the device runs at.
//!
//! One thread does all of it. It keeps a little sound ahead of the device (60 ms before it
//! starts, so packets that come unevenly don't make holes) and throws the oldest away if more
//! than a quarter of a second has piled up, so the sound never drifts behind the picture.
//! A packet that never came is told by its sequence number and made up with Opus's loss
//! concealment, or from the copy of it the next packet carries if the host put one there
//! (in-band FEC). At most 100 ms is made up in one hole: beyond that, or at a packet the host
//! marks as a start (after a silence), the decoder starts afresh instead. A duplicate or a
//! packet that came late is dropped.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use remoteman_core::media::Packet;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};

/// What the hosts send: 48 kHz, two channels.
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
/// Samples (of both channels together) to have before the device is given any.
const START: usize = RATE as usize * CHANNELS * 60 / 1000;
/// More than this waiting is sound that is late: the oldest goes.
const MOST: usize = RATE as usize * CHANNELS / 4;

/// A packet as the sound thread is given it.
struct Item {
    seq: u32,
    start: bool,
    data: Vec<u8>,
}

pub struct Player {
    tx: mpsc::Sender<Item>,
}

impl Player {
    /// Open the default output and start playing what is pushed. Fails if there is no output
    /// device or no decoder.
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Item>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<String>>();
        // Packets taken and frames given to the device, for the log.
        let (p, w) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        std::thread::Builder::new().name("sound".into()).spawn(move || {
            if let Err(e) = play(rx, &ready_tx, &p, &w) {
                // Before it was ready: the caller hears of it. After: only the log does.
                if ready_tx.send(Err(anyhow::anyhow!("{e:#}"))).is_err() {
                    say!("sound: stopped: {e:#}");
                }
            }
        })?;
        let device = ready_rx.recv_timeout(Duration::from_secs(5)).context("the sound device didn't answer")??;
        say!("sound: playing through {device}");
        Ok(Self { tx })
    }

    pub fn push(&self, packet: &Packet) {
        let _ = self.tx.send(Item { seq: packet.seq, start: packet.start, data: packet.data.clone() });
    }
}

/// What the sequence numbers say about a packet that has just come, given the one before it.
#[derive(Debug, PartialEq, Eq)]
enum Gap {
    /// The next one, or the first of a stream: just decode it.
    None,
    /// This many packets (at least one) never came: make them up, then decode this one.
    Conceal(u32),
    /// A new run of sound, or a hole too large to be worth filling: forget what went before
    /// and decode this one from nothing.
    Resync,
    /// A duplicate or one that came after a later one: it is not played.
    Stale,
}

/// The most that is made up for a hole: 100 ms. More than that is not a lost packet but a
/// pause, and made-up sound would only drone.
const MOST_CONCEALED_US: u64 = 100_000;

/// Compare the number of a packet that has come with the one before it (`None` before the
/// first). `start` is the host's mark on the first packet after a silence or a restart, where
/// the numbers say nothing about what is missing. `frame_us` is how long a packet is.
fn gap(last: Option<u32>, seq: u32, start: bool, frame_us: u64) -> Gap {
    let Some(last) = last else { return Gap::None };
    if start {
        return Gap::Resync;
    }
    // Wrap-around safe: the distance, read as signed.
    let ahead = seq.wrapping_sub(last) as i32;
    match ahead {
        1 => Gap::None,
        i32::MIN..=0 => Gap::Stale,
        _ => {
            let missing = ahead as u32 - 1;
            if u64::from(missing) * frame_us.max(1) <= MOST_CONCEALED_US { Gap::Conceal(missing) } else { Gap::Resync }
        }
    }
}

/// The longest packet Opus makes: 120 ms.
const MAX_FRAME: usize = RATE as usize * 120 / 1000;

/// libopus's decoder, which conceals what never came (FFmpeg's does not).
struct Opus {
    decoder: opusic_c::Decoder,
    /// The number of the last packet that was taken, and how many samples of each channel it held.
    last: Option<u32>,
    frame: usize,
    scratch: Vec<f32>,
    concealed: u64,
}

impl Opus {
    fn new() -> Result<Self> {
        let decoder = opusic_c::Decoder::new(opusic_c::Channels::Stereo, opusic_c::SampleRate::Hz48000).map_err(|e| anyhow::anyhow!("open the Opus decoder ({e:?})"))?;
        Ok(Self { decoder, last: None, frame: RATE as usize / 100, scratch: vec![0.0; MAX_FRAME * CHANNELS], concealed: 0 })
    }

    /// One packet's sound, both channels interleaved, added to `out`, with whatever packets
    /// went missing before it made up first.
    fn take(&mut self, packet: &Item, out: &mut VecDeque<f32>) {
        let frame_us = (self.frame as u64 * 1_000_000 / RATE as u64).max(1);
        let missing = match gap(self.last, packet.seq, packet.start, frame_us) {
            Gap::Stale => return,
            Gap::None => 0,
            Gap::Conceal(n) => n,
            Gap::Resync => {
                let _ = self.decoder.reset();
                0
            }
        };
        for i in 0..missing {
            // The last one missing may be in this packet's in-band copy (if the host put one
            // there); the others, and that one without it, are guessed from the sound before.
            let fec = i + 1 == missing && !packet.data.is_empty();
            let samples = self.frame * CHANNELS;
            let made = if fec { self.decoder.decode_float_to_slice(&packet.data, &mut self.scratch[..samples], true) } else { self.decoder.decode_float_to_slice(&[], &mut self.scratch[..samples], false) };
            let made = match made {
                Ok(n) => n,
                // The copy isn't there (or this packet is not one): guess instead.
                Err(_) if fec => self.decoder.decode_float_to_slice(&[], &mut self.scratch[..samples], false).unwrap_or(0),
                Err(_) => 0,
            };
            out.extend(self.scratch[..made * CHANNELS].iter().copied());
            self.concealed += 1;
        }
        self.last = Some(packet.seq);
        if packet.data.is_empty() {
            return;
        }
        if let Ok(n) = self.decoder.decode_float_to_slice(&packet.data, &mut self.scratch, false) {
            if n > 0 {
                self.frame = n;
            }
            out.extend(self.scratch[..n * CHANNELS].iter().copied());
        }
    }
}

fn play(rx: mpsc::Receiver<Item>, ready: &mpsc::Sender<Result<String>>, packets: &AtomicU32, written: &AtomicU32) -> Result<()> {
    let mut opus = Opus::new()?;
    // SAFETY: COM calls on this thread, which initialises COM for itself; every pointer handed
    // over lives through its call, and the render buffer is written within the frames asked for.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("no audio devices")?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).context("no sound output on this PC")?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).context("open the sound output")?;
        let format = WAVEFORMATEX {
            // IEEE float.
            wFormatTag: 3,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 4,
            nBlockAlign: CHANNELS as u16 * 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        // A tenth of a second of buffer (in units of 100 ns); Windows converts to the device's
        // own rate and layout.
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, 1_000_000, 0, &format, None).context("set the sound output up")?;
        let size = client.GetBufferSize()?;
        let render: IAudioRenderClient = client.GetService()?;
        client.Start()?;
        let _ = ready.send(Ok("the default output, 48 kHz stereo".into()));

        let mut queue: VecDeque<f32> = VecDeque::new();
        let (mut playing, mut underruns, mut dropped) = (false, 0u32, 0u64);
        let mut logged = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_millis(5)) {
                Ok(data) => {
                    packets.fetch_add(1, Ordering::Relaxed);
                    opus.take(&data, &mut queue);
                    while let Ok(data) = rx.try_recv() {
                        packets.fetch_add(1, Ordering::Relaxed);
                        opus.take(&data, &mut queue);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if queue.len() > MOST {
                let over = queue.len() - START;
                queue.drain(..over);
                dropped += (over / CHANNELS) as u64;
            }
            if !playing && queue.len() >= START {
                playing = true;
            }
            if playing {
                let padding = client.GetCurrentPadding()?;
                let frames = (size - padding).min((queue.len() / CHANNELS) as u32);
                if frames > 0 {
                    let buffer = render.GetBuffer(frames)?;
                    let out = std::slice::from_raw_parts_mut(buffer as *mut f32, frames as usize * CHANNELS);
                    for (to, from) in out.iter_mut().zip(queue.drain(..frames as usize * CHANNELS)) {
                        *to = from;
                    }
                    render.ReleaseBuffer(frames, 0)?;
                    written.fetch_add(frames, Ordering::Relaxed);
                } else if queue.is_empty() && padding == 0 {
                    // Ran dry: the host stops sending while there is only silence.
                    playing = false;
                    underruns += 1;
                }
            }
            if logged.elapsed() >= Duration::from_secs(10) {
                logged = Instant::now();
                say!("sound: {} packets so far, {} frames given to the device, ran dry {underruns} times, {dropped} frames thrown away as late, {} made up for lost packets", opus.concealed, packets.load(Ordering::Relaxed), written.load(Ordering::Relaxed));
            }
        }
        let _ = client.Stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: u64 = 10_000;

    #[test]
    fn the_next_packet_leaves_no_gap() {
        assert_eq!(gap(None, 500, false, F), Gap::None);
        assert_eq!(gap(None, 500, true, F), Gap::None);
        assert_eq!(gap(Some(7), 8, false, F), Gap::None);
    }

    #[test]
    fn missing_packets_are_counted() {
        assert_eq!(gap(Some(7), 9, false, F), Gap::Conceal(1));
        assert_eq!(gap(Some(7), 12, false, F), Gap::Conceal(4));
        // Exactly 100 ms is still made up; one more is not.
        assert_eq!(gap(Some(0), 10, false, F), Gap::Conceal(9));
        assert_eq!(gap(Some(0), 11, false, F), Gap::Conceal(10));
        assert_eq!(gap(Some(0), 12, false, F), Gap::Resync);
    }

    #[test]
    fn a_huge_jump_resyncs() {
        assert_eq!(gap(Some(7), 5_000, false, F), Gap::Resync);
        assert_eq!(gap(Some(7), 7 + 0x7fff_ffff, false, F), Gap::Resync);
    }

    #[test]
    fn the_numbers_wrap() {
        assert_eq!(gap(Some(u32::MAX), 0, false, F), Gap::None);
        assert_eq!(gap(Some(u32::MAX), 1, false, F), Gap::Conceal(1));
        assert_eq!(gap(Some(u32::MAX - 1), 2, false, F), Gap::Conceal(3));
    }

    #[test]
    fn a_start_resyncs_without_concealing() {
        assert_eq!(gap(Some(7), 9, true, F), Gap::Resync);
        assert_eq!(gap(Some(7), 8, true, F), Gap::Resync);
        assert_eq!(gap(Some(7), 3, true, F), Gap::Resync);
    }

    #[test]
    fn duplicates_and_late_packets_are_stale() {
        assert_eq!(gap(Some(7), 7, false, F), Gap::Stale);
        assert_eq!(gap(Some(7), 6, false, F), Gap::Stale);
        assert_eq!(gap(Some(0), u32::MAX, false, F), Gap::Stale);
    }

    #[test]
    fn longer_frames_allow_fewer() {
        // 20 ms packets: five are 100 ms.
        assert_eq!(gap(Some(0), 6, false, 20_000), Gap::Conceal(5));
        assert_eq!(gap(Some(0), 7, false, 20_000), Gap::Resync);
    }

    /// A synthetic tone through the real encoder, one packet in ten lost: what comes out is as
    /// long as what went in, the made-up frames carry on with the tone, and a duplicate adds nothing.
    #[test]
    fn lost_packets_are_made_up_with_the_tone() {
        let mut enc = opusic_c::Encoder::new(opusic_c::Channels::Stereo, opusic_c::SampleRate::Hz48000, opusic_c::Application::LowDelay).unwrap();
        enc.set_inband_fec(opusic_c::InbandFec::Mode1).unwrap();
        enc.set_packet_loss(10).unwrap();
        let packets: Vec<Vec<u8>> = (0..100)
            .map(|p| {
                let frame: Vec<f32> = (0..480)
                    .flat_map(|i| {
                        let v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * (p * 480 + i) as f32 / 48_000.0).sin();
                        [v, v]
                    })
                    .collect();
                let mut out = [0u8; 1275];
                let n = enc.encode_float_to_slice(&frame, &mut out).unwrap();
                out[..n].to_vec()
            })
            .collect();
        let mut opus = Opus::new().unwrap();
        let mut out = VecDeque::new();
        // The first sequence number is arbitrary, and the run crosses the wrap.
        let base = u32::MAX - 40;
        for (p, data) in packets.iter().enumerate() {
            if p % 10 == 5 {
                continue;
            }
            opus.take(&Item { seq: base.wrapping_add(p as u32), start: p == 0, data: data.clone() }, &mut out);
            if p == 20 {
                let before = out.len();
                opus.take(&Item { seq: base.wrapping_add(20), start: false, data: data.clone() }, &mut out);
                assert_eq!(out.len(), before, "a duplicate is played");
            }
        }
        assert_eq!(out.len(), 100 * 480 * 2);
        assert_eq!(opus.concealed, 10);
        // The frame made up for packet 55 is not silent and no louder than the tone.
        let v: Vec<f32> = out.iter().copied().collect();
        let made = &v[55 * 960..56 * 960];
        let peak = made.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.1..0.8).contains(&peak), "made-up frame peaks at {peak}");
        assert!(v.iter().all(|s| s.abs() < 1.0));
    }

    #[test]
    fn a_long_hole_starts_afresh() {
        let mut enc = opusic_c::Encoder::new(opusic_c::Channels::Stereo, opusic_c::SampleRate::Hz48000, opusic_c::Application::LowDelay).unwrap();
        let frame = vec![0.1f32; 960];
        let mut buf = [0u8; 1275];
        let n = enc.encode_float_to_slice(&frame, &mut buf).unwrap();
        let data = buf[..n].to_vec();
        let mut opus = Opus::new().unwrap();
        let mut out = VecDeque::new();
        opus.take(&Item { seq: 1, start: true, data: data.clone() }, &mut out);
        opus.take(&Item { seq: 1000, start: false, data }, &mut out);
        assert_eq!(opus.concealed, 0);
        assert_eq!(out.len(), 2 * 480 * 2);
    }
}
