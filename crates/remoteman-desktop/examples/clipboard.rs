//! The clipboard test, with nothing real involved: a throwaway headless Mutter on a private
//! D-Bus session, this crate's clipboard on a session of it, and an ordinary GTK app
//! (`clipboard_helper.py`) that copies and pastes. Prints PASS or FAIL for each case and leaves
//! the live desktop and its clipboard alone.
//!
//!   cargo run --release -p remoteman-desktop --example clipboard
//!
//! It runs itself again inside `dbus-run-session`; that second run starts Mutter, kills it by
//! its process id at the end, and refuses to work on the bus it was started from. Like
//! `scripts/e2e.sh`, the session's screencast stream shows up as a node on the user's
//! PipeWire; nothing reads it. Needs `mutter`, `dbus-run-session`, `dbus-monitor` and Python
//! with GTK 4.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use remoteman_desktop::clipboard::{Changes, Clipboard, DEFAULT_CAP, TooLarge, text_type};
use remoteman_desktop::mutter::VirtualDisplay;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Set for the run inside the private D-Bus session.
const INNER: &str = "RM_CLIP_INNER";
/// The address of the bus the test was started from: the one never to touch.
const REAL_BUS: &str = "RM_CLIP_REAL_BUS";

const UTF8: &str = "text/plain;charset=utf-8";
const PNG: &str = "image/png";

/// An 8 by 8 picture.
const PICTURE: [u8; 191] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x06, 0x00, 0x00, 0x00, 0xc4, 0x0f, 0xbe,
    0x8b, 0x00, 0x00, 0x00, 0x86, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x15, 0xca, 0x41, 0x01, 0x02, 0x00, 0x08, 0x04, 0xc1, 0x8b, 0x42, 0x14, 0xa2, 0x10, 0x85, 0x06, 0x46, 0x90, 0x06, 0x46, 0x90,
    0x06, 0x46, 0x90, 0x06, 0x46, 0xc0, 0x65, 0xde, 0x23, 0x69, 0xd7, 0xf4, 0x5b, 0xd7, 0x77, 0x43, 0x9f, 0x4d, 0xbd, 0xb7, 0xf4, 0xda, 0xd6, 0x73, 0x47, 0x8f, 0x95, 0x8c, 0x60, 0x04, 0x23, 0x18,
    0xc1, 0x08, 0x46, 0x30, 0x82, 0x5d, 0x70, 0x82, 0x13, 0x9c, 0xe0, 0x04, 0x27, 0x38, 0xc1, 0x09, 0x7e, 0x21, 0x08, 0x41, 0x08, 0x42, 0x10, 0x82, 0x10, 0x84, 0x20, 0xc4, 0x85, 0x24, 0x24, 0x21,
    0x09, 0x49, 0x48, 0x42, 0x12, 0x92, 0x90, 0x17, 0x8a, 0x50, 0x84, 0x22, 0x14, 0xa1, 0x08, 0x45, 0x28, 0x42, 0x5d, 0x68, 0x42, 0x13, 0x9a, 0xd0, 0x84, 0x26, 0x34, 0xa1, 0x09, 0x7d, 0x61, 0x08,
    0x43, 0x18, 0xc2, 0x10, 0x86, 0x30, 0x84, 0x21, 0xcc, 0x63, 0xff, 0x6d, 0x47, 0xa9, 0x81, 0xf2, 0xc3, 0xcc, 0x7a, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn main() -> ExitCode {
    let result = if std::env::var_os(INNER).is_none() { outer() } else { inner() };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            println!("clipboard test could not run: {e:#}");
            ExitCode::from(2)
        }
    }
}

/// Run this program again on a D-Bus session of its own, with no display to fall back on.
fn outer() -> Result<bool> {
    let real = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let mut session = Command::new("dbus-run-session")
        .arg("--")
        .arg(std::env::current_exe()?)
        .env(INNER, "1")
        .env(REAL_BUS, real)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stderr(Stdio::piped())
        .spawn()
        .context("dbus-run-session")?;
    // The private bus's own chatter aside, what goes wrong is worth seeing.
    for line in BufReader::new(session.stderr.take().context("stderr")?).lines().map_while(Result::ok) {
        if !line.starts_with("dbus-daemon[") {
            eprintln!("{line}");
        }
    }
    Ok(session.wait()?.success())
}

