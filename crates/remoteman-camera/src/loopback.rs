//! The second output: a v4l2loopback device, for the programs that only read V4L2 (Zoom, and
//! browsers without their PipeWire camera switch).
//!
//! UNTESTED: written against the V4L2 and v4l2loopback (0.15) interfaces, never run. The module
//! needs root to load and the machine this was written on had it unloaded. To try it:
//! `sudo modprobe v4l2loopback card_label="remoteman camera" exclusive_caps=1`, then run the
//! `play` example of this crate and read the device (`ffplay /dev/videoN`, a browser, Zoom).
//!
//! Only a device made for remoteman is ever opened: its card label must contain "remoteman" (as
//! `/sys/class/video4linux/*/name` gives it), and once open it must say itself that it is a
//! v4l2loopback device with that label. A real camera is never opened, let alone written to.
//!
//! The device is given whole YUYV pictures with `write`, one call each, from a thread of its own
//! (so a stuck reader cannot hold up the PipeWire camera). On drop the thread stops the stream
//! and closes the device. If the process is killed instead, the kernel closes the device for it;
//! what cannot be undone from here is a format that readers still holding the device have pinned.

use std::fs::File;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail, ensure};

use crate::IDLE_INTERVAL;
use crate::picture::{Layout, Picture};

/// The label a loopback device must carry (anywhere in its card label, in any case).
pub const LABEL: &str = "remoteman";

/// The loopback device set up for remoteman, if there is one: the first `/dev/videoN` whose card
/// label contains "remoteman". Looks at names only; opens nothing.
pub fn find() -> Option<PathBuf> {
    find_in(Path::new("/sys/class/video4linux"), Path::new("/dev"))
}

/// [`find`], with the two directories given (for tests).
pub fn find_in(sys: &Path, dev: &Path) -> Option<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(sys).ok()?.flatten() {
        let name = entry.file_name();
        let Some(number) = name.to_str().and_then(|n| n.strip_prefix("video")).and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(label) = std::fs::read_to_string(entry.path().join("name")) else { continue };
        if label.to_lowercase().contains(LABEL) {
            found.push((number, dev.join(name)));
        }
    }
    found.sort();
    found.into_iter().next().map(|(_, path)| path)
}

// The kernel's structures (`linux/videodev2.h`), as far as they are used here. Sizes and request
// numbers were checked against the headers of Linux 7.3 on x86-64.

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

/// `struct v4l2_format`: the type, then a union (aligned to 8 bytes, 200 long) of which only the
/// single-plane picture format is used.
#[repr(C)]
struct Format {
    kind: u32,
    _align: u32,
    pix: PixFormat,
    _rest: [u8; 152],
}

/// `struct v4l2_streamparm` with `struct v4l2_outputparm` inside.
#[repr(C)]
struct StreamParm {
    kind: u32,
    capability: u32,
    outputmode: u32,
    numerator: u32,
    denominator: u32,
    extendedmode: u32,
    writebuffers: u32,
    reserved: [u32; 4],
    _rest: [u8; 160],
}

const _: () = assert!(std::mem::size_of::<Capability>() == 104);
const _: () = assert!(std::mem::size_of::<PixFormat>() == 48);
const _: () = assert!(std::mem::size_of::<Format>() == 208);
const _: () = assert!(std::mem::size_of::<StreamParm>() == 204);

const VIDIOC_QUERYCAP: libc::c_ulong = 0x8068_5600;
const VIDIOC_S_FMT: libc::c_ulong = 0xc0d0_5605;
const VIDIOC_S_PARM: libc::c_ulong = 0xc0cc_5616;
const VIDIOC_STREAMOFF: libc::c_ulong = 0x4004_5613;
const V4L2_BUF_TYPE_VIDEO_OUTPUT: u32 = 2;
const V4L2_CAP_VIDEO_OUTPUT: u32 = 0x2;
const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const V4L2_PIX_FMT_YUYV: u32 = u32::from_le_bytes(*b"YUYV");
const V4L2_FIELD_NONE: u32 = 1;

