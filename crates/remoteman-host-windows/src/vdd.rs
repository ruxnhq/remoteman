//! The display driver's side of the bargain: Guesthouse's `drivers/windows-display` (a fork of
//! zako-vdd), spoken to through its control device. Everything that is this driver's own, and
//! no other's, lives here: the interface GUID, the IOCTLs, the text commands, and the layout of
//! the frame channel's metadata with the rule for reading it. The rest of the host sees a
//! monitor that can be asked for modes ([`Driver::set_modes`]) and a [`Channel`] of shared
//! textures with a newest frame.
//!
//! The contract, as read from the driver's source (`Common/Include/vdd_control_ioctl.h`,
//! `Rendering/SharedFrameExporter.cpp`):
//! - The device takes SYSTEM and Administrators only, so this process must be elevated.
//! - The driver copies frames into the channel only once something has asked for the
//!   channel's capabilities, and the channel itself (three textures, a metadata section, an
//!   event) is made by the first frame after that. Until then opening it says "not ready".
//! - Each texture has a keyed mutex: the driver takes it with key 0 and releases it with key 1
//!   when a frame is in it; a reader takes it with key 1 and gives it back with key 0. When no
//!   texture is free the driver overwrites an unread one, so the newest frame always wins.
//! - The metadata is written under a sequence number that is odd while it is being written.
//!   Its high half counts the times the textures were made anew (another size or format): after
//!   that the handles held here show a picture that no longer changes, and the channel must be
//!   opened again. The same goes when Windows gives the monitor a new swap chain, which this
//!   record can't show: see [`Channel::same_as`].

use std::ffi::c_void;
use std::sync::atomic::{Ordering, compiler_fence};

use anyhow::{Context, Result, bail, ensure};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Foundation::{CloseHandle, CompareObjectHandles, ERROR_NOT_READY, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Memory::{FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, UnmapViewOfFile};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::core::{GUID, HSTRING, PCWSTR};

/// `GUID_DEVINTERFACE_ZAKO_VDD_CONTROL`.
const CONTROL: GUID = GUID::from_u128(0xDA9F8C2B_7E4F_49A1_9D4E_6F2B0E1A0C4D);

/// `CTL_CODE(FILE_DEVICE_UNKNOWN, function, METHOD_BUFFERED, access)`.
const fn ctl(function: u32, access: u32) -> u32 {
    (0x22 << 16) | (access << 14) | (function << 2)
}
const FILE_READ_ACCESS: u32 = 1;
const FILE_WRITE_DATA: u32 = 2;
const IOCTL_COMMAND: u32 = ctl(0x800, FILE_WRITE_DATA);
const IOCTL_PING: u32 = ctl(0x801, FILE_READ_ACCESS);
const IOCTL_QUERY_CAPS: u32 = ctl(0x802, FILE_READ_ACCESS);
const IOCTL_OPEN_CHANNEL: u32 = ctl(0x803, FILE_READ_ACCESS | FILE_WRITE_DATA);
const _: () = assert!(IOCTL_COMMAND == 0x22A000 && IOCTL_PING == 0x226004 && IOCTL_QUERY_CAPS == 0x226008 && IOCTL_OPEN_CHANNEL == 0x22E00C);

const MAX_SLOTS: usize = 8;

/// What marks this driver's adapter among the display adapters Windows lists (its device
/// description, hardware id or name), most telling first.
pub const ADAPTER_HINTS: [&str; 4] = ["guesthousedisplay", "guesthouse", "rdpudd chained", "zakovdd"];

/// `VDD_FRAME_CHANNEL_CAPS`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Caps {
    size: u32,
    pub version: u32,
    pub flags: u32,
    pub max_slots: u32,
    pub metadata_size: u32,
    reserved0: u32,
    reserved1: u64,
}

/// `VDD_FRAME_CHANNEL_OPEN_REQUEST`.
#[repr(C)]
#[derive(Default)]
struct OpenRequest {
    size: u32,
    version: u32,
    monitor: u32,
    required_flags: u32,
    target_pid: u32,
    desired_slots: u32,
    luid_low: u32,
    luid_high: i32,
    reserved0: u64,
}

