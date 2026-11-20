//! The host's half: make the virtual camera, feed it.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, RegCloseKey, RegOpenKeyExW};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{HSTRING, Result};

use crate::CLSID_VCAM_STRING;
use crate::ring::{self, Ring};
use crate::shm::{self, Mapping};
use crate::source::{FPS, HEIGHT, WIDTH};

/// Who sees the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// The user the program runs as. (From the host's service that is SYSTEM, which is nobody
    /// who sits at the PC.)
    CurrentUser,
    /// Everyone who signs in (needs the program to be elevated or SYSTEM).
    AllUsers,
}

/// Is the media source's class registered machine-wide (the installer did it)? The host offers
/// `camera` only if it is.
pub fn installed() -> bool {
    key_exists(HKEY_LOCAL_MACHINE)
}

/// Is it registered for the current user (the test setup)?
pub fn installed_for_user() -> bool {
    key_exists(windows::Win32::System::Registry::HKEY_CURRENT_USER)
}

fn key_exists(root: HKEY) -> bool {
    let path = HSTRING::from(format!(r"Software\Classes\CLSID\{CLSID_VCAM_STRING}\InprocServer32"));
    let mut key = HKEY::default();
    // SAFETY: plain registry calls.
    let status = unsafe { RegOpenKeyExW(root, &path, None, KEY_READ | KEY_WOW64_64KEY, &mut key) };
    if status.is_ok() {
        unsafe {
            let _ = RegCloseKey(key);
        }
    }
    status != ERROR_FILE_NOT_FOUND && status.is_ok()
}

/// A virtual camera on this PC, and the ring that feeds it. Dropping it takes the camera away.
pub struct VirtualCamera {
    camera: IMFVirtualCamera,
    ring: Ring,
    // Keeps the ring's memory mapped for as long as the ring is used.
    _mapping: Mapping,
    /// Where the ring's memory is: the global section, or the file.
    pub backing: &'static str,
}

// SAFETY: the camera interface is free-threaded; the ring is for one writer, which `&mut self`
// of `push` keeps true.
unsafe impl Send for VirtualCamera {}

impl VirtualCamera {
    /// Make the camera called `name`; it shows black until [`VirtualCamera::push`] is called.
    pub fn start(name: &str, access: Access) -> Result<VirtualCamera> {
        // The ring first, so that the source finds it the moment it is started. The memory is
        // sized for the largest picture the ring format takes in v1 (1920x1080), whatever size
        // is used.
        let capacity = ring::ring_bytes(1920, 1080);
        let (mapping, backing) = match Mapping::create_section(shm::SECTION_NAME, capacity, shm::SECTION_SDDL) {
            Ok(m) => (m, "section"),
            Err(_) => (Mapping::create_file(&shm::file_path(), capacity)?, "file"),
        };
        // SAFETY: the mapping is kept as long as the ring.
        let ring = unsafe { Ring::create(mapping.ptr(), mapping.len(), WIDTH, HEIGHT, FPS) }.map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        ring.set_active(true, shm::now_ms());
        unsafe {
            MFStartup(MF_VERSION, 0)?;
            let camera = MFCreateVirtualCamera(
                MFVirtualCameraType_SoftwareCameraSource,
                MFVirtualCameraLifetime_Session,
                if access == Access::AllUsers { MFVirtualCameraAccess_AllUsers } else { MFVirtualCameraAccess_CurrentUser },
                &HSTRING::from(name),
                &HSTRING::from(CLSID_VCAM_STRING),
                None,
            )
            .map_err(|e| windows::core::Error::new(e.code(), format!("MFCreateVirtualCamera: {e}")))?;
            camera.Start(None).map_err(|e| windows::core::Error::new(e.code(), format!("starting the virtual camera: {e}")))?;
            Ok(VirtualCamera { camera, ring, _mapping: mapping, backing })
        }
    }

    /// Show a picture: NV12, [`WIDTH`]x[`HEIGHT`], video range. `capture_us` is when it was
    /// captured, the sender's clock.
    pub fn push(&mut self, nv12: &[u8], capture_us: u64) {
        self.ring.write(nv12, capture_us, shm::now_ms());
    }

    /// Say the writer is still here (call at least every [`ring::BEAT_MS`]).
    pub fn beat(&self) {
        self.ring.beat(shm::now_ms());
    }
}

impl Drop for VirtualCamera {
    fn drop(&mut self) {
        self.ring.set_active(false, shm::now_ms());
        unsafe {
            let _ = self.camera.Stop();
            let _ = self.camera.Remove();
            let _ = self.camera.Shutdown();
            let _ = MFShutdown();
        }
    }
}
