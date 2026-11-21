//! What the installer asks of Windows, in one place: elevation, processes, the service, the
//! registry, folders only administrators can write to, a shortcut, a message box. Nothing here
//! decides whether to change anything: src/engine.rs does, and only when it is not a dry run.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LUID, LocalFree, WAIT_OBJECT_0};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{
    AdjustTokenPrivileges, GetTokenInformation, LookupPrivilegeValueW, PSECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED, SECURITY_ATTRIBUTES, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES, TOKEN_QUERY,
    TokenElevation,
};
use windows::Win32::Storage::FileSystem::{CreateDirectoryW, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING, MoveFileExW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STOP,
    SERVICE_STOPPED,
};
use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateMutexW, GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{FOLDERID_CommonPrograms, FOLDERID_ProgramFiles, FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, KF_FLAG_DEFAULT, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SEE_MASK_NOZONECHECKS, SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW, SW_SHOWNORMAL};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR, w};

use crate::{Product, product};

/// The service setup.ps1 registers, and the firewall rule's and the old logon task's name.
pub const SERVICE: &str = "remoteman host";
/// Our entry in Settings > Apps: the host's in HKLM, the viewer's in HKCU (`hive`).
pub fn uninstall_key() -> String {
    format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{}", product().name())
}
/// The Inno Setup installer's entry (remoteman-host.iss, AppId).
pub const INNO_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{6A0F3C2E-5B7D-4E8A-9C1F-2D4B6E8A0C13}_is1";
pub const RUN_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
pub const RUN_VALUE: &str = "remoteman host";
pub const SHORTCUT: &str = "Remoteman Host.lnk";
/// The viewer's shortcut in its user's Start menu.
pub const VIEWER_SHORTCUT: &str = "remoteman.lnk";

fn wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

/// Libraries loaded by name come from System32 only, never from the exe's own folder.
pub fn safe_dll_search() {
    // SAFETY: plain call.
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

/// `--pack` prints to the console it was started from, unless its output goes somewhere already.
pub fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: plain calls.
    unsafe {
        if GetStdHandle(STD_OUTPUT_HANDLE).map(|h| h.is_invalid() || h.0.is_null()).unwrap_or(true) {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

pub fn is_elevated() -> bool {
    // SAFETY: the token is closed; the structure is the size given.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(token, TokenElevation, Some(&mut elevation as *mut _ as *mut _), size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// So that a host running as SYSTEM can be ended by its pid.
pub fn enable_debug_privilege() {
    // SAFETY: the token is closed; the structure is one privilege long.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token).is_err() {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(None, w!("SeDebugPrivilege"), &mut luid).is_ok() {
            let mut privileges = TOKEN_PRIVILEGES { PrivilegeCount: 1, ..Default::default() };
            privileges.Privileges[0].Luid = luid;
            privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
            let _ = AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None);
        }
        let _ = CloseHandle(token);
    }
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the string Windows allocates is freed with its allocator.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let path = PathBuf::from(p.to_string().ok()?);
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        Some(path)
    }
}