/// `VDD_FRAME_CHANNEL_OPEN_RESPONSE`, with its slots' `{ handle, reserved }` pairs.
#[repr(C)]
struct OpenResponse {
    size: u32,
    version: u32,
    flags: u32,
    slot_count: u32,
    metadata_size: u32,
    reserved0: u32,
    metadata: u64,
    frame_ready: u64,
    slots: [[u64; 2]; MAX_SLOTS],
}

/// `SharedFrameMetadata`: one record, rewritten for every frame.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Metadata {
    /// 'ZVDF'.
    pub magic: u32,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// A `DXGI_FORMAT`.
    pub format: u32,
    pub is_hdr: u32,
    pub max_nits: f32,
    pub min_nits: f32,
    pub max_fall: f32,
    /// Counts the frames put into the channel.
    pub frame_counter: u64,
    /// When Windows meant the frame to be shown, by the performance counter.
    pub present_qpc: u64,
    pub publish_qpc: u64,
    pub presentation_frame: u32,
    pub dirty_rects: u32,
    pub replaced_unread: u64,
    pub dropped_held: u64,
    pub dropped_failed: u64,
    pub metadata_size: u32,
    pub slot_count: u32,
    /// The slot holding the newest frame.
    pub slot: u32,
    /// High half: the channel's generation. Low half: even when the record is whole.
    pub sequence: u32,
    /// The adapter the driver renders on: the one the textures can be opened on.
    pub luid_low: u32,
    pub luid_high: i32,
    pub qpc_frequency: u64,
}

const _: () = assert!(size_of::<Caps>() == 32 && size_of::<OpenRequest>() == 40 && size_of::<OpenResponse>() == 40 + 16 * MAX_SLOTS && size_of::<Metadata>() == 128);
const _: () = assert!(std::mem::offset_of!(Metadata, metadata_size) == 96 && std::mem::offset_of!(Metadata, sequence) == 108 && std::mem::offset_of!(Metadata, qpc_frequency) == 120);

const MAGIC: u32 = 0x5A56_4446;

impl Metadata {
    pub fn generation(&self) -> u16 {
        (self.sequence >> 16) as u16
    }
}

/// The driver's control device, open.
pub struct Driver {
    handle: HANDLE,
    /// The device interface's path, for the log.
    pub path: String,
}

// SAFETY: a file handle; DeviceIoControl on it may be called from any thread.
unsafe impl Send for Driver {}
unsafe impl Sync for Driver {}

