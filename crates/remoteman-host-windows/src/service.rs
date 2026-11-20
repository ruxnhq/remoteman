//! `remoteman-host service [HOST OPTIONS]`: the Windows service that keeps the host running from
//! boot, before anyone signs in, across signing out and switching users, and at the lock screen.
//!
//! A service runs as SYSTEM in session 0, which has no desktop: nothing there can be captured,
//! clicked or typed into. So the service doesn't host; it keeps one host running in the
//! session at the console, as SYSTEM (its own token, moved to that session), started with the
//! service's own options after the word `service`, and with no window. That host follows the
//! input desktop (`crate::desk`), so it reaches the sign-in screen, the lock screen and
//! Windows' permission prompts, which no program run by the user can. When the console moves to
//! another session (signing out, switching users, a remote desktop taking it) the host there
//! is stopped and one is started in the new one; a host that ends is started again, sooner
//! the first time, later if it keeps ending.
//!
//! The service does one thing the host can't: Ctrl+Alt+Del (`SendSAS`), which Windows takes
//! from a service only, and only where the policy `SoftwareSASGeneration` allows services (the
//! installer sets it). The host asks for it through an event ([`crate::desk::SAS_EVENT`]).

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_CALL_NOT_IMPLEMENTED, HANDLE, NO_ERROR, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary,
    TokenSessionId,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken, PROCESS_INFORMATION, ResetEvent, STARTUPINFOW, SetEvent,
    TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

/// The service's name with the service manager (the installer registers it so).
pub const NAME: &str = "remoteman host";

static STOP: AtomicBool = AtomicBool::new(false);
static STATUS: OnceLock<SendHandle> = OnceLock::new();
/// Woken on stop and on every session change.
static WAKE: OnceLock<SendHandle> = OnceLock::new();
/// The host's options, after the word `service`.
static ARGS: OnceLock<Vec<String>> = OnceLock::new();

#[derive(Clone, Copy)]
struct SendHandle(isize);
// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// Run as the service. Returns when the service manager has stopped it; fails at once if this
/// process wasn't started by the service manager.
pub fn run(args: Vec<String>) -> Result<()> {
    let _ = ARGS.set(args);
    let name = HSTRING::from(NAME);
    let table = [SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR(name.as_ptr() as *mut u16), lpServiceProc: Some(service_main) }, SERVICE_TABLE_ENTRYW::default()];
    // SAFETY: a table ending in an empty entry; the name outlives the dispatcher.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.context("this is the service's way in; run it from the service manager (or without the word `service` to host here)")?;
    Ok(())
}

fn report(state: SERVICE_STATUS_CURRENT_STATE, exit: u32) {
    let Some(handle) = STATUS.get() else { return };
    let accepted = if state == SERVICE_RUNNING { SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE } else { 0 };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: exit,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED { 0 } else { 10_000 },
        ..Default::default()
    };
    // SAFETY: the handle the service manager gave; a whole status.
    let _ = unsafe { SetServiceStatus(SERVICE_STATUS_HANDLE(handle.0 as *mut c_void), &status) };
}

unsafe extern "system" fn handler(control: u32, _kind: u32, _data: *mut c_void, _context: *mut c_void) -> u32 {
    match control {
        c if c == SERVICE_CONTROL_STOP || c == SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::Relaxed);
            report(SERVICE_STOP_PENDING, 0);
            wake();
            NO_ERROR.0
        }
        c if c == SERVICE_CONTROL_SESSIONCHANGE => {
            wake();
            NO_ERROR.0
        }
        c if c == SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

fn wake() {
    if let Some(event) = WAKE.get() {
        // SAFETY: an event of this process's.
        let _ = unsafe { SetEvent(HANDLE(event.0 as *mut c_void)) };
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    // SAFETY: the name is static text; the handler lives for the process.
    let Ok(status) = (unsafe { RegisterServiceCtrlHandlerExW(&HSTRING::from(NAME), Some(handler), None) }) else { return };
    let _ = STATUS.set(SendHandle(status.0 as isize));
    report(SERVICE_START_PENDING, 0);
    let result = supervise();
    if let Err(e) = &result {
        tracing::error!("the service stops: {e:#}");
    }
    report(SERVICE_STOPPED, if result.is_ok() { 0 } else { 1 });
}

/// An event of the service's, named in the global namespace so that the host in the console's
/// session finds it. Made with the service's own (SYSTEM's) default protection.
fn event(name: &str, manual: bool) -> Result<HANDLE> {
    // SAFETY: plain call.
    unsafe { CreateEventW(None, manual, false, &HSTRING::from(name)) }.with_context(|| format!("making the event {name}"))
}

/// The host the service started, in the session it was started in.
struct Started {
    process: HANDLE,
    pid: u32,
    session: u32,
    at: Instant,
}

/// Start the host as SYSTEM in this session, with no window.
fn start(session: u32) -> Result<Started> {
    let exe = std::env::current_exe().context("where this program is")?;
    let quote = |a: &str| if a.is_empty() || a.contains([' ', '\t', '"']) { format!("\"{}\"", a.replace('"', "\\\"")) } else { a.to_string() };
    let args = ARGS.get().cloned().unwrap_or_default();
    let line = std::iter::once(quote(&exe.to_string_lossy())).chain(args.iter().map(|a| quote(a))).chain(["--stop-event".into(), quote(crate::desk::STOP_EVENT)]).collect::<Vec<_>>().join(" ");
    let mut line: Vec<u16> = line.encode_utf16().chain([0]).collect();
    let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
    let folder = exe.parent().map(|p| HSTRING::from(p.as_os_str()));
    // SAFETY: the tokens are this function's and closed here; the strings outlive the calls;
    // the process and thread handles come back to the caller (the thread's closed here).
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT | TOKEN_ADJUST_SESSIONID, &mut own).context("the service's own token")?;
        let mut token = HANDLE::default();
        let duplicated = DuplicateTokenEx(own, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut token);
        let _ = CloseHandle(own);
        duplicated.context("a copy of the service's token")?;
        let moved = SetTokenInformation(token, TokenSessionId, &session as *const u32 as *const c_void, size_of::<u32>() as u32);
        if let Err(e) = moved {
            let _ = CloseHandle(token);
            bail!("moving the token to session {session}: {e}");
        }
        let start = STARTUPINFOW { cb: size_of::<STARTUPINFOW>() as u32, lpDesktop: PWSTR(desktop.as_mut_ptr()), ..Default::default() };
        let mut info = PROCESS_INFORMATION::default();
        let made = CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            None,
            folder.as_ref().map_or(PCWSTR::null(), |f| PCWSTR(f.as_ptr())),
            &start,
            &mut info,
        );
        let _ = CloseHandle(token);
        made.with_context(|| format!("starting the host in session {session}"))?;
        let _ = CloseHandle(info.hThread);
        Ok(Started { process: info.hProcess, pid: info.dwProcessId, session, at: Instant::now() })
    }
}