/// A process of ours, ended by its process id when this goes.
struct Ours(Child);

impl Ours {
    fn pid(&self) -> u32 {
        self.0.id()
    }

    /// Ask it to go, wait, and insist if it hasn't.
    fn end(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = Command::new("kill").arg(self.pid().to_string()).status();
            let asked = Instant::now();
            while matches!(self.0.try_wait(), Ok(None)) && asked.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl Drop for Ours {
    fn drop(&mut self) {
        self.end();
    }
}

fn gone(pid: u32) -> &'static str {
    if Path::new(&format!("/proc/{pid}")).exists() { "STILL RUNNING" } else { "gone" }
}

fn inner() -> Result<bool> {
    let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").context("no D-Bus session of our own")?;
    let runtime_dir = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR")?);
    let users = format!("unix:path={}", runtime_dir.join("bus").display());
    ensure!(bus != std::env::var(REAL_BUS).unwrap_or_default() && !bus.starts_with(&users), "this is the user's own D-Bus session; not touching it");

    let display = format!("rm-clip-{}", std::process::id());
    let dir = std::env::temp_dir().join(&display);
    std::fs::create_dir_all(&dir)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build()?;
    // A bus where Mutter already answers is somebody's desktop.
    ensure!(!runtime.block_on(names_owned())?, "a Mutter is already on this D-Bus session; not touching it");

    let log = std::fs::File::create(dir.join("mutter.log"))?;
    let mut mutter = Ours(
        Command::new("mutter")
            .args(["--headless", "--wayland", "--no-x11", "--wayland-display", &display, "--virtual-monitor", "1280x720@60"])
            // No file-system daemon started for it on the private bus.
            .env("GIO_USE_VFS", "local")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("start mutter")?,
    );
    let passed = runtime.block_on(run(&dir, &runtime_dir.join(&display), &display));
    drop(runtime);
    let pid = mutter.pid();
    mutter.end();
    println!("mutter (process {pid}) is {}", gone(pid));
    // What a Mutter that had to be killed leaves behind.
    for left in [runtime_dir.join(&display), runtime_dir.join(format!("{display}.lock"))] {
        let _ = std::fs::remove_file(left);
    }
    match &passed {
        Ok(true) => std::fs::remove_dir_all(&dir)?,
        _ => println!("logs: {}", dir.display()),
    }
    passed
}

/// Whether Mutter's interfaces are on the bus.
async fn names_owned() -> Result<bool> {
    let conn = zbus::Connection::session().await?;
    let bus = zbus::fdo::DBusProxy::new(&conn).await?;
    for name in ["org.gnome.Mutter.RemoteDesktop", "org.gnome.Mutter.ScreenCast"] {
        if !bus.name_has_owner(name.try_into()?).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The app: `clipboard_helper.py`, told what to do a line at a time.
struct Helper {
    process: Ours,
    stdin: ChildStdin,
    answers: mpsc::UnboundedReceiver<String>,
    dir: PathBuf,
    files: u32,
}

impl Helper {
    async fn start(dir: &Path, display: &str) -> Result<Self> {
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/clipboard_helper.py");
        let mut child = Command::new("python3")
            .arg(script)
            .env("WAYLAND_DISPLAY", display)
            .env("GDK_BACKEND", "wayland")
            .env_remove("DISPLAY")
            // Nothing else started for it on the private bus: no portals, no accessibility
            // bus, no file-system daemon.
            .env("GDK_DEBUG", "no-portals")
            .env("GTK_A11Y", "none")
            .env("GIO_USE_VFS", "local")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(dir.join("helper.log"))?)
            .spawn()
            .context("start the helper app")?;
        let stdin = child.stdin.take().context("helper's input")?;
        let stdout = child.stdout.take().context("helper's output")?;
        let (tx, answers) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut helper = Self { process: Ours(child), stdin, answers, dir: dir.to_owned(), files: 0 };
        let first = helper.answer().await.context("the helper app's window never got the keyboard focus")?;
        ensure!(first == "ready", "the helper app said {first:?}");
        Ok(helper)
    }

    async fn answer(&mut self) -> Result<String> {
        timeout(Duration::from_secs(30), self.answers.recv()).await.context("the helper app didn't answer")?.context("the helper app has gone")
    }

    /// Give a command and wait for its "ok ...".
    async fn tell(&mut self, command: &str) -> Result<String> {
        writeln!(self.stdin, "{command}")?;
        self.stdin.flush()?;
        let answer = self.answer().await?;
        match answer.strip_prefix("ok") {
            Some(rest) => Ok(rest.trim().to_owned()),
            None => bail!("the helper app, told to {command:?}, said {answer:?}"),
        }
    }

    fn file(&mut self) -> PathBuf {
        self.files += 1;
        self.dir.join(format!("{}.bin", self.files))
    }

    /// Have the app copy text as an app's Copy does.
    async fn copy_text(&mut self, text: &str) -> Result<()> {
        let path = self.file();
        std::fs::write(&path, text)?;
        self.tell(&format!("copy-text {}", path.display())).await.map(drop)
    }

    /// Have the app offer these bytes as this type.
    async fn copy(&mut self, mime: &str, data: &[u8]) -> Result<()> {
        let path = self.file();
        std::fs::write(&path, data)?;
        self.tell(&format!("copy {mime} {}", path.display())).await.map(drop)
    }

    /// Have the app paste as text, as an app's Paste does.
    async fn paste_text(&mut self) -> Result<Vec<u8>> {
        let path = self.file();
        self.tell(&format!("paste-text {}", path.display())).await?;
        Ok(std::fs::read(path)?)
    }

    /// Have the app paste the bytes of this type.
    async fn paste(&mut self, mime: &str) -> Result<Vec<u8>> {
        let path = self.file();
        self.tell(&format!("paste {mime} {}", path.display())).await?;
        Ok(std::fs::read(path)?)
    }

    async fn formats(&mut self) -> Result<Vec<String>> {
        let answer = self.tell("formats").await?;
        Ok(answer.split(' ').skip(1).filter(|s| !s.is_empty()).map(str::to_owned).collect())
    }
}

/// The private bus watched from outside this crate, by `dbus-monitor`: how many times Mutter
/// has announced a clipboard owner. This tells "nothing was reported" from "nothing was sent".
struct Wire {
    process: Ours,
    announced: Arc<AtomicUsize>,
}

impl Wire {
    fn start() -> Result<Self> {
        let rule = "type='signal',interface='org.gnome.Mutter.RemoteDesktop.Session',member='SelectionOwnerChanged'";
        let mut child = Command::new("dbus-monitor").args(["--session", "--profile", rule]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("start dbus-monitor")?;
        let stdout = child.stdout.take().context("dbus-monitor's output")?;
        let announced = Arc::new(AtomicUsize::new(0));
        let count = announced.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.contains("SelectionOwnerChanged") {
                    count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        Ok(Self { process: Ours(child), announced })
    }

    /// The count, once the monitor has had time to print what it saw.
    async fn announced(&self) -> usize {
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.announced.load(Ordering::SeqCst)
    }
}

/// The next change of the desktop's clipboard that has this type.
async fn change_with(changes: &mut Changes, mime: &str) -> Result<Vec<String>> {
    let wait = async {
        loop {
            match changes.next().await {
                Some(types) if types.iter().any(|t| t == mime) => return Ok(types),
                Some(_) => continue,
                None => bail!("the session closed"),
            }
        }
    };
    timeout(Duration::from_secs(5), wait).await.with_context(|| format!("no change with {mime} reported"))?
}

/// Nothing reported for this long.
async fn quiet(changes: &mut Changes, long: Duration) -> Result<()> {
    match timeout(long, changes.next()).await {
        Err(_) => Ok(()),
        Ok(Some(types)) => bail!("a change was reported: {types:?}"),
        Ok(None) => bail!("the session closed"),
    }
}

struct Test {
    helper: Helper,
    wire: Wire,
    clipboard: Clipboard,
    changes: Changes,
}

/// A type neither this crate nor Mutter knows anything about.
const OTHER: &str = "application/x-remoteman-test";

/// (a) Text copied by the app arrives through the API.
async fn text_out(t: &mut Test) -> Result<String> {
    let text = "copied in the app: žluťoučký kůň ✓ 🙂";
    t.helper.copy_text(text).await?;
    let types = change_with(&mut t.changes, UTF8).await?;
    let mime = text_type(&types).context("no text type")?;
    let got = t.clipboard.read(mime).await?;
    ensure!(got == text.as_bytes(), "read {:?}", String::from_utf8_lossy(&got));
    Ok(format!("change reported with {types:?}; read {} bytes as {mime}: {:?}", got.len(), String::from_utf8_lossy(&got)))
}

/// (b) Text offered through the API is what the app pastes, by every name for text.
async fn text_in(t: &mut Test) -> Result<String> {
    let text = "offered by the device: žluťoučký kůň ✓ 🙂";
    t.clipboard.offer(vec![(UTF8.into(), text.into())]).await?;
    let pasted = t.helper.paste_text().await?;
    ensure!(pasted == text.as_bytes(), "the app's Paste gave {:?}", String::from_utf8_lossy(&pasted));
    let names = ["text/plain", "UTF8_STRING", "STRING", "TEXT", UTF8];
    for name in names {
        let pasted = t.helper.paste(name).await?;
        ensure!(pasted == text.as_bytes(), "pasting {name} gave {:?}", String::from_utf8_lossy(&pasted));
    }
    quiet(&mut t.changes, Duration::from_millis(500)).await.context("our own offer came back as news")?;
    Ok(format!("the app's Paste gave {:?}; the same {} bytes pasted as each of {names:?}; our own offer not reported back", String::from_utf8_lossy(&pasted), pasted.len()))
}

/// (c) A picture copied by the app arrives through the API, byte for byte.
async fn picture_out(t: &mut Test) -> Result<String> {
    t.helper.copy(PNG, &PICTURE).await?;
    let types = change_with(&mut t.changes, PNG).await?;
    let got = t.clipboard.read(PNG).await?;
    ensure!(got == PICTURE, "read {} bytes that differ", got.len());
    Ok(format!("change reported with {types:?}; read {} bytes, identical", got.len()))
}

/// (d) A picture offered through the API is what the app pastes, byte for byte, along with a
/// type this crate knows nothing about; and the text offered before is gone.
async fn picture_in(t: &mut Test) -> Result<String> {
    let other = OTHER;
    let every_byte: Vec<u8> = (0..=255).collect();
    t.clipboard.offer(vec![(PNG.into(), PICTURE.to_vec()), (other.into(), every_byte.clone())]).await?;
    let formats = t.helper.formats().await?;
    ensure!(formats == [PNG, other], "the app sees {formats:?}");
    let pasted = t.helper.paste(PNG).await?;
    ensure!(pasted == PICTURE, "pasted {} bytes that differ", pasted.len());
    let passed = t.helper.paste(other).await?;
    ensure!(passed == every_byte, "{other}: pasted {} bytes that differ", passed.len());
    // Far more than a pipe holds at once, while Mutter takes its own copy alongside.
    let large: Vec<u8> = (0..2u32 << 20).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    t.clipboard.offer(vec![(PNG.into(), large.clone())]).await?;
    let big = t.helper.paste(PNG).await?;
    ensure!(big == large, "of {} bytes offered, pasted {} that differ", large.len(), big.len());
    Ok(format!(
        "the app sees {formats:?} (the text offered before is replaced); pasted {} bytes of {PNG}, identical; {} bytes of {other}, identical; then {} bytes offered as {PNG}, identical",
        pasted.len(),
        passed.len(),
        big.len()
    ))
}

/// (e) A copy larger than the cap is refused, and the clipboard goes on working.
async fn too_large(t: &mut Test) -> Result<String> {
    let blob = "application/octet-stream";
    t.helper.tell(&format!("copy-big {blob} {}", DEFAULT_CAP + 1)).await?;
    change_with(&mut t.changes, blob).await?;
    let refusal = match t.clipboard.read(blob).await {
        Ok(data) => bail!("read {} bytes, over the cap of {DEFAULT_CAP}", data.len()),
        Err(e) if e.downcast_ref::<TooLarge>().is_some() => format!("{e:#}"),
        Err(e) => return Err(e.context("failed, but not as too large")),
    };
    t.helper.tell(&format!("copy-big {blob} {DEFAULT_CAP}")).await?;
    change_with(&mut t.changes, blob).await?;
    let most = t.clipboard.read(blob).await.context("a copy of exactly the cap")?;
    ensure!(most.len() == DEFAULT_CAP, "a copy of exactly the cap read as {} bytes", most.len());
    let text = "small again";
    t.helper.copy_text(text).await?;
    change_with(&mut t.changes, UTF8).await?;
    let got = t.clipboard.read(UTF8).await.context("a small copy afterwards")?;
    ensure!(got == text.as_bytes(), "a small copy afterwards read as {:?}", String::from_utf8_lossy(&got));
    Ok(format!("{} bytes refused with \"{refusal}\"; then exactly {} bytes read, and then {:?}", DEFAULT_CAP + 1, most.len(), String::from_utf8_lossy(&got)))
}

/// (f) After turning sharing off nothing more is reported, because Mutter sends nothing more;
/// turned on again, it is.
async fn off_and_on(t: &mut Test) -> Result<String> {
    // Something of ours on the clipboard, to see it leave.
    t.clipboard.offer(vec![(UTF8.into(), "offered before sharing was turned off".into()), (OTHER.into(), vec![1, 2, 3])]).await?;
    let held = t.helper.formats().await?;
    ensure!(held.iter().any(|f| f == OTHER), "the app sees {held:?} before sharing is turned off");
    let before = t.wire.announced().await;
    ensure!(before > 0, "dbus-monitor saw none of Mutter's announcements so far");
    t.clipboard.disable().await?;
    let left = t.helper.formats().await?;
    ensure!(!left.iter().any(|f| f == OTHER), "our offer is still on the clipboard: {left:?}");
    let text = "copied while sharing was off";
    t.helper.copy_text(text).await?;
    quiet(&mut t.changes, Duration::from_millis(1500)).await?;
    let sent = t.wire.announced().await - before;
    ensure!(sent == 0, "Mutter announced {sent} clipboard owners while sharing was off");
    let read = t.clipboard.read(UTF8).await.err().context("reading still works")?;
    let offer = t.clipboard.offer(vec![(UTF8.into(), "no".into())]).await.err().context("offering still works")?;
    t.clipboard.enable().await?;
    let types = change_with(&mut t.changes, UTF8).await.context("after turning sharing on again")?;
    let got = t.clipboard.read(UTF8).await?;
    ensure!(got == text.as_bytes(), "after turning sharing on again, read {:?}", String::from_utf8_lossy(&got));
    let again = t.wire.announced().await - before;
    ensure!(again > 0, "dbus-monitor saw no announcement after sharing was turned on again");
    Ok(format!(
        "turned off, the app sees {left:?} where our offer was; the app copied, and in 1.5 s Mutter announced nothing on the bus (dbus-monitor: {before} announcements before, 0 while off, {again} once on again) and nothing was reported; read fails with \"{read:#}\", offer with \"{offer:#}\"; on again, {types:?} reported and {:?} read",
        String::from_utf8_lossy(&got)
    ))
}

/// (g) Dropping the clipboard turns sharing off as well.
async fn dropped(helper: &mut Helper, wire: &Wire, clipboard: Clipboard, mut changes: Changes) -> Result<String> {
    clipboard.offer(vec![(OTHER.into(), vec![4, 5, 6])]).await?;
    let held = helper.formats().await?;
    ensure!(held == [OTHER], "the app sees {held:?}");
    drop(clipboard);
    let ended = async { while changes.next().await.is_some() {} };
    timeout(Duration::from_secs(3), ended).await.context("the changes didn't end")?;
    let started = Instant::now();
    let mut left = helper.formats().await?;
    while !left.is_empty() && started.elapsed() < Duration::from_secs(3) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        left = helper.formats().await?;
    }
    ensure!(left.is_empty(), "our offer is still on the clipboard: {left:?}");
    let before = wire.announced().await;
    helper.copy_text("copied after the drop").await?;
    let sent = wire.announced().await - before;
    ensure!(sent == 0, "Mutter announced {sent} clipboard owners after the drop");
    Ok(format!("the changes ended; our offer ({held:?}) left the clipboard; the app copied and Mutter announced nothing on the bus"))
}

/// (h) When the session closes, the changes end and the calls fail, promptly.
async fn closed(display: VirtualDisplay) -> Result<String> {
    // A second clipboard on the same session, after the first was dropped.
    let (clipboard, mut changes) = Clipboard::new(&display, DEFAULT_CAP).await?;
    let types = change_with(&mut changes, UTF8).await.context("a new clipboard on the same session")?;
    clipboard.offer(vec![(UTF8.into(), "held when the session closed".into())]).await?;
    display.close().await;
    let ended = async { while changes.next().await.is_some() {} };
    timeout(Duration::from_secs(3), ended).await.context("the changes didn't end")?;
    let read = timeout(Duration::from_secs(3), clipboard.read(UTF8)).await.context("read hangs")?.err().context("reading still works")?;
    let offer = timeout(Duration::from_secs(3), clipboard.offer(vec![(UTF8.into(), "no".into())])).await.context("offer hangs")?.err().context("offering still works")?;
    Ok(format!("a new clipboard on the session first reported {types:?}; the session closed and the changes ended; read fails with \"{read:#}\", offer with \"{offer:#}\""))
}

fn report(failed: &mut bool, case: &str, result: Result<String>) {
    match result {
        Ok(detail) => println!("PASS {case}: {detail}"),
        Err(e) => {
            *failed = true;
            println!("FAIL {case}: {e:#}");
        }
    }
}

async fn run(dir: &Path, socket: &Path, display_name: &str) -> Result<bool> {
    let started = Instant::now();
    while !(socket.exists() && names_owned().await?) {
        ensure!(started.elapsed() < Duration::from_secs(15), "mutter didn't come up (see {})", dir.join("mutter.log").display());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let wire = Wire::start()?;
    let display = VirtualDisplay::create(false, false).await?;
    let (clipboard, changes) = Clipboard::new(&display, DEFAULT_CAP).await?;
    // After the session, whose first key makes the keyboard an app's window gets the focus of.
    let helper = Helper::start(dir, display_name).await?;
    let mut t = Test { helper, wire, clipboard, changes };

    let mut failed = false;
    report(&mut failed, "(a) text copied in an app reaches the host", text_out(&mut t).await);
    report(&mut failed, "(b) text offered by the host is what an app pastes", text_in(&mut t).await);
    report(&mut failed, "(c) a PNG copied in an app reaches the host", picture_out(&mut t).await);
    report(&mut failed, "(d) a PNG offered by the host is what an app pastes", picture_in(&mut t).await);
    report(&mut failed, "(e) a copy larger than the cap is refused", too_large(&mut t).await);
    report(&mut failed, "(f) nothing is reported while sharing is off", off_and_on(&mut t).await);
    let Test { mut helper, mut wire, clipboard, changes } = t;
    report(&mut failed, "(g) dropping the clipboard turns sharing off", dropped(&mut helper, &wire, clipboard, changes).await);
    report(&mut failed, "(h) the session closing ends it", closed(display).await);

    let _ = writeln!(helper.stdin, "quit");
    for (name, process) in [("the helper app", &mut helper.process), ("dbus-monitor", &mut wire.process)] {
        let pid = process.pid();
        process.end();
        println!("{name} (process {pid}) is {}", gone(pid));
    }
    Ok(!failed)
}