/// The text of a fixed-size, zero-padded field.
fn text(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Whether the driver and card label an open device reports are those of a loopback device made
/// for remoteman.
fn is_ours(driver: &str, card: &str) -> bool {
    driver.to_lowercase().starts_with("v4l2 loopback") && card.to_lowercase().contains(LABEL)
}

/// An open loopback device being written to. Dropping it stops and closes the device.
pub(crate) struct Loopback {
    path: PathBuf,
    tx: Option<mpsc::Sender<Arc<Picture>>>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    /// Open `path` (from [`find`]) for pictures of this size. Fails, touching nothing, unless the
    /// device says it is a loopback device labelled for remoteman that takes pictures.
    pub fn open(path: &Path, width: u32, height: u32, fps: u32) -> Result<Loopback> {
        let file = File::options().read(true).write(true).open(path).with_context(|| format!("open {}", path.display()))?;
        let fd = file.as_raw_fd();

        // SAFETY (this and the ioctls below): each request gets a pointer to the zeroed structure
        // of exactly the size the request number encodes, alive for the call.
        let mut cap: Capability = unsafe { std::mem::zeroed() };
        ensure!(unsafe { libc::ioctl(fd, VIDIOC_QUERYCAP, &mut cap) } == 0, "{}: not a V4L2 device: {}", path.display(), std::io::Error::last_os_error());
        let (driver, card) = (text(&cap.driver), text(&cap.card));
        ensure!(is_ours(&driver, &card), "{}: not a loopback device for remoteman (driver \"{driver}\", card \"{card}\")", path.display());
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
        // With `exclusive_caps`, a device another program is already feeding no longer offers this.
        ensure!(caps & V4L2_CAP_VIDEO_OUTPUT != 0, "{}: does not take pictures right now (another program is feeding it?)", path.display());

        let mut format: Format = unsafe { std::mem::zeroed() };
        format.kind = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        format.pix = PixFormat {
            width,
            height,
            pixelformat: V4L2_PIX_FMT_YUYV,
            field: V4L2_FIELD_NONE,
            bytesperline: width * 2,
            sizeimage: width * height * 2,
            ..Default::default()
        };
        ensure!(unsafe { libc::ioctl(fd, VIDIOC_S_FMT, &mut format) } == 0, "{}: set the picture format: {}", path.display(), std::io::Error::last_os_error());
        let got = format.pix;
        if got.pixelformat != V4L2_PIX_FMT_YUYV || got.width != width || got.height != height || got.sizeimage < width * height * 2 {
            // A format pinned by readers that have the device open, or by `v4l2loopback-ctl`.
            bail!(
                "{}: wants {}x{} {} instead of {width}x{height} YUYV",
                path.display(),
                got.width,
                got.height,
                String::from_utf8_lossy(&got.pixelformat.to_le_bytes())
            );
        }

        // The frame rate readers are told; the device works without it.
        let mut parm: StreamParm = unsafe { std::mem::zeroed() };
        parm.kind = V4L2_BUF_TYPE_VIDEO_OUTPUT;
        parm.numerator = 1;
        parm.denominator = fps.max(1);
        let _ = unsafe { libc::ioctl(fd, VIDIOC_S_PARM, &mut parm) };

        let (tx, rx) = mpsc::channel::<Arc<Picture>>();
        let name = path.to_owned();
        let thread = std::thread::Builder::new()
            .name("remoteman-camera-v4l2".into())
            .spawn(move || write_pictures(file, &name, rx, width, height))
            .context("start the loopback thread")?;
        Ok(Loopback { path: path.to_owned(), tx: Some(tx), thread: Some(thread) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Show this picture from now on.
    pub fn show(&self, picture: Arc<Picture>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(picture);
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        // The thread ends when its channel does, and closes the device on its way out.
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The writing thread: every new picture, and the current one again every `IDLE_INTERVAL` while
/// none comes (black before the first), until the channel closes or the device fails.
fn write_pictures(mut file: File, path: &Path, rx: mpsc::Receiver<Arc<Picture>>, width: u32, height: u32) {
    let mut frame = vec![0u8; Layout::Yuy2.size(width, height)];
    Picture::black(width, height).pack(Layout::Yuy2, &mut frame);
    loop {
        match rx.recv_timeout(IDLE_INTERVAL) {
            Ok(picture) => {
                if picture.width == width && picture.height == height {
                    picture.pack(Layout::Yuy2, &mut frame);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        // One `write` is one picture.
        match file.write(&frame) {
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) => {}
            Err(e) => {
                eprintln!("camera: {}: {e}; the loopback camera stops", path.display());
                break;
            }
        }
    }
    // Leave the device as it was found: stopped, then closed (when `file` goes).
    let kind = V4L2_BUF_TYPE_VIDEO_OUTPUT as libc::c_int;
    // SAFETY: STREAMOFF takes a pointer to an int holding the buffer type.
    let _ = unsafe { libc::ioctl(file.as_raw_fd(), VIDIOC_STREAMOFF, &kind) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(devices: &[(&str, &str)]) -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!("remoteman-camera-test-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        for (device, label) in devices {
            std::fs::create_dir_all(root.join(device)).unwrap();
            std::fs::write(root.join(device).join("name"), format!("{label}\n")).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn finds_only_a_device_labelled_for_remoteman() {
        let dev = Path::new("/dev");
        let none = fake(&[("video0", "Logitech BRIO"), ("video1", "Logitech BRIO"), ("video2", "Dummy video device (0x0000)")]);
        assert_eq!(find_in(&none, dev), None);
        let one = fake(&[("video0", "Logitech BRIO"), ("video12", "Remoteman camera"), ("video10", "remoteman camera 2"), ("v4l-subdev0", "remoteman")]);
        assert_eq!(find_in(&one, dev), Some(PathBuf::from("/dev/video10")));
        assert_eq!(find_in(Path::new("/nonexistent/remoteman"), dev), None);
        for root in [none, one] {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn only_a_loopback_device_with_the_label_is_ours() {
        assert!(is_ours("v4l2 loopback", "remoteman camera"));
        assert!(is_ours("v4l2 loopback", "Anna's Remoteman"));
        assert!(!is_ours("uvcvideo", "remoteman camera"));
        assert!(!is_ours("v4l2 loopback", "OBS Virtual Camera"));
        assert_eq!(text(b"v4l2 loopback\0\0\0"), "v4l2 loopback");
        assert_eq!(text(b"full"), "full");
    }

    #[test]
    fn a_file_that_is_not_a_device_is_refused() {
        let path = std::env::temp_dir().join(format!("remoteman-camera-test-notadevice-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        assert!(Loopback::open(&path, 1280, 720, 30).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        std::fs::remove_file(&path).unwrap();
    }
}
