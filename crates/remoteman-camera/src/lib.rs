//! A connected device's camera as a camera on this desktop.
//!
//! The device (an iPad, an iPhone) sends its camera as H.264 frames; the host gets them from the
//! core (`HostHandler::camera`) and pushes them in here. Each one is decoded in software and
//! shown through:
//!
//! - **a PipeWire camera node**, always: class `Video/Source`, role `Camera`, which is what the
//!   desktop's camera portal hands to programs (GNOME's Camera, Firefox and Chromium with their
//!   PipeWire camera switches on). No root needed.
//! - **a v4l2loopback device**, when one labelled for remoteman exists and can be written to: for
//!   the programs that only read V4L2. See [`loopback`]; that half has never been run.
//!
//! ```no_run
//! let mut camera = remoteman_camera::Camera::start("Anna's iPad camera")?;
//! # let frames: Vec<(Vec<u8>, bool, u64)> = Vec::new();
//! for (data, key, capture_us) in frames {
//!     if camera.push(&data, key, capture_us) == remoteman_camera::Pushed::Lost {
//!         // ask the device for a keyframe (`camera_keyframe`)
//!     }
//! }
//! drop(camera); // the camera is gone from the desktop
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! # One size, for as long as the camera lives
//!
//! The camera has the size it was started with (1280x720 unless [`Options`] say otherwise) and
//! offers it in three pixel layouts: NV12, I420 and YUY2, at the frame rate it was started with.
//! When the device's picture changes size at a keyframe (it was turned on its side), the camera
//! does not: the picture is scaled to fit, keeping its proportions, and centred on black.
//!
//! Renegotiating the stream instead would break the programs this is for. A camera consumer
//! picks one mode when it opens the camera and asks for exactly that size (WebRTC's PipeWire
//! code, so Firefox and Chromium, does): offered another size mid-stream, its link fails rather
//! than adapts. And a v4l2loopback device cannot change format at all while a reader has it open.
//! A webcam never changes size under a program, so neither does this.
//!
//! # Two programs at once
//!
//! Several programs can read the camera together as long as they read the same layout (the
//! second takes what the first negotiated; seen with two readers of I420). A second program
//! that insists on a different layout does not get one, and, as seen here on PipeWire 1.6.11
//! with WirePlumber 0.5.19, the first program's pictures stop too and stay stopped until it
//! opens the camera again. GStreamer's own `pipewiresink mode=provide` camera does exactly the
//! same, so this is PipeWire's handling of one output port with two formats asked of it, not
//! something this crate can negotiate around.
//!
//! # A paused camera
//!
//! While no frame is pushed, the last picture stays: after [`IDLE_INTERVAL`] without a new one it
//! is sent again, that often, so a program reading the camera neither stalls nor decides the
//! camera died, and one that starts reading during a pause gets a picture at once. Before the
//! first frame, the picture is black. (A frozen picture is what a stalled call looks like
//! anyway; to show nothing, drop the `Camera`, as the host does on `camera` off.)
//!
//! # Lost and damaged frames
//!
//! The core already withholds frames after a loss until a keyframe. On top of that, nothing in
//! a frame can make [`Camera::push`] fail loudly: a frame the decoder rejects, or can only
//! decode by patching over holes, is not shown and [`Pushed::Lost`] comes back; frames are then
//! skipped ([`Pushed::Waiting`]) until the next keyframe.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, ensure};

mod decode;
pub mod loopback;
mod picture;
mod pw;
mod source;

pub use source::Source;

/// How long after the last new picture the current one is sent again, and how often from then
/// on: 5 a second.
pub const IDLE_INTERVAL: Duration = Duration::from_millis(200);

/// How a camera is set up. `Options::default()` is the mode every device sends: 1280x720 at 30
/// a second, with the loopback device used if there is one.
#[derive(Debug, Clone)]
pub struct Options {
    /// The camera's size, for as long as it lives (what `camera` on announced). Rounded up to a
    /// multiple of 8 by 2, so that every consumer agrees where a row ends.
    pub width: u32,
    pub height: u32,
    /// Frames a second, as announced to consumers. Frames go out as they are pushed.
    pub fps: u32,
    /// Also write to the v4l2loopback device labelled for remoteman, if there is one.
    pub loopback: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { width: 1280, height: 720, fps: 30, loopback: true }
    }
}

/// What became of a pushed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Pushed {
    /// Decoded and shown.
    Shown,
    /// It could not be decoded and nothing will be until a keyframe: ask the device for one
    /// (`camera_keyframe`). The last good picture stays.
    Lost,
    /// Skipped: still waiting for that keyframe.
    Waiting,
}

/// A camera on this desktop, fed with a device's H.264 frames. Dropping it removes the camera.
pub struct Camera {
    decoder: decode::Decoder,
    output: pw::Output,
    loopback: Option<loopback::Loopback>,
    clock: Clock,
    node_name: String,
    size: (u32, u32),
}

// The host's handler owns the camera on a thread of its own.
const _: () = {
    const fn is_send<T: Send>() {}
    is_send::<Camera>();
};

impl Camera {
    /// A camera called `name` (what people see in a camera picker, e.g. "Anna's iPad camera"),
    /// 1280x720 at 30 a second. Returns once PipeWire has the node.
    pub fn start(name: &str) -> Result<Camera> {
        Self::start_with(name, Options::default())
    }

