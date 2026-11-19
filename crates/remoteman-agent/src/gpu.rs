//! Which GPU encodes, and the two halves of the picture path for it: NVENC (fed by the Vulkan
//! converter) on NVIDIA, VA-API on AMD and Intel. The GPU is the one the session's compositor
//! renders on, found as Mutter finds its primary GPU: a render node tagged
//! `mutter-device-preferred-primary` in udev, else the one the firmware booted with (boot_vga),
//! else the first. `REMOTEMAN_ENCODER=nvenc`, `vaapi` or `vaapi:/dev/dri/renderDN` overrides it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use remoteman_core::{self as control, Mode};
use remoteman_encode::vaapi::{self, Caps};
use remoteman_encode::{Codec, Config, Encoder, How, Made};
use remoteman_gpu::{Chroma, Converter};

#[derive(Debug)]
pub enum Backend {
    Nvenc,
    Vaapi { node: PathBuf, caps: Caps },
}

/// The encoder this host uses, chosen once.
pub fn backend() -> &'static Backend {
    static CHOSEN: OnceLock<Backend> = OnceLock::new();
    CHOSEN.get_or_init(|| {
        let chosen = choose();
        tracing::info!(?chosen, "the encoder");
        chosen
    })
}

fn choose() -> Backend {
    let vaapi_on = |node: PathBuf| match vaapi::probe(&node) {
        Some(caps) if caps.convert && (caps.hevc8 || caps.hevc10 || caps.av1_8 || caps.av1_10) => Some(Backend::Vaapi { node, caps }),
        Some(caps) => {
            tracing::warn!(node = %node.display(), ?caps, "this GPU encodes nothing remoteman sends through VA-API");
            None
        }
        None => None,
    };
    match std::env::var("REMOTEMAN_ENCODER").ok().as_deref() {
        Some("nvenc") => return Backend::Nvenc,
        Some(v) if v.starts_with("vaapi") => {
            let nodes = match v.strip_prefix("vaapi:") {
                Some(node) => vec![PathBuf::from(node)],
                None => vaapi::render_nodes().into_iter().filter(|n| vendor(n) != Some(0x10de)).collect(),
            };
            if let Some(b) = nodes.into_iter().find_map(vaapi_on) {
                return b;
            }
            tracing::warn!("REMOTEMAN_ENCODER={v}, but no GPU here encodes through VA-API");
        }
        Some(v) => tracing::warn!("REMOTEMAN_ENCODER={v} is not nvenc, vaapi or vaapi:NODE; ignored"),
        None => {}
    }
    let Some(node) = primary() else { return Backend::Nvenc };
    if vendor(&node) == Some(0x10de) {
        return Backend::Nvenc;
    }
    vaapi_on(node).unwrap_or(Backend::Nvenc)
}

/// The sysfs directory of a render node's device.
fn device(node: &Path) -> PathBuf {
    Path::new("/sys/class/drm").join(node.file_name().unwrap_or_default()).join("device")
}

fn vendor(node: &Path) -> Option<u32> {
    let v = std::fs::read_to_string(device(node).join("vendor")).ok()?;
    u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok()
}

/// Whether udev tagged this node, or the card beside it, for Mutter to prefer.
fn preferred(node: &Path) -> bool {
    let mut ids = Vec::new();
    let mut add = |dev: PathBuf| {
        if let Ok(s) = std::fs::read_to_string(dev.join("dev")) {
            ids.push(format!("c{}", s.trim()));
        }
    };
    add(Path::new("/sys/class/drm").join(node.file_name().unwrap_or_default()));
    for card in std::fs::read_dir(device(node).join("drm")).into_iter().flatten().flatten() {
        if card.file_name().to_string_lossy().starts_with("card") {
            add(card.path());
        }
    }
    ids.iter().any(|id| std::fs::read_to_string(Path::new("/run/udev/data").join(id)).is_ok_and(|d| d.lines().any(|l| (l.starts_with("G:") || l.starts_with("Q:")) && l[2..] == *"mutter-device-preferred-primary")))
}

/// The GPU Mutter renders on, by Mutter's own rule.
fn primary() -> Option<PathBuf> {
    let nodes = vaapi::render_nodes();
    let boot_vga = |n: &PathBuf| std::fs::read_to_string(device(n).join("boot_vga")).is_ok_and(|s| s.trim() == "1");
    nodes.iter().find(|n| preferred(n)).or_else(|| nodes.iter().find(|n| boot_vga(n))).or(nodes.first()).cloned()
}

/// The modes this host's encoder can send, best first.
pub fn modes() -> Vec<Mode> {
    let m = |codec, chroma, depth| Mode { codec, chroma, depth };
    use control::{Chroma::*, Codec::*};
    match backend() {
        Backend::Nvenc => vec![m(Av1, C420, 10), m(Hevc, C444, 10), m(Hevc, C420, 10)],
        Backend::Vaapi { caps, .. } => [m(Av1, C420, 10), m(Hevc, C420, 10), m(Av1, C420, 8), m(Hevc, C420, 8)]
            .into_iter()
            .filter(|m| caps.encodes(if m.codec == Av1 { Codec::Av1 } else { Codec::Hevc }, m.depth))
            .collect(),
    }
}

/// The capture thread's half: the compositor's buffer into the encoder's picture.
pub enum Conv {
    Nv(Converter),
    Va(vaapi::Converter),
}

impl Conv {
    pub fn size(&self) -> (u32, u32) {
        match self {
            Conv::Nv(c) => (c.width, c.height),
            Conv::Va(c) => (c.width, c.height),
        }
    }

    pub fn reset_images(&mut self) {
        match self {
            Conv::Nv(c) => c.reset_images(),
            Conv::Va(c) => c.reset_images(),
        }
    }