/// `C:\Program Files\remoteman host`, or the viewer's `%LOCALAPPDATA%\Programs\remoteman`.
pub fn install_dir() -> PathBuf {
    match product() {
        Product::Host => known_folder(&FOLDERID_ProgramFiles).unwrap_or_else(|| PathBuf::from(r"C:\Program Files")).join("remoteman host"),
        Product::Viewer => known_folder(&FOLDERID_UserProgramFiles)
            .or_else(|| std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs")))
            .unwrap_or_else(|| std::env::temp_dir().join("Programs"))
            .join("remoteman"),
    }
}

/// The Start menu's folder for every user.
pub fn common_programs() -> Option<PathBuf> {
    known_folder(&FOLDERID_CommonPrograms)
}

/// The Start menu's folder of the user who runs this.
pub fn user_programs() -> Option<PathBuf> {
    known_folder(&FOLDERID_Programs)
}

pub fn windows_dir() -> PathBuf {
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer is the length given.
    let n = unsafe { GetSystemWindowsDirectoryW(Some(&mut buffer)) } as usize;
    if n == 0 || n >= buffer.len() {
        return PathBuf::from(r"C:\Windows");
    }
    PathBuf::from(String::from_utf16_lossy(&buffer[..n]))
}

pub fn powershell() -> PathBuf {
    windows_dir().join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
}

/// Whether `inner` is `outer` or under it (paths as Windows compares them: without case).
pub fn is_under(inner: &Path, outer: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase().replace('/', "\\");
    let (i, o) = (norm(inner), norm(outer));
    i == o || i.starts_with(&format!("{o}\\"))
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    is_under(a, b) && is_under(b, a)
}

/// A new, empty folder. Elevated, it is under Windows' own Temp and only Administrators and
/// SYSTEM may write to it (nothing the user's unelevated programs can put a DLL into);
/// otherwise under the user's %TEMP%.
pub fn private_dir(elevated: bool) -> io::Result<PathBuf> {
    let base = if elevated { windows_dir().join("Temp") } else { std::env::temp_dir() };
    for attempt in 0..20u32 {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let dir = base.join(format!("{}-{}-{:08x}", product().setup_name(), std::process::id(), nanos.wrapping_add(attempt)));
        let made = if elevated {
            // SAFETY: the security descriptor lives until LocalFree, after the call.
            unsafe {
                let mut sd = PSECURITY_DESCRIPTOR::default();
                ConvertStringSecurityDescriptorToSecurityDescriptorW(w!("D:P(A;OICI;FA;;;BA)(A;OICI;FA;;;SY)"), SDDL_REVISION_1, &mut sd, None).map_err(io::Error::other)?;
                let attributes = SECURITY_ATTRIBUTES { nLength: size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd.0, bInheritHandle: false.into() };
                let made = CreateDirectoryW(&HSTRING::from(dir.as_os_str()), Some(&attributes));
                let _ = LocalFree(Some(HLOCAL(sd.0)));
                made.map_err(io::Error::other)
            }
        } else {
            std::fs::create_dir(&dir)
        };
        match made {
            Ok(()) => return Ok(dir),
            Err(_) if dir.exists() => continue,
            Err(e) => return Err(io::Error::other(format!("{}: {e}", dir.display()))),
        }
    }
    Err(io::Error::other("no new folder could be made in Temp"))
}

/// Ends with Windows: one setup at a time.
pub fn single_instance() -> bool {
    // SAFETY: the handle is kept for the life of the process on purpose.
    unsafe {
        match CreateMutexW(None, true, &HSTRING::from(format!("Local\\{}", product().setup_name()))) {
            Ok(_) => windows::Win32::Foundation::GetLastError() != windows::Win32::Foundation::ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}

pub fn message(text: &str) {
    // SAFETY: plain call.
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(format!("{} setup", product().name())), MB_OK | MB_ICONERROR | MB_SETFOREGROUND);
    }
}

pub fn allow_foreground() {
    // SAFETY: plain call.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

// ---- Command lines and processes ----

/// One argument as the C runtime's parser reads it back.
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

pub enum Launched {
    Process(OwnedProcess),
    /// The user said no to the UAC prompt.
    Cancelled,
}

pub struct OwnedProcess(HANDLE);

impl OwnedProcess {
    /// Waits for it to end; its exit code.
    pub fn wait(&self) -> u32 {
        // SAFETY: the handle is ours until drop.
        unsafe {
            WaitForSingleObject(self.0, INFINITE);
            let mut code = 1u32;
            let _ = GetExitCodeProcess(self.0, &mut code);
            code
        }
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: ours to close.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Starts `exe` (this program's copy) as this process is, elevated if it is, with no shell and
/// so no prompt of any kind, not waiting.
pub fn spawn(exe: &Path, args: &[String], dir: &Path) -> io::Result<OwnedProcess> {
    use std::os::windows::io::IntoRawHandle;
    let child = std::process::Command::new(exe).args(args).current_dir(dir).spawn()?;
    Ok(OwnedProcess(HANDLE(child.into_raw_handle())))
}

/// Starts `exe`, this program's own file (or its copy), again through the shell: `runas` asks
/// for elevation (the UAC prompt), `open` doesn't. With no zone check: a downloaded file the
/// user has just opened would otherwise be asked about a second time, before the UAC prompt.
pub fn shell_start(exe: &Path, args: &[String], verb: &str, dir: &Path) -> io::Result<Launched> {
    let params = args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
    let (verb, file, params, dir) = (HSTRING::from(verb), HSTRING::from(exe.as_os_str()), HSTRING::from(params), HSTRING::from(dir.as_os_str()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_NOZONECHECKS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: the strings outlive the call; the process handle is owned after it.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) if !info.hProcess.is_invalid() => Ok(Launched::Process(OwnedProcess(info.hProcess))),
        Ok(()) => Err(io::Error::other("Windows started it but gave no process back")),
        Err(e) if e.code() == windows::Win32::Foundation::ERROR_CANCELLED.to_hresult() => Ok(Launched::Cancelled),
        Err(e) => Err(io::Error::other(e)),
    }
}

/// Runs a program with no window and waits, with a time limit; its exit code and output.
pub fn run_hidden(exe: &Path, args: &[&str], timeout: Duration) -> io::Result<(i32, String)> {
    let mut child = std::process::Command::new(exe)
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let out = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut out) = out {
            let _ = std::io::Read::read_to_string(&mut out, &mut text);
        }
        text
    });
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok((status.code().unwrap_or(1), reader.join().unwrap_or_default()));
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            return Err(io::Error::other(format!("{} took longer than {} s", exe.display(), timeout.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The processes whose program is `exe` (its full path, compared without case).
pub fn processes_of(exe: &Path) -> Vec<u32> {
    let name = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut found = Vec::new();
    // SAFETY: the snapshot and each process handle are closed; the buffers are the sizes given.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return found };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase() == name
                && let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, entry.th32ProcessID)
            {
                let mut buffer = [0u16; 1024];
                let mut size = buffer.len() as u32;
                if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut size).is_ok() {
                    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]));
                    if same_path(&path, exe) {
                        found.push(entry.th32ProcessID);
                    }
                }
                let _ = CloseHandle(process);
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// Ends a process and waits up to five seconds for it to be gone.
pub fn end_process(pid: u32) -> io::Result<()> {
    // SAFETY: the handle is closed.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid).map_err(io::Error::other)?;
        let ended = TerminateProcess(process, 1).map_err(io::Error::other);
        let gone = WaitForSingleObject(process, 5000) == WAIT_OBJECT_0;
        let _ = CloseHandle(process);
        ended?;
        if !gone {
            return Err(io::Error::other(format!("process {pid} didn't end")));
        }
    }
    Ok(())
}

/// Starts a program as this process is (not elevated, from the starter), not waiting.
pub fn start(exe: &Path, args: &[&str]) -> io::Result<()> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    std::process::Command::new(exe).args(args).current_dir(dir).spawn().map(|_| ())
}

// ---- The service ----

pub enum ServiceState {
    Missing,
    Stopped,
    Running,
    Other,
}

pub fn service_state(name: &str) -> ServiceState {
    // SAFETY: the handles are closed.
    unsafe {
        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else { return ServiceState::Missing };
        let state = match OpenServiceW(scm, &HSTRING::from(name), SERVICE_QUERY_STATUS) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let state = match QueryServiceStatus(service, &mut status) {
                    Ok(()) if status.dwCurrentState == SERVICE_STOPPED => ServiceState::Stopped,
                    Ok(()) if status.dwCurrentState == SERVICE_RUNNING => ServiceState::Running,
                    Ok(()) => ServiceState::Other,
                    Err(_) => ServiceState::Other,
                };
                let _ = CloseServiceHandle(service);
                state
            }
            Err(_) => ServiceState::Missing,
        };
        let _ = CloseServiceHandle(scm);
        state
    }
}

