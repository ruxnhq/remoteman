//! The computer's sound played here: Opus packets decoded with FFmpeg's own decoder (the same
//! library the picture uses, so nothing more is shipped) and written to Windows' default
//! output through WASAPI, which converts to whatever the device runs at.
//!
//! One thread does all of it. It keeps a little sound ahead of the device (60 ms before it
//! starts, so packets that come unevenly don't make holes) and throws the oldest away if more
//! than a quarter of a second has piled up, so the sound never drifts behind the picture.
//! A packet that never came is a short gap: nothing conceals it yet.

use std::collections::VecDeque;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ffmpeg_sys_next as ffi;
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

pub struct Player {
    tx: mpsc::Sender<Vec<u8>>,
}

impl Player {
    /// Open the default output and start playing what is pushed. Fails if there is no output
    /// device or no decoder.
    pub fn start() -> Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
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
        let _ = self.tx.send(packet.data.clone());
    }
}

struct Opus {
    ctx: *mut ffi::AVCodecContext,
    packet: *mut ffi::AVPacket,
    frame: *mut ffi::AVFrame,
}

impl Drop for Opus {
    fn drop(&mut self) {
        // SAFETY: all three are ours; each call takes a null in its stride.
        unsafe {
            ffi::av_frame_free(&mut self.frame);
            ffi::av_packet_free(&mut self.packet);
            ffi::avcodec_free_context(&mut self.ctx);
        }
    }
}

impl Opus {
    fn new() -> Result<Self> {
        // SAFETY: FFmpeg's allocation and set-up calls, each checked; `this` owns what is
        // allocated from the start.
        unsafe {
            let found = ffi::avcodec_find_decoder(ffi::AVCodecID::AV_CODEC_ID_OPUS);
            anyhow::ensure!(!found.is_null(), "this FFmpeg has no Opus decoder");
            let mut this = Self { ctx: ffi::avcodec_alloc_context3(found), packet: ptr::null_mut(), frame: ptr::null_mut() };
            anyhow::ensure!(!this.ctx.is_null(), "out of memory");
            (*this.ctx).sample_rate = RATE as i32;
            ffi::av_channel_layout_default(&mut (*this.ctx).ch_layout, CHANNELS as i32);
            let err = ffi::avcodec_open2(this.ctx, found, ptr::null_mut());
            anyhow::ensure!(err >= 0, "open the Opus decoder ({err})");
            this.packet = ffi::av_packet_alloc();
            this.frame = ffi::av_frame_alloc();
            anyhow::ensure!(!this.packet.is_null() && !this.frame.is_null(), "out of memory");
            Ok(this)
        }
    }

    /// One packet's sound, both channels interleaved, added to `out`.
    fn decode(&mut self, data: &[u8], out: &mut VecDeque<f32>) {
        // SAFETY: the packet is filled with a copy of `data`; the frame's planes are read
        // within the sample count it gives.
        unsafe {
            if ffi::av_new_packet(self.packet, data.len() as i32) < 0 {
                return;
            }
            ptr::copy_nonoverlapping(data.as_ptr(), (*self.packet).data, data.len());
            let sent = ffi::avcodec_send_packet(self.ctx, self.packet);
            ffi::av_packet_unref(self.packet);
            if sent < 0 {
                return;
            }
            while ffi::avcodec_receive_frame(self.ctx, self.frame) >= 0 {
                let f = &*self.frame;
                let n = f.nb_samples as usize;
                let channels = (f.ch_layout.nb_channels as usize).clamp(1, 2);
                if f.format == ffi::AVSampleFormat::AV_SAMPLE_FMT_FLTP as i32 {
                    let left = std::slice::from_raw_parts(f.data[0] as *const f32, n);
                    let right = if channels > 1 { std::slice::from_raw_parts(f.data[1] as *const f32, n) } else { left };
                    for i in 0..n {
                        out.push_back(left[i]);
                        out.push_back(right[i]);
                    }
                } else if f.format == ffi::AVSampleFormat::AV_SAMPLE_FMT_FLT as i32 {
                    let all = std::slice::from_raw_parts(f.data[0] as *const f32, n * channels);
                    for frame in all.chunks_exact(channels) {
                        out.push_back(frame[0]);
                        out.push_back(frame[channels - 1]);
                    }
                }
                ffi::av_frame_unref(self.frame);
            }
        }
    }
}

fn play(rx: mpsc::Receiver<Vec<u8>>, ready: &mpsc::Sender<Result<String>>, packets: &AtomicU32, written: &AtomicU32) -> Result<()> {
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
                    opus.decode(&data, &mut queue);
                    while let Ok(data) = rx.try_recv() {
                        packets.fetch_add(1, Ordering::Relaxed);
                        opus.decode(&data, &mut queue);
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
                say!("sound: {} packets so far, {} frames given to the device, ran dry {underruns} times, {dropped} frames thrown away as late", packets.load(Ordering::Relaxed), written.load(Ordering::Relaxed));
            }
        }
        let _ = client.Stop();
    }
    Ok(())
}