/// Ask the host to stop (the stop event), and end it if it hasn't in five seconds.
fn stop(host: Started, stop_event: HANDLE) {
    // SAFETY: handles of this process's; the process handle is closed here.
    unsafe {
        let _ = SetEvent(stop_event);
        if WaitForSingleObject(host.process, 5000) != WAIT_OBJECT_0 {
            tracing::warn!(pid = host.pid, "the host didn't stop when asked: ended");
            let _ = TerminateProcess(host.process, 1);
            let _ = WaitForSingleObject(host.process, 2000);
        }
        let _ = CloseHandle(host.process);
        let _ = ResetEvent(stop_event);
    }
}

/// Ctrl+Alt+Del, through `sas.dll`'s `SendSAS` (looked up at run time: it isn't in every edition).
fn send_sas() {
    type SendSas = unsafe extern "system" fn(i32);
    // SAFETY: the library stays loaded for the process; the function has this signature.
    unsafe {
        let Ok(library) = LoadLibraryW(w!("sas.dll")) else {
            tracing::warn!("no sas.dll: Ctrl+Alt+Del can't be sent");
            return;
        };
        match GetProcAddress(library, windows::core::s!("SendSAS")) {
            Some(f) => {
                let send: SendSas = std::mem::transmute(f);
                send(0);
                tracing::info!("Ctrl+Alt+Del sent (Windows takes it only where SoftwareSASGeneration allows services)");
            }
            None => tracing::warn!("sas.dll has no SendSAS"),
        }
    }
}

fn supervise() -> Result<()> {
    let wake_event = event(r"Global\remoteman-host-service-wake", false)?;
    let _ = WAKE.set(SendHandle(wake_event.0 as isize));
    let stop_event = event(crate::desk::STOP_EVENT, true)?;
    let sas_event = event(crate::desk::SAS_EVENT, false)?;
    report(SERVICE_RUNNING, 0);
    tracing::info!(args = ?ARGS.get(), "the service runs: it keeps a host in the console's session");

    let mut host: Option<Started> = None;
    // When to start a host again, and how long to wait the next time one ends early.
    let (mut not_before, mut backoff) = (Instant::now(), Duration::from_secs(2));
    while !STOP.load(Ordering::Relaxed) {
        // SAFETY: plain call.
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        if let Some(h) = &host {
            let mut code = 0u32;
            // SAFETY: a live process handle of ours.
            let ended = unsafe { WaitForSingleObject(h.process, 0) } == WAIT_OBJECT_0;
            if ended {
                let _ = unsafe { GetExitCodeProcess(h.process, &mut code) };
                let ran = h.at.elapsed();
                tracing::warn!(pid = h.pid, code = format!("{code:#x}"), ran_s = ran.as_secs(), "the host ended: started again in {} s", backoff.as_secs());
                // SAFETY: ours.
                let _ = unsafe { CloseHandle(h.process) };
                host = None;
                not_before = Instant::now() + backoff;
                backoff = if ran > Duration::from_secs(60) { Duration::from_secs(2) } else { (backoff * 2).min(Duration::from_secs(60)) };
            } else if console != u32::MAX && h.session != console {
                // (No session at the console for a moment, as while it is being handed over:
                // the host stays until there is another.)
                tracing::info!(from = h.session, to = console, "the console moved to another session: the host moves with it");
                if let Some(h) = host.take() {
                    stop(h, stop_event);
                }
                not_before = Instant::now();
            }
        }
        if host.is_none() && console != u32::MAX && Instant::now() >= not_before {
            match start(console) {
                Ok(h) => {
                    tracing::info!(pid = h.pid, session = console, "the host started in the console's session");
                    host = Some(h);
                }
                Err(e) => {
                    tracing::warn!("couldn't start the host: {e:#}; trying again in {} s", backoff.as_secs());
                    not_before = Instant::now() + backoff;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
        let mut waits = vec![wake_event, sas_event];
        if let Some(h) = &host {
            waits.push(h.process);
        }
        // SAFETY: live handles; one second at most, so that a session change missed is seen.
        let woke = unsafe { WaitForMultipleObjects(&waits, false, 1000) };
        if woke.0 == WAIT_OBJECT_0.0 + 1 {
            send_sas();
        }
    }
    tracing::info!("the service is stopping: the host too");
    if let Some(h) = host.take() {
        stop(h, stop_event);
    }
    Ok(())
}