/// Stops a service and waits (up to `timeout`) until it has stopped.
pub fn stop_service(name: &str, timeout: Duration) -> io::Result<()> {
    // SAFETY: the handles are closed.
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT).map_err(io::Error::other)?;
        let service = match OpenServiceW(scm, &HSTRING::from(name), SERVICE_QUERY_STATUS | SERVICE_STOP) {
            Ok(s) => s,
            Err(e) => {
                let _ = CloseServiceHandle(scm);
                return Err(io::Error::other(e));
            }
        };
        let mut status = SERVICE_STATUS::default();
        let _ = ControlService(service, SERVICE_CONTROL_STOP, &mut status);
        let started = Instant::now();
        let mut result = Err(io::Error::other(format!("the service \"{name}\" didn't stop within {} s", timeout.as_secs())));
        while started.elapsed() < timeout {
            if QueryServiceStatus(service, &mut status).is_ok() && status.dwCurrentState == SERVICE_STOPPED {
                result = Ok(());
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
        result
    }
}

// ---- The registry (the product's hive, 64-bit view unless said) ----

/// HKLM for the host, which is installed for the whole PC; HKCU for the viewer, which is the
/// user's own.
fn hive() -> HKEY {
    if product().per_machine() { HKEY_LOCAL_MACHINE } else { HKEY_CURRENT_USER }
}

