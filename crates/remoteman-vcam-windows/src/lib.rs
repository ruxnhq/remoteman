//! A connected device's camera as a camera on this Windows PC.
//!
//! Windows 11 lets a program make a "virtual camera" (`MFCreateVirtualCamera`): the Frame Server
//! loads a media source of the program's making, as a COM in-process server, and shows it to every
//! program as a camera. This crate is both halves of that:
//!
//! - the DLL (`remoteman_vcam_windows.dll`): the media source ([`source`]), with the COM
//!   exports below. It is registered under `HKLM\Software\Classes\CLSID\{CLSID}\InprocServer32`
//!   by the host's installer (`windows/host-installer/setup.ps1`), threading model `Both`.
//! - the library the host links ([`host`]): it makes the virtual camera, and feeds the
//!   pictures of the device's camera into the shared-memory ring ([`ring`], [`shm`]) that the media
//!   source, inside the Frame Server's process, reads.
//!
//! The media source offers one mode, NV12 1280x720 at 30 a second (the host letterboxes the
//! device's picture into it, as the Linux host does), and shows black while the ring has no
//! live writer.
//!
//! On other systems only [`ring`] is built (its tests run anywhere).

pub mod ring;

#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod shm;
#[cfg(windows)]
pub mod source;

#[cfg(windows)]
mod com {
    use std::sync::atomic::Ordering;

    use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, S_FALSE, S_OK};
    use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
    use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};

    use crate::source::{LIVE, MediaSource};
    use crate::{CLSID_VCAM, log};

    #[implement(IClassFactory)]
    struct Factory;

    impl IClassFactory_Impl for Factory_Impl {
        fn CreateInstance(&self, outer: Ref<IUnknown>, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> Result<()> {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe { *ppv = std::ptr::null_mut() };
            if outer.is_some() {
                return Err(CLASS_E_NOAGGREGATION.into());
            }
            log("CreateInstance");
            let source = MediaSource::create().inspect_err(|e| log(&format!("CreateInstance failed: {e}")))?;
            let unknown: IUnknown = source.cast()?;
            unsafe { unknown.query(riid, ppv) }.ok()
        }

        fn LockServer(&self, lock: BOOL) -> Result<()> {
            LIVE.fetch_add(if lock.as_bool() { 1 } else { -1 }, Ordering::SeqCst);
            Ok(())
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn DllGetClassObject(clsid: *const GUID, riid: *const GUID, ppv: *mut *mut core::ffi::c_void) -> HRESULT {
        if clsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        unsafe { *ppv = std::ptr::null_mut() };
        if unsafe { *clsid } != CLSID_VCAM {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        log("DllGetClassObject");
        let factory: IClassFactory = Factory.into();
        unsafe { factory.query(riid, ppv) }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn DllCanUnloadNow() -> HRESULT {
        if LIVE.load(Ordering::SeqCst) <= 0 { S_OK } else { S_FALSE }
    }
}

/// The media source's class id: what the host passes to `MFCreateVirtualCamera` and the
/// installer registers.
pub const CLSID_VCAM: windows_core::GUID = windows_core::GUID::from_u128(0xB3F1C4A2_6D5E_4F87_9A21_7C0E3D5B8A64);
/// The same, as the registry and `MFCreateVirtualCamera` spell it.
pub const CLSID_VCAM_STRING: &str = "{B3F1C4A2-6D5E-4F87-9A21-7C0E3D5B8A64}";

/// A line in `%ProgramData%\remoteman\vcam\vcam.log`, if that file exists (make an empty one to
/// ask for the log: the DLL runs in a service where nothing else shows what it does), and always
/// in the debugger.
#[cfg(windows)]
pub(crate) fn log(message: &str) {
    use std::io::Write;
    let exe = std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let line = format!("[{} {exe} pid {}] {message}\n", shm::now_ms(), std::process::id());
    unsafe {
        windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(&windows::core::HSTRING::from(format!("remoteman-vcam: {line}")));
    }
    let path = shm::data_dir().join("vcam.log");
    if path.exists() {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}
