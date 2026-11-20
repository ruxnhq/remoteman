//! Where a host started by the service acts: as SYSTEM, in the console's session, on whichever
//! desktop takes input at the moment, and for whoever is signed in there.
//!
//! The sign-in screen, the lock screen and Windows' own permission prompts are on the session's
//! `Winlogon` desktop; everything else is on `Default`. `SendInput`, the cursor's place and
//! display changes act on the calling thread's desktop, and only that desktop's input reaches
//! anything, so each thread that does them first moves to the input desktop ([`follow`]). Only
//! a SYSTEM process may open `Winlogon`: a host run any other way stays on its own desktop, as
//! it always did. A thread can't move while it has windows or hooks on its desktop; the
//! threads that call [`follow`] have none.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW, HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::PWSTR;

thread_local! {
    /// The desktop this thread was last moved to, by name, with its handle (kept open while the
    /// thread is on it).
    static ON: RefCell<Option<(String, HDESK)>> = const { RefCell::new(None) };
}

/// `GENERIC_ALL` for a desktop: what reading, writing and switching to it need.
const ALL: u32 = 0x1000_0000;

fn name_of(desk: HDESK) -> Option<String> {
    let mut buffer = [0u16; 64];
    let mut needed = 0u32;
    // SAFETY: the buffer is ours and its size in bytes is said.
    unsafe { GetUserObjectInformationW(HANDLE(desk.0), UOI_NAME, Some(buffer.as_mut_ptr() as *mut c_void), (buffer.len() * 2) as u32, Some(&mut needed)) }.ok()?;
    Some(String::from_utf16_lossy(&buffer[..buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len())]))
}

/// Move this thread to the desktop that takes input now, if it isn't there already. Says the
/// desktop's name (`Default`, `Winlogon`), or `None` where the input desktop can't be opened
/// (any process but SYSTEM, at the sign-in screen), in which case the thread stays where it is.
pub fn follow() -> Option<String> {
    // SAFETY: plain call; the handle is closed below or kept in `ON`.
    let input = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(ALL)) }.ok()?;
    let Some(name) = name_of(input) else {
        // SAFETY: ours.
        let _ = unsafe { CloseDesktop(input) };
        return None;
    };
    ON.with(|on| {
        let mut on = on.borrow_mut();
        if on.as_ref().is_some_and(|(was, _)| *was == name) {
            // SAFETY: ours; the thread keeps the one it is on.
            let _ = unsafe { CloseDesktop(input) };
            return Some(name);
        }
        // SAFETY: a desktop handle with the rights it needs.
        match unsafe { SetThreadDesktop(input) } {
            Ok(()) => {
                let thread = std::thread::current().name().unwrap_or("?").to_string();
                match on.replace((name.clone(), input)) {
                    Some((was, old)) => {
                        tracing::info!(thread, from = was, to = name, "a thread moved to the input desktop");
                        // SAFETY: the thread is no longer on it.
                        let _ = unsafe { CloseDesktop(old) };
                    }
                    None if name != "Default" => tracing::info!(thread, to = name, "a thread moved to the input desktop"),
                    None => {}
                }
                Some(name)
            }
            Err(e) => {
                tracing::info!(thread = std::thread::current().name().unwrap_or("?"), desktop = name, "a thread couldn't move to the input desktop: {e}");
                // SAFETY: ours.
                let _ = unsafe { CloseDesktop(input) };
                None
            }
        }
    })
}

/// The SID, as text, of a token's user.
fn sid_of(token: HANDLE) -> Option<String> {
    // SAFETY: the buffer outlives the reads of it; the text is LocalAlloc'd and freed here.
    unsafe {
        let mut size = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
        let mut buffer = vec![0u8; size as usize];
        GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr() as *mut c_void), size, &mut size).ok()?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut c_void)));
        sid
    }
}

/// The SID, as text, of the user this process runs as.
pub fn own_sid() -> Option<String> {
    // SAFETY: the token is ours and closed here.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let sid = sid_of(token);
        let _ = CloseHandle(token);
        sid
    }
}

/// Is this process SYSTEM (started by the service)?
pub fn is_system() -> bool {
    own_sid().as_deref() == Some("S-1-5-18")
}

/// The token of the user signed in at the console, if anyone is (SYSTEM only). Closed on drop.
pub struct UserToken(pub HANDLE);

impl Drop for UserToken {
    fn drop(&mut self) {
        // SAFETY: ours.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub fn console_user() -> Option<UserToken> {
    // SAFETY: plain calls; the token is handed to the caller, who closes it.
    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        if session == u32::MAX {
            return None;
        }
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token).ok()?;
        Some(UserToken(token))
    }
}

/// The SID, as text, of the user signed in at the console (SYSTEM only).
pub fn console_user_sid() -> Option<String> {
    console_user().and_then(|t| sid_of(t.0))
}