/// The hive's short name, for what is logged and said.
pub fn hive_name() -> &'static str {
    if product().per_machine() { "HKLM" } else { "HKCU" }
}

pub struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: ours to close.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn win32(code: windows::Win32::Foundation::WIN32_ERROR, what: &str) -> io::Result<()> {
    if code.is_ok() { Ok(()) } else { Err(io::Error::other(format!("{what}: {}", windows::core::Error::from(code.to_hresult()).message()))) }
}

pub fn open_key(path: &str, view: REG_SAM_FLAGS) -> Option<Key> {
    let mut key = HKEY::default();
    // SAFETY: the key is closed by Key.
    let code = unsafe { RegOpenKeyExW(hive(), &HSTRING::from(path), None, KEY_READ | view, &mut key) };
    code.is_ok().then_some(Key(key))
}

pub fn create_key(path: &str) -> io::Result<Key> {
    let mut key = HKEY::default();
    // SAFETY: the key is closed by Key.
    let code = unsafe { RegCreateKeyExW(hive(), &HSTRING::from(path), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY, None, &mut key, None) };
    win32(code, &format!(r"{}\{path}", hive_name()))?;
    Ok(Key(key))
}

pub fn delete_key(path: &str, view: REG_SAM_FLAGS) -> io::Result<()> {
    let Some(parent_path) = path.rsplit_once('\\').map(|(p, _)| p) else { return Ok(()) };
    let name = &path[parent_path.len() + 1..];
    let mut parent = HKEY::default();
    // SAFETY: the key is closed.
    unsafe {
        let code = RegOpenKeyExW(hive(), &HSTRING::from(parent_path), None, KEY_READ | KEY_WRITE | view, &mut parent);
        win32(code, &format!(r"{}\{parent_path}", hive_name()))?;
        let parent = Key(parent);
        let code = RegDeleteTreeW(parent.0, &HSTRING::from(name));
        if code == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        win32(code, &format!(r"{}\{path}", hive_name()))?;
        let code = windows::Win32::System::Registry::RegDeleteKeyExW(parent.0, &HSTRING::from(name), view.0, None);
        if code != windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            win32(code, &format!(r"{}\{path}", hive_name()))?;
        }
    }
    Ok(())
}

pub const VIEW64: REG_SAM_FLAGS = KEY_WOW64_64KEY;
pub const VIEW32: REG_SAM_FLAGS = KEY_WOW64_32KEY;

impl Key {
    pub fn set_str(&self, name: &str, value: &str) -> io::Result<()> {
        let data: Vec<u8> = wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
        // SAFETY: the data is the length given.
        win32(unsafe { RegSetValueExW(self.0, &HSTRING::from(name), None, REG_SZ, Some(&data)) }, name)
    }

    pub fn set_dword(&self, name: &str, value: u32) -> io::Result<()> {
        // SAFETY: the data is the length given.
        win32(unsafe { RegSetValueExW(self.0, &HSTRING::from(name), None, REG_DWORD, Some(&value.to_le_bytes())) }, name)
    }

