//! Where the ring's memory comes from: a named section the host (SYSTEM) creates in the global
//! namespace and the Frame Server's account may read, or, where that isn't possible (a host
//! run by a person, without the privilege for `Global\`), a mapped file in the machine's
//! ProgramData that the same readers can open. The media source tries the section first, then
//! the file.

use std::path::PathBuf;

use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LocalFree};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING};
use windows::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_ALL_ACCESS, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile, VirtualQuery, MEMORY_BASIC_INFORMATION};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::core::{HSTRING, PCWSTR};

/// The section's name.
pub const SECTION_NAME: &str = "Global\\remoteman-vcam-ring";

/// Who may do what with the section: SYSTEM and administrators everything; LOCAL SERVICE (the
/// Frame Server) and the signed-in users read.
pub const SECTION_SDDL: &str = "D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;LS)(A;;GR;;;IU)";

/// Milliseconds since boot: the writer's sign of life and the readers' clock are this one.
pub fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

/// `%ProgramData%\remoteman\vcam`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("remoteman").join("vcam")
}

/// The mapped file used where the section can't be.
pub fn file_path() -> PathBuf {
    data_dir().join("ring.bin")
}

/// A view of shared memory, unmapped when dropped.
pub struct Mapping {
    handle: HANDLE,
    view: *mut u8,
    len: usize,
}

// SAFETY: a mapped view is just memory; what is done with it is the ring's business.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut _ });
            let _ = CloseHandle(self.handle);
        }
    }
}

impl Mapping {
    pub fn ptr(&self) -> *mut u8 {
        self.view
    }
    pub fn len(&self) -> usize {
        self.len
    }

    fn map(handle: HANDLE, access: windows::Win32::System::Memory::FILE_MAP) -> windows::core::Result<Mapping> {
        unsafe {
            let view = MapViewOfFile(handle, access, 0, 0, 0);
            if view.Value.is_null() {
                let e = windows::core::Error::from_thread();
                let _ = CloseHandle(handle);
                return Err(e);
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(Some(view.Value), &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>());
            Ok(Mapping { handle, view: view.Value as *mut u8, len: info.RegionSize })
        }
    }

    /// The named section, made (or, if it exists, taken: it keeps the size and the access it
    /// was made with) for writing.
    pub fn create_section(name: &str, len: usize, sddl: &str) -> windows::core::Result<Mapping> {
        unsafe {
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(sddl), SDDL_REVISION_1, &mut descriptor, None)?;
            let attributes = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into() };
            let made = CreateFileMappingW(INVALID_HANDLE_VALUE, Some(&attributes), PAGE_READWRITE, (len as u64 >> 32) as u32, len as u32, &HSTRING::from(name));
            let _ = LocalFree(Some(HLOCAL(descriptor.0)));
            Mapping::map(made?, FILE_MAP_ALL_ACCESS)
        }
    }

    /// The named section, for reading.
    pub fn open_section(name: &str) -> windows::core::Result<Mapping> {
        unsafe {
            let handle = windows::Win32::System::Memory::OpenFileMappingW(FILE_MAP_READ.0, false, &HSTRING::from(name))?;
            Mapping::map(handle, FILE_MAP_READ)
        }
    }

    /// The mapped file, made if need be and `len` bytes long, for writing. It is not truncated: a
    /// reader may have it mapped.
    pub fn create_file(path: &std::path::Path, len: usize) -> windows::core::Result<Mapping> {
        unsafe {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // TEMPORARY: the pictures are not worth writing to disk.
            let file = CreateFileW(&HSTRING::from(path.as_os_str()), (GENERIC_READ | GENERIC_WRITE).0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, None)?;
            let made = CreateFileMappingW(file, None, PAGE_READWRITE, (len as u64 >> 32) as u32, len as u32, PCWSTR::null());
            let _ = CloseHandle(file);
            Mapping::map(made?, FILE_MAP_ALL_ACCESS)
        }
    }

    /// The mapped file, for reading.
    pub fn open_file(path: &std::path::Path) -> windows::core::Result<Mapping> {
        unsafe {
            let file = CreateFileW(&HSTRING::from(path.as_os_str()), GENERIC_READ.0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, None)?;
            let made = CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, PCWSTR::null());
            let _ = CloseHandle(file);
            Mapping::map(made?, FILE_MAP_READ)
        }
    }

    /// The ring's memory as the readers find it: the section, else the file.
    pub fn open_ring() -> windows::core::Result<Mapping> {
        Mapping::open_section(SECTION_NAME).or_else(|_| Mapping::open_file(&file_path()))
    }
}