    /// [`Camera::start`] with another size or frame rate, or without the loopback device.
    pub fn start_with(name: &str, options: Options) -> Result<Camera> {
        ensure!((16..=8192).contains(&options.width) && (16..=8192).contains(&options.height), "a camera of {}x{}", options.width, options.height);
        let (width, height) = (options.width.next_multiple_of(8), options.height.next_multiple_of(2));
        let fps = if options.fps == 0 { 30 } else { options.fps };
        let decoder = decode::Decoder::new(width, height)?;
        let node_name = node_name(name);
        let output = pw::Output::start(&node_name, name, width, height, fps)?;
        // The loopback device is an extra: without it, or if it refuses, the camera still works.
        let loopback = match loopback::find() {
            Some(path) if options.loopback => match loopback::Loopback::open(&path, width, height, fps) {
                Ok(device) => Some(device),
                Err(e) => {
                    eprintln!("camera: no loopback camera: {e:#}");
                    None
                }
            },
            _ => None,
        };
        Ok(Camera { decoder, output, loopback, clock: Clock::default(), node_name, size: (width, height) })
    }

    /// One whole H.264 frame as the core delivers it (Annex-B, parameter sets in front of a
    /// keyframe): decode it and show its picture. `key` is the frame's keyframe mark and
    /// `capture_us` when it was captured, on the sender's clock (it spaces the pictures'
    /// timestamps evenly, whatever the network did to their arrival).
    ///
    /// Takes about as long as decoding does (a millisecond or two at 720p); never panics on the
    /// frame's contents.
    pub fn push(&mut self, data: &[u8], key: bool, capture_us: u64) -> Pushed {
        match self.decoder.decode(data, key) {
            decode::Decoded::Picture(picture) => {
                let picture = Arc::new(picture);
                let taken = self.clock.stamp(capture_us, monotonic_ns());
                if let Some(loopback) = &self.loopback {
                    loopback.show(picture.clone());
                }
                self.output.show(picture, taken);
                Pushed::Shown
            }
            decode::Decoded::Lost => Pushed::Lost,
            decode::Decoded::Waiting => Pushed::Waiting,
        }
    }

    /// The camera's size: every picture consumers get has it.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The PipeWire node's `node.name` (the name given, reduced to letters, digits, `-`, `_` and
    /// `.`); its `node.description` is the name as given.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// The PipeWire node's id.
    pub fn node_id(&self) -> u32 {
        self.output.node_id()
    }

    /// The loopback device being written to, if any.
    pub fn loopback_device(&self) -> Option<&Path> {
        self.loopback.as_ref().map(|l| l.path())
    }
}

/// A `node.name` from a camera's name: runs of anything but ASCII letters, digits, `_` and `.`
/// become one `-`.
fn node_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "remoteman-camera".to_owned() } else { out.to_owned() }
}

/// Now, on the clock PipeWire stamps buffers with, in nanoseconds.
pub(crate) fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid timespec to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// Puts the sender's capture times on this machine's clock: pictures keep the spacing they were
/// captured with, as long as that stays within [`Clock::SLACK`] of when they arrived and never
/// ahead of it.
#[derive(Default)]
struct Clock {
    /// A capture time (µs, the sender's clock) and the local time (ns) it stands for.
    anchor: Option<(u64, i64)>,
}

impl Clock {
    /// How far behind its arrival a picture's timestamp may be.
    const SLACK: i64 = 100_000_000;

    fn stamp(&mut self, capture_us: u64, now: i64) -> i64 {
        if let Some((capture, local)) = self.anchor {
            let stamp = capture_us.checked_sub(capture).and_then(|us| i64::try_from(us).ok()).and_then(|us| us.checked_mul(1000)).and_then(|ns| local.checked_add(ns));
            if let Some(stamp) = stamp {
                if stamp <= now && now - stamp <= Self::SLACK {
                    return stamp;
                }
            }
        }
        // The first picture, one that came sooner than any before it, or a jump (a pause, a new
        // sender clock): count from here.
        self.anchor = Some((capture_us, now));
        now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_names() {
        assert_eq!(node_name("Anna's iPad camera"), "Anna-s-iPad-camera");
        assert_eq!(node_name("remoteman-test-camera-42"), "remoteman-test-camera-42");
        assert_eq!(node_name("  Zoë’s  phone!  "), "Zo-s-phone");
        assert_eq!(node_name("📷"), "remoteman-camera");
    }

    #[test]
    fn timestamps_follow_capture_times_within_slack() {
        const MS: i64 = 1_000_000;
        let mut clock = Clock::default();
        // The first picture is stamped on arrival.
        assert_eq!(clock.stamp(5_000_000, 1000 * MS), 1000 * MS);
        // Captured 33 ms later, arrived 50 ms later: stamped 33 ms later.
        assert_eq!(clock.stamp(5_033_000, 1050 * MS), 1033 * MS);
        // Arrived sooner than its capture time says it could: never stamped in the future.
        assert_eq!(clock.stamp(5_066_000, 1060 * MS), 1060 * MS);
        assert_eq!(clock.stamp(5_100_000, 1100 * MS), 1094 * MS);
        // A pause (far more than the slack behind): from here again.
        assert_eq!(clock.stamp(5_133_000, 3000 * MS), 3000 * MS);
        // The sender's clock started over.
        assert_eq!(clock.stamp(10_000, 3033 * MS), 3033 * MS);
        assert_eq!(clock.stamp(43_000, 3070 * MS), 3066 * MS);
        // Absurd values do no harm.
        assert_eq!(clock.stamp(u64::MAX, 3100 * MS), 3100 * MS);
    }
}