    pub fn get_str(&self, name: &str) -> Option<String> {
        let name = HSTRING::from(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = 0u32;
        // SAFETY: the buffer is the size Windows asked for.
        unsafe {
            if RegQueryValueExW(self.0, &name, None, Some(&mut kind), None, Some(&mut size)).is_err() || size == 0 {
                return None;
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            if RegQueryValueExW(self.0, &name, None, Some(&mut kind), Some(buffer.as_mut_ptr() as *mut u8), Some(&mut size)).is_err() {
                return None;
            }
            let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            Some(String::from_utf16_lossy(&buffer[..len]))
        }
    }

    pub fn delete_value(&self, name: &str) -> io::Result<()> {
        // SAFETY: plain call.
        let code = unsafe { RegDeleteValueW(self.0, &HSTRING::from(name)) };
        if code == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        win32(code, name)
    }
}

pub fn open_key_writable(path: &str) -> Option<Key> {
    let mut key = HKEY::default();
    // SAFETY: the key is closed by Key.
    let code = unsafe { RegOpenKeyExW(hive(), &HSTRING::from(path), None, KEY_READ | KEY_WRITE | KEY_WOW64_64KEY, &mut key) };
    code.is_ok().then_some(Key(key))
}

// ---- Files ----

/// What could not go at once and goes when Windows restarts.
#[derive(Default)]
pub struct Removed {
    pub files: usize,
    pub at_restart: Vec<PathBuf>,
}

/// Deletes `dir` and everything in it. A file in use (a program running from it) is moved out
/// of the folder into `trash` (on the same drive) if it can be, and either way deleted when
/// Windows restarts; so is a folder that can't go now.
pub fn remove_dir(dir: &Path, trash: Option<&Path>) -> io::Result<Removed> {
    let mut removed = Removed::default();
    if !dir.exists() {
        return Ok(removed);
    }
    let mut dirs = vec![dir.to_path_buf()];
    let mut files = Vec::new();
    let mut i = 0;
    while i < dirs.len() {
        for entry in std::fs::read_dir(&dirs[i])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
        i += 1;
    }
    for file in files {
        if let Ok(meta) = std::fs::metadata(&file) {
            let mut permissions = meta.permissions();
            if permissions.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = std::fs::set_permissions(&file, permissions);
            }
        }
        if std::fs::remove_file(&file).is_ok() {
            removed.files += 1;
            continue;
        }
        // A running program's file can't be deleted, but it can be moved.
        let mut moved = None;
        if let Some(trash) = trash {
            let to = trash.join(format!("{}-{}", removed.at_restart.len(), file.file_name().unwrap_or_default().to_string_lossy()));
            // SAFETY: plain call.
            if unsafe { MoveFileExW(&HSTRING::from(file.as_os_str()), &HSTRING::from(to.as_os_str()), MOVEFILE_REPLACE_EXISTING) }.is_ok() {
                moved = Some(to);
            }
        }
        match moved {
            // Out of the folder already: deleted at restart if Windows takes it (it needs an
            // administrator), or left in Temp.
            Some(to) => {
                let _ = delete_at_restart(&to);
                removed.at_restart.push(to);
            }
            None => {
                delete_at_restart(&file).map_err(|e| io::Error::other(format!("{}: {e}", file.display())))?;
                removed.at_restart.push(file);
            }
        }
    }
    for d in dirs.iter().rev() {
        if std::fs::remove_dir(d).is_err() {
            delete_at_restart(d).map_err(|e| io::Error::other(format!("{}: {e}", d.display())))?;
            removed.at_restart.push(d.clone());
        }
    }
    Ok(removed)
}

pub fn delete_at_restart(path: &Path) -> io::Result<()> {
    // SAFETY: plain call.
    unsafe { MoveFileExW(&HSTRING::from(path.as_os_str()), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT) }.map_err(io::Error::other)
}

/// After this process has ended, `dir` is deleted (by a hidden cmd.exe that waits a moment), and
/// failing that when Windows restarts.
pub fn delete_after_exit(dir: &Path) {
    let line = format!("/d /c \"ping -n 4 127.0.0.1 >nul & rd /s /q \"{}\"\"", dir.display());
    let _ = std::process::Command::new(windows_dir().join(r"System32\cmd.exe")).raw_arg(line).current_dir(windows_dir()).creation_flags(CREATE_NO_WINDOW.0).spawn();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let _ = delete_at_restart(&entry.path());
        }
    }
    let _ = delete_at_restart(dir);
}

/// A shortcut (`.lnk`) to `target`.
pub fn make_shortcut(link: &Path, target: &Path, description: &str) -> io::Result<()> {
    // SAFETY: COM on this thread for the call, undone after.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> windows::core::Result<()> {
            let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            shell.SetPath(&HSTRING::from(target.as_os_str()))?;
            if let Some(dir) = target.parent() {
                shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            shell.SetDescription(&HSTRING::from(description))?;
            shell.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
            shell.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        result.map_err(io::Error::other)
    }
}
