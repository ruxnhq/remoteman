//! Between the UI thread and everything that must not block it. The host is asked on a thread
//! of its own (`status` once a second, and whatever the owner chose); its answers come back to
//! the UI thread through a window nobody sees, whose procedure hands them to the application as
//! messages. Reactor's applications have no timers or background work of their own (its
//! components do, but the window comes and goes while the app lives on).
//!
//! The same window is how a second start of the app reaches the first: it finds the window by
//! its class and the port in its title, and sends it which page to open.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, PostMessageW, RegisterClassW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_COPYDATA, WNDCLASSW,
};
use windows::core::{HSTRING, PCWSTR, w};
use windows_reactor::LocalSender;

use crate::app::{AppMessage, Page};
use crate::host::{self, Status, Unreached};

const CLASS: PCWSTR = w!("remoteman-host-app");
/// "Answers are waiting": posted by the asking thread.
const WM_ANSWERS: u32 = WM_APP + 1;
/// WM_COPYDATA's tag for "open the window at this page" from a second start.
const OPEN: usize = 0x524D_4F50;
/// How often the host is asked how it stands.
const EVERY: Duration = Duration::from_secs(1);

/// What the owner wants the host told.
pub enum Job {
    Tell(String),
}

/// What came back.
#[derive(Clone)]
pub enum Answer {
    Status(Box<Result<Status, Unreached>>),
    Told { words: String, said: Result<String, Unreached> },
}

static ANSWERS: Mutex<VecDeque<Answer>> = Mutex::new(VecDeque::new());
static WINDOW: AtomicIsize = AtomicIsize::new(0);

thread_local! {
    /// Where the hidden window's procedure hands things on: the application, while it runs.
    static APP: RefCell<Option<LocalSender<AppMessage>>> = const { RefCell::new(None) };
}

fn title(port: u16) -> HSTRING {
    HSTRING::from(format!("remoteman host app, port {port}"))
}

/// Make the hidden window on the UI thread and start asking the host. Returns where to send
/// jobs for the host.
pub fn start(port: u16, app: LocalSender<AppMessage>) -> Sender<Job> {
    APP.with(|a| *a.borrow_mut() = Some(app));
    // SAFETY: a window class and a message-only window of this module, on this thread.
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSW { lpfnWndProc: Some(procedure), hInstance: instance.into(), lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&class);
        match CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, &title(port), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None) {
            Ok(hwnd) => WINDOW.store(hwnd.0 as isize, Ordering::SeqCst),
            Err(e) => crate::log::say(&format!("no hidden window ({e}): the host's answers won't reach the app")),
        }
    }
    let (jobs, inbox) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new().name("host".into()).spawn(move || {
        let mut next = Instant::now() + EVERY;
        loop {
            match inbox.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(Job::Tell(words)) => {
                    let said = host::ask(port, &words);
                    deliver(Answer::Told { words, said });
                    // What it changed, straight away.
                    deliver(Answer::Status(Box::new(host::read(port))));
                    next = Instant::now() + EVERY;
                }
                Err(RecvTimeoutError::Timeout) => {
                    deliver(Answer::Status(Box::new(host::read(port))));
                    next = Instant::now() + EVERY;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    if let Err(e) = spawned {
        crate::log::say(&format!("couldn't start asking the host: {e}"));
    }
    jobs
}

fn deliver(answer: Answer) {
    ANSWERS.lock().unwrap().push_back(answer);
    let hwnd = WINDOW.load(Ordering::SeqCst);
    if hwnd != 0 {
        // SAFETY: plain call; the window lives as long as the app.
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_ANSWERS, WPARAM(0), LPARAM(0)) };
    }
}

fn to_app(message: AppMessage) {
    APP.with(|a| {
        if let Some(app) = a.borrow().as_ref() {
            let _ = app.send(message);
        }
    });
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_ANSWERS => {
            let answers: Vec<Answer> = ANSWERS.lock().unwrap().drain(..).collect();
            for answer in answers {
                to_app(AppMessage::Host(answer));
            }
            LRESULT(0)
        }
        WM_COPYDATA => {
            // SAFETY: WM_COPYDATA's lParam is a COPYDATASTRUCT valid for this call.
            let data = unsafe { &*(lparam.0 as *const COPYDATASTRUCT) };
            if data.dwData != OPEN {
                return LRESULT(0);
            }
            // SAFETY: as above; cbData bytes at lpData.
            let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
            let page = Page::from_tag(&String::from_utf8_lossy(bytes));
            crate::log::say(&format!("another start asked for the window ({})", page.map(Page::tag).unwrap_or("as it was")));
            to_app(AppMessage::Open(page));
            LRESULT(1)
        }
        // SAFETY: the default for everything else.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

/// One app a user and port. `true`: this is the first, go on. `false`: another is running and
/// was asked to open its window at `page` (unless this one was started in the background, as at
/// sign-in, which asks for nothing).
pub fn first(port: u16, page: Option<Page>, background: bool) -> bool {
    let name = HSTRING::from(format!("Local\\remoteman-host-app-{port}"));
    // SAFETY: plain call; the mutex is kept (never closed) for as long as this process lives.
    let mutex = unsafe { CreateMutexW(None, false, &name) };
    // SAFETY: plain call, straight after the one it reports on.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if mutex.is_err() || !existed {
        return true;
    }
    if background {
        crate::log::say("another Remoteman Host is running for this user: this one ends");
        return false;
    }
    // The first may still be starting: give its window a moment to appear.
    for _ in 0..50 {
        // SAFETY: plain calls; the data outlives SendMessageTimeoutW.
        unsafe {
            if let Ok(other) = FindWindowExW(Some(HWND_MESSAGE), None, CLASS, &title(port)) {
                let mut pid = 0u32;
                GetWindowThreadProcessId(other, Some(&mut pid));
                // So the first may bring its window to the front.
                let _ = AllowSetForegroundWindow(pid);
                let tag = page.map(Page::tag).unwrap_or("").as_bytes().to_vec();
                let data = COPYDATASTRUCT { dwData: OPEN, cbData: tag.len() as u32, lpData: tag.as_ptr() as *mut _ };
                SendMessageTimeoutW(other, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG, 3000, None);
                crate::log::say("another Remoteman Host is running for this user: asked it to show its window");
                return false;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    crate::log::say("another Remoteman Host holds the lock but has no window: this one ends");
    false
}