impl Driver {
    /// Find the driver's control interface and open it. Opening it also wakes the driver.
    pub fn open() -> Result<Self> {
        let path = interface_path().context("looking for the display driver's control interface")?;
        // SAFETY: plain calls; `wide` outlives the call.
        let handle = unsafe {
            let wide = HSTRING::from(path.as_str());
            // The driver opens this process while impersonating it, to hand it the channel's
            // handles: say outright that it may.
            let flags = FILE_FLAGS_AND_ATTRIBUTES(SECURITY_SQOS_PRESENT.0 | SECURITY_IMPERSONATION.0);
            CreateFileW(&wide, (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, flags, None)
        }
        .with_context(|| format!("opening {path} (the driver lets in SYSTEM and Administrators only: is this process elevated?)"))?;
        Ok(Self { handle, path })
    }

    fn ioctl(&self, code: u32, input: Option<&[u8]>, output: Option<&mut [u8]>) -> windows::core::Result<u32> {
        let mut returned = 0u32;
        let (in_ptr, in_len) = input.map_or((None, 0), |b| (Some(b.as_ptr() as *const c_void), b.len() as u32));
        let (out_ptr, out_len) = output.map_or((None, 0), |b| (Some(b.as_mut_ptr() as *mut c_void), b.len() as u32));
        // SAFETY: the buffers are live for the call and of the lengths given.
        unsafe { DeviceIoControl(self.handle, code, in_ptr, in_len, out_ptr, out_len, Some(&mut returned), None) }?;
        Ok(returned)
    }

    /// Is the driver there and answering?
    pub fn ping(&self) -> Result<()> {
        self.ioctl(IOCTL_PING, None, None).map(drop).context("the display driver didn't answer a ping")
    }

    /// Send one of the driver's text commands. The driver answers "done" whatever came of it
    /// (what it made of the command is only in its ETW trace, provider `Guesthouse.Display`),
    /// so success here means no more than that it was delivered.
    ///
    /// Only `SETMODES` and `REFRESHMODES` are sent by this host. Most of the others rewrite the
    /// driver's settings file and reload it, which is not this program's to do.
    fn command(&self, text: &str) -> Result<()> {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        ensure!(wide.len() <= 2048, "a driver command of {} characters; 2047 is the most", wide.len() - 1);
        // SAFETY: reading the u16s as bytes.
        let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
        self.ioctl(IOCTL_COMMAND, Some(bytes), None).map(drop).with_context(|| format!("sending the driver {:?}", text.split(' ').next().unwrap_or_default()))
    }

    /// Replace the list of modes the driver's monitors offer, as (width, height, refresh). The
    /// first is the preferred one. If Windows' description of a monitor lacks one of them the
    /// driver takes that monitor away and brings it back, which takes a moment and gives it a
    /// new swap chain (so a [`Channel`] opened before is dead afterwards).
    pub fn set_modes(&self, modes: &[(u32, u32, u32)]) -> Result<()> {
        ensure!(!modes.is_empty(), "no modes to set");
        // The driver itself keeps 96 at most (the first and the last 95).
        ensure!(modes.len() <= 96, "{} modes; the driver takes 96", modes.len());
        let list: Vec<String> = modes.iter().map(|(w, h, r)| format!("{w}x{h}x{r}")).collect();
        self.command(&format!("SETMODES {}", list.join(",")))
    }

    /// What the frame channel is like. Asking is also what makes the driver start copying
    /// frames into it; it doesn't until someone has.
    pub fn caps(&self) -> Result<Caps> {
        let mut caps = Caps::default();
        // SAFETY: plain old data, written as bytes.
        let bytes = unsafe { std::slice::from_raw_parts_mut(&mut caps as *mut Caps as *mut u8, size_of::<Caps>()) };
        let n = self.ioctl(IOCTL_QUERY_CAPS, None, Some(bytes)).context("asking the driver about its frame channel")?;
        ensure!(n as usize >= size_of::<Caps>(), "the driver's answer about its frame channel was {n} bytes");
        ensure!(caps.version == 1, "the driver's frame channel is version {}; this host knows 1", caps.version);
        ensure!(caps.metadata_size as usize >= size_of::<Metadata>(), "the frame channel's metadata is {} bytes; {} are needed", caps.metadata_size, size_of::<Metadata>());
        Ok(caps)
    }

    /// Have the driver hand this process the frame channel of one of its monitors. `None`: the
    /// channel isn't there yet (no frame has been shown since it was first asked about, or the
    /// monitor is between two modes); try again after the desktop has drawn something.
    pub fn open_channel(&self, monitor: u32) -> Result<Option<Channel>> {
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        // Any adapter: which one it is comes back in the metadata.
        let request = OpenRequest { size: size_of::<OpenRequest>() as u32, version: 1, monitor, target_pid: pid, ..Default::default() };
        let mut response = std::mem::MaybeUninit::<OpenResponse>::zeroed();
        // SAFETY: both are plain old data, read and written as bytes.
        let (input, output) = unsafe {
            (std::slice::from_raw_parts(&request as *const OpenRequest as *const u8, size_of::<OpenRequest>()), std::slice::from_raw_parts_mut(response.as_mut_ptr() as *mut u8, size_of::<OpenResponse>()))
        };
        let n = match self.ioctl(IOCTL_OPEN_CHANNEL, Some(input), Some(output)) {
            Ok(n) => n,
            Err(e) if e.code() == ERROR_NOT_READY.to_hresult() => return Ok(None),
            Err(e) => return Err(e).context("opening the driver's frame channel"),
        };
        // SAFETY: zeroed to begin with, and every bit pattern is a valid value.
        let r = unsafe { response.assume_init() };
        // From here on the handles are this process's, and `Channel` closes them.
        let slots = r.slots.iter().take((r.slot_count as usize).min(MAX_SLOTS)).map(|s| HANDLE(s[0] as *mut c_void)).filter(|h| !h.0.is_null()).collect();
        let mut channel = Channel { slots, mapping: HANDLE(r.metadata as *mut c_void), ready: HANDLE(r.frame_ready as *mut c_void), view: std::ptr::null(), flags: r.flags };
        ensure!(n as usize >= size_of::<OpenResponse>() && r.version == 1, "the driver's frame channel answered {n} bytes, version {}", r.version);
        ensure!(!channel.slots.is_empty() && !channel.mapping.0.is_null() && !channel.ready.0.is_null(), "the driver's frame channel came without its handles");
        ensure!(r.metadata_size as usize >= size_of::<Metadata>(), "the frame channel's metadata is {} bytes; {} are needed", r.metadata_size, size_of::<Metadata>());
        // SAFETY: a section handle the driver gave this process, mapped to read.
        let view = unsafe { MapViewOfFile(channel.mapping, FILE_MAP_READ, 0, 0, size_of::<Metadata>()) };
        if view.Value.is_null() {
            bail!("mapping the frame channel's metadata: {}", windows::core::Error::from_thread());
        }
        channel.view = view.Value as *const Metadata;
        Ok(Some(channel))
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // SAFETY: ours, and not used again.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// One monitor's frame channel, as handles in this process.
pub struct Channel {
    /// The shared textures, to be opened on a Direct3D device (`OpenSharedResource1`).
    pub slots: Vec<HANDLE>,
    mapping: HANDLE,
    /// Set by the driver whenever a frame has been put into a slot (it resets itself).
    pub ready: HANDLE,
    view: *const Metadata,
    pub flags: u32,
}

// SAFETY: handles and a read-only mapping; used from the picture's thread only.
unsafe impl Send for Channel {}

impl Channel {
    /// The metadata as it stands, whole: never a record caught half written. `None` if the
    /// driver kept writing through every try (it writes once a frame, so that doesn't last) or
    /// the record isn't this driver's.
    pub fn snapshot(&self) -> Option<Metadata> {
        let sequence = unsafe { &raw const (*self.view).sequence };
        for _ in 0..64 {
            // SAFETY: the view is mapped for as long as `self` lives; the driver writes to it
            // from another process, hence the volatile reads and the check around them.
            let before = unsafe { sequence.read_volatile() };
            if before & 1 == 0 {
                compiler_fence(Ordering::SeqCst);
                // SAFETY: as above.
                let record = unsafe { self.view.read_volatile() };
                compiler_fence(Ordering::SeqCst);
                // SAFETY: as above.
                if unsafe { sequence.read_volatile() } == before {
                    let whole = record.magic == MAGIC && record.version == 1 && (record.slot as usize) < self.slots.len();
                    return whole.then_some(Metadata { sequence: before, ..record });
                }
            }
            std::thread::yield_now();
        }
        None
    }

    /// Is `other` the same channel as this one (the same event object behind the handles)? The
    /// driver makes a new one for every swap chain Windows gives its monitor, and nothing in
    /// the old one says that it has been left behind: asking for the channel again and
    /// comparing is the way to find out.
    pub fn same_as(&self, other: &Channel) -> bool {
        // SAFETY: two live handles.
        unsafe { CompareObjectHandles(self.ready, other.ready) }.as_bool()
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        // SAFETY: ours, and not used again.
        unsafe {
            if !self.view.is_null() {
                let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut c_void });
            }
            for h in self.slots.drain(..).chain([self.mapping, self.ready]) {
                if !h.0.is_null() {
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

/// The path of the driver's control interface, by its interface class.
fn interface_path() -> Result<String> {
    // SAFETY: the SetupDi calls as documented; the buffer is sized as they ask and aligned for
    // the detail struct.
    unsafe {
        let set = SetupDiGetClassDevsW(Some(&CONTROL), PCWSTR::null(), None, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE).context("SetupDiGetClassDevs")?;
        let found = (|| {
            let mut data = SP_DEVICE_INTERFACE_DATA { cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32, ..Default::default() };
            SetupDiEnumDeviceInterfaces(set, None, &CONTROL, 0, &mut data).context("no such interface is present: is the display driver installed and started?")?;
            let mut needed = 0u32;
            // Fails by design, saying how much room the path needs.
            let _ = SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut needed), None);
            ensure!(needed as usize >= size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>(), "SetupDiGetDeviceInterfaceDetail asked for {needed} bytes");
            let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
            let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), needed, None, None).context("SetupDiGetDeviceInterfaceDetail")?;
            let path = PCWSTR((&raw const (*detail).DevicePath) as *const u16);
            Ok(path.to_string()?)
        })();
        let _ = SetupDiDestroyDeviceInfoList(set);
        found
    }
}