/// Is this session at its lock screen, or (for a host the service started) at the sign-in
/// screen with nobody signed in? Windows draws both on the main display only. (Not the input
/// desktop: Windows puts the lock screen's picture on the user's own desktop, and moves to
/// `Winlogon` only while a password is typed.)
pub fn locked() -> bool {
    use windows::Win32::System::RemoteDesktop::{WTS_CURRENT_SESSION, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx};
    if is_system_cached() && console_user().is_none() {
        return true;
    }
    // SAFETY: the buffer is Windows' and freed here; read as the type it is for this class.
    unsafe {
        let mut buffer = PWSTR::null();
        let mut bytes = 0u32;
        if WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, WTSSessionInfoEx, &mut buffer, &mut bytes).is_err() || buffer.is_null() {
            return false;
        }
        let info = &*(buffer.0 as *const WTSINFOEXW);
        // WTS_SESSIONSTATE_LOCK is 0, UNLOCK 1, UNKNOWN 0xFFFFFFFF.
        let locked = info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags == 0;
        WTSFreeMemory(buffer.0 as *mut c_void);
        locked
    }
}

/// The name of the user of this session (signed in, or last signed in at its lock screen).
pub fn user_name() -> Option<String> {
    use windows::Win32::System::RemoteDesktop::{WTS_CURRENT_SESSION, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName};
    // SAFETY: the buffer is Windows' text and freed here.
    unsafe {
        let mut buffer = PWSTR::null();
        let mut bytes = 0u32;
        WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, WTSUserName, &mut buffer, &mut bytes).ok()?;
        let name = buffer.to_string().ok();
        WTSFreeMemory(buffer.0 as *mut c_void);
        name.filter(|n| !n.is_empty())
    }
}

/// Lock this session (after a device that unlocked it has gone).
pub fn lock_now() -> bool {
    follow();
    // SAFETY: plain call.
    unsafe { windows::Win32::System::Shutdown::LockWorkStation() }.is_ok()
}

/// Type a password into Windows' lock or sign-in screen, as its owner would at the keyboard:
/// wake the screen (Ctrl, which types nothing), wait for the password box, clear it, type,
/// Enter, and wait to see whether Windows unlocked. `Ok(false)`: still locked (a wrong
/// password; Windows' "incorrect" message is dismissed again). Only a host the service started
/// can reach that screen. The password is never logged.
pub fn unlock_with(password: &str) -> anyhow::Result<bool> {
    use std::time::{Duration, Instant};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_RETURN,
    };
    anyhow::ensure!(is_system_cached(), "only the service's host can reach the lock screen");
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT { r#type: INPUT_KEYBOARD, Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } } };
    let send = |inputs: &[INPUT]| {
        follow();
        // SAFETY: whole INPUTs and their size.
        (unsafe { SendInput(inputs, size_of::<INPUT>() as i32) }) as usize == inputs.len()
    };
    // Wake the lock screen; the password box comes on the Winlogon desktop.
    let started = Instant::now();
    while follow().as_deref() != Some("Winlogon") && started.elapsed() < Duration::from_secs(5) {
        send(&[key(VK_CONTROL, false), key(VK_CONTROL, true)]);
        std::thread::sleep(Duration::from_millis(400));
    }
    anyhow::ensure!(follow().as_deref() == Some("Winlogon"), "Windows' password box didn't come up");
    std::thread::sleep(Duration::from_millis(600));
    // Whatever is in the box already goes (Ctrl+A, Backspace), then the password and Enter.
    let a = VIRTUAL_KEY(b'A' as u16);
    send(&[key(VK_CONTROL, false), key(a, false), key(a, true), key(VK_CONTROL, true), key(VK_BACK, false), key(VK_BACK, true)]);
    let unit = |c: u16, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wScan: c, dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) }, ..Default::default() } },
    };
    // One character at a time: in one batch Windows' text controls lose all but the first word
    // (see `Event::Text` in input.rs).
    for c in password.encode_utf16() {
        anyhow::ensure!(send(&[unit(c, false), unit(c, true)]), "Windows didn't take the typing");
        std::thread::sleep(Duration::from_millis(15));
    }
    send(&[key(VK_RETURN, false), key(VK_RETURN, true)]);
    let typed_at = Instant::now();
    while typed_at.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(250));
        if !locked() {
            return Ok(true);
        }
    }
    // Wrong: Windows says so with an OK button; Enter takes it away again.
    send(&[key(VK_RETURN, false), key(VK_RETURN, true)]);
    Ok(false)
}

fn is_system_cached() -> bool {
    static IS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS.get_or_init(is_system)
}

/// The event the service waits on to send Windows Ctrl+Alt+Del (`SendSAS`), which a service
/// may and this host may not.
pub const SAS_EVENT: &str = r"Global\remoteman-host-sas";

/// The event the service sets when this host is to stop.
pub const STOP_EVENT: &str = r"Global\remoteman-host-stop";

/// Ask the service for Ctrl+Alt+Del. False where there is no service to ask.
pub fn secure_attention() -> bool {
    use windows::Win32::System::Threading::{EVENT_MODIFY_STATE, OpenEventW, SetEvent};
    // SAFETY: plain calls on a handle that is ours and closed here.
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, &windows::core::HSTRING::from(SAS_EVENT)) {
            Ok(event) => {
                let set = SetEvent(event).is_ok();
                let _ = CloseHandle(event);
                set
            }
            Err(_) => false,
        }
    }
}