    pub fn convert(&mut self, fd: i32, modifier: u64, offset: u32, stride: u32, slot: usize) -> Result<()> {
        match self {
            Conv::Nv(c) => c.convert(fd, modifier, offset, stride, slot),
            Conv::Va(c) => c.convert(fd, modifier, offset, stride, slot),
        }
    }
}

/// What the encoder thread opens its encoder from.
pub enum Opener {
    Nv { inputs: Vec<(i32, u64)>, pitch: u32 },
    Va(vaapi::Encoder),
}

impl Opener {
    pub fn open(self, cfg: &Config) -> Result<Enc> {
        match self {
            Opener::Nv { inputs, pitch } => {
                let mut enc = Encoder::new(cfg)?;
                for (fd, size) in inputs {
                    enc.add_input(fd, size, pitch)?;
                }
                Ok(Enc::Nv(enc))
            }
            Opener::Va(e) => Ok(Enc::Va(e)),
        }
    }
}

/// The encoder thread's half.
pub enum Enc {
    Nv(Encoder),
    Va(vaapi::Encoder),
}

impl Enc {
    pub fn note(&self) -> &str {
        match self {
            Enc::Nv(e) => &e.note,
            Enc::Va(e) => &e.note,
        }
    }

    /// Recent frames a loss can be answered from (none with VA-API: every loss is a keyframe).
    pub fn short_term(&self) -> u32 {
        match self {
            Enc::Nv(e) => e.short_term(),
            Enc::Va(_) => 0,
        }
    }

    pub fn long_term(&self) -> u32 {
        match self {
            Enc::Nv(e) => e.long_term(),
            Enc::Va(_) => 0,
        }
    }

    /// With VA-API the encoder is opened afresh, so the next frame is a keyframe.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        match self {
            Enc::Nv(e) => e.set_bitrate(bitrate),
            Enc::Va(e) => e.set_bitrate(bitrate),
        }
    }

    pub fn invalidate(&mut self, timestamp: u64) -> Result<()> {
        match self {
            Enc::Nv(e) => e.invalidate(timestamp),
            Enc::Va(_) => bail!("VA-API can't drop references"),
        }
    }

    pub fn encode_how(&mut self, slot: usize, timestamp: u64, how: How) -> Result<Made<'_>> {
        match self {
            Enc::Nv(e) => e.encode_how(slot, timestamp, how),
            Enc::Va(e) => {
                let (data, key) = e.encode(slot, timestamp, how.idr)?;
                Ok(Made { data, key, marked: None, referred: 0 })
            }
        }
    }
}

/// How long a step took, frame by frame, in tenths of a millisecond (up to 100 ms), for the
/// log at the end of a picture.
pub struct Times {
    tenths: Vec<u32>,
    n: u32,
}

impl Times {
    pub fn new() -> Self {
        Self { tenths: vec![0; 1001], n: 0 }
    }

    pub fn add(&mut self, since: std::time::Instant) {
        let t = (since.elapsed().as_secs_f64() * 10_000.0) as usize;
        self.tenths[t.min(1000)] += 1;
        self.n += 1;
    }

    /// The median, 99th percentile and the longest, in milliseconds, as text.
    pub fn summary(&self) -> String {
        if self.n == 0 {
            return "none".into();
        }
        let at = |q: f64| {
            let want = ((self.n - 1) as f64 * q) as u32;
            let mut seen = 0;
            for (i, c) in self.tenths.iter().enumerate() {
                seen += c;
                if seen > want {
                    return i as f64 / 10.0;
                }
            }
            100.0
        };
        format!("median {:.1} p99 {:.1} max {:.1} ms over {}", at(0.5), at(0.99), at(1.0), self.n)
    }
}

/// Set up the picture path for this host's GPU: the converter, the DMA-BUF layouts to offer
/// the compositor, and what the encoder thread opens its encoder from.
pub fn open(width: u32, height: u32, mode: Mode, fps: u32, bitrate: u32, slots: usize) -> Result<(Conv, Vec<u64>, Opener)> {
    let codec = match mode.codec {
        control::Codec::Hevc => Codec::Hevc,
        control::Codec::Av1 => Codec::Av1,
    };
    let (conv, modifiers, opener) = match backend() {
        Backend::Nvenc => {
            anyhow::ensure!(mode.depth == 10, "only 10-bit is wired up");
            let chroma = match mode.chroma {
                control::Chroma::C420 => Chroma::C420,
                control::Chroma::C444 => Chroma::C444,
            };
            let mut conv = Converter::new(width, height, chroma, slots).context("Vulkan converter")?;
            let modifiers = conv.modifiers()?;
            let pitch = conv.pitch;
            let inputs: Vec<(i32, u64)> = conv.slots.iter_mut().map(|s| (std::mem::replace(&mut s.fd, -1), s.size)).collect();
            (Conv::Nv(conv), modifiers, Opener::Nv { inputs, pitch })
        }
        Backend::Vaapi { node, caps } => {
            anyhow::ensure!(mode.chroma == control::Chroma::C420 && caps.encodes(codec, mode.depth), "{} doesn't encode {mode:?}", node.display());
            let modifiers = remoteman_gpu::modifiers_on(node)?;
            let (conv, enc) = vaapi::open(node, &vaapi::Config { codec, depth: mode.depth, width, height, fps, bitrate, slots })?;
            (Conv::Va(conv), modifiers, Opener::Va(enc))
        }
    };
    anyhow::ensure!(!modifiers.is_empty(), "this GPU offers no DMA-BUF layout for the compositor's format");
    Ok((conv, modifiers, opener))
}
