//! GNOME: the clipboard, shared both ways through the clipboard calls of
//! `org.gnome.Mutter.RemoteDesktop.Session`. Checked against Mutter 51.1.
//!
//! From the desktop to the device: [`Changes`] says what types the desktop's clipboard holds
//! each time something is copied there, and [`Clipboard::read`] fetches one of them.
//! From the device to the desktop: [`Clipboard::offer`] puts the device's content on the
//! desktop's clipboard and hands it to whatever pastes.
//!
//! What Mutter does, as seen on 51.1 (its interface description doesn't say):
//! - `EnableClipboard` works before `Start` and after it. If the clipboard already holds
//!   something, Mutter announces it straight away.
//! - Content is never pushed: each paste asks for one type and gets a pipe. Mutter itself
//!   pastes every offer once, at once: it keeps a copy of text and pictures for when the
//!   owner goes.
//! - A type the clipboard doesn't hold reads as nothing, with no error.
//! - `text/plain` from a GTK 4 app came as ASCII with everything else escaped (`\E2\9C\93`
//!   for a check mark, in a UTF-8 locale); the UTF-8 types are the ones to read. See
//!   [`text_type`].
//! - When the app that copied exits, the clipboard is first empty and then Mutter's own, with
//!   the one type it kept.
//! - `DisableClipboard` takes an offer of ours off the clipboard, and the same happens: text
//!   or a picture we offered stays there as Mutter's copy, other types go.
//! - Our own offer is announced back to us, marked as ours, and Mutter refuses to read it
//!   back to us.
//! - A paste answered with "no" gives the app nothing at once; one never answered keeps the
//!   app waiting for 15 seconds.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::pipe;
use tokio::sync::watch;
use zbus::Proxy;
use zbus::zvariant::{self, OwnedValue, Value};

use crate::mutter::VirtualDisplay;

/// The most a single [`Clipboard::read`] takes unless told otherwise: 8 MiB.
pub const DEFAULT_CAP: usize = 8 << 20;

/// The names apps ask for text by, best first. All of them get the same UTF-8 bytes.
/// (`STRING` and `TEXT` are older X11 names that promise less than UTF-8; apps that still ask
/// for them take UTF-8.)
const TEXT_TYPES: [&str; 5] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"];

/// How long an app gets to hand over what it copied, or to take what it pastes. Mutter itself
/// gives up on a paste nobody answers after 15 seconds.
const PATIENCE: Duration = Duration::from_secs(15);

/// The type to read text as, if the clipboard holds text: UTF-8 where an app offers it.
pub fn text_type(types: &[String]) -> Option<&str> {
    TEXT_TYPES.iter().find_map(|t| types.iter().find(|have| have.as_str() == *t)).map(String::as_str)
}

/// What [`Clipboard::read`] fails with when the content is larger than the cap.
#[derive(Debug)]
pub struct TooLarge {
    /// The cap, in bytes.
    pub cap: usize,
}

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the clipboard's content is larger than {} bytes", self.cap)
    }
}

impl std::error::Error for TooLarge {}

/// What the device has put on the desktop's clipboard.
struct Offer {
    /// Each type and its bytes, as given.
    items: Vec<(String, Vec<u8>)>,
    /// The item that answers for every text name not given itself.
    text: Option<usize>,
}

impl Offer {
    fn new(items: Vec<(String, Vec<u8>)>) -> Self {
        let text = TEXT_TYPES.iter().find_map(|t| items.iter().position(|(mime, _)| mime == t));
        Self { items, text }
    }

    /// The types to announce: those given, then the text names that weren't.
    fn types(&self) -> Vec<&str> {
        let mut types: Vec<&str> = Vec::new();
        let aliases = TEXT_TYPES.iter().copied().filter(|_| self.text.is_some());
        for mime in self.items.iter().map(|(mime, _)| mime.as_str()).chain(aliases) {
            if !types.contains(&mime) {
                types.push(mime);
            }
        }
        types
    }

    fn get(&self, mime: &str) -> Option<&[u8]> {
        let given = self.items.iter().position(|(have, _)| have == mime);
        let at = given.or_else(|| self.text.filter(|_| TEXT_TYPES.contains(&mime)))?;
        Some(&self.items[at].1)
    }
}

/// What the listener and the calls share.
#[derive(Default)]
struct Shared {
    /// Whether sharing is on. Mutter says nothing while it is off; this also drops what was
    /// already on its way when it was turned off.
    on: AtomicBool,
    /// The device's content, kept until the next offer, until sharing is turned off or until
    /// the session closes. Not tied to who owns the clipboard: Mutter only asks while we do.
    offer: Mutex<Option<Arc<Offer>>>,
    /// The types an app on the desktop holds on the clipboard; none while the content is ours.
    theirs: Mutex<Vec<String>>,
}

/// The desktop's clipboard changing, as [`Clipboard::new`] hands out.
pub struct Changes(watch::Receiver<Vec<String>>);

impl Changes {
    /// Wait until something is copied on the desktop and give the types it is offered as, to
    /// pick from and [`Clipboard::read`]. No types means the clipboard was emptied. Only the
    /// newest state is kept: copies made while nobody waited show as one change. What the
    /// device itself offered is never reported. `None` once the session has closed or the
    /// [`Clipboard`] is dropped.
    pub async fn next(&mut self) -> Option<Vec<String>> {
        self.0.changed().await.ok()?;
        Some(self.0.borrow_and_update().clone())
    }
}

/// The clipboard of a [`VirtualDisplay`]'s session, shared with the device.
///
/// It works until the display is closed. Dropping it turns sharing off.
pub struct Clipboard {
    session: Proxy<'static>,
    shared: Arc<Shared>,
    cap: usize,
    /// One at a time for turning on, turning off and offering.
    turn: tokio::sync::Mutex<()>,
    listener: tokio::task::JoinHandle<()>,
    runtime: tokio::runtime::Handle,
}

impl Clipboard {
    /// Turn clipboard sharing on for the display's session. `cap` is the most bytes a
    /// [`read`](Self::read) will take ([`DEFAULT_CAP`] is a good one). If the clipboard
    /// already holds something, the first change comes at once.
    ///
    /// Needs a tokio runtime with I/O enabled, as the rest of this crate does.
    pub async fn new(display: &VirtualDisplay, cap: usize) -> Result<(Self, Changes)> {
        let session = display.session().clone();
        // Listening before turning it on, so the first announcement isn't missed.
        let owner = session.receive_signal("SelectionOwnerChanged").await?;
        let transfer = session.receive_signal("SelectionTransfer").await?;
        let closed = session.receive_signal("Closed").await?;
        let events = stream::select(stream::select(owner.map(Signal::Owner), transfer.map(Signal::Transfer)), closed.map(|_| Signal::Closed));
        let shared = Arc::new(Shared::default());
        let (changed, changes) = watch::channel(Vec::new());
        let listener = tokio::spawn(listen(session.clone(), shared.clone(), changed, events));
        let clipboard = Self { session, shared, cap, turn: Default::default(), listener, runtime: tokio::runtime::Handle::current() };
        clipboard.enable().await?;
        Ok((clipboard, Changes(changes)))
    }

    /// Turn sharing on again after [`disable`](Self::disable). Does nothing if it is on.
    pub async fn enable(&self) -> Result<()> {
        let _turn = self.turn.lock().await;
        if self.shared.on.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.shared.theirs.lock().unwrap().clear();
        let options = HashMap::<&str, Value>::new();
        if let Err(e) = self.session.call::<_, _, ()>("EnableClipboard", &(options,)).await {
            self.shared.on.store(false, Ordering::SeqCst);
            return Err(e).context("turn clipboard sharing on");
        }
        Ok(())
    }

    /// Turn sharing off: nothing more is reported, and reading and offering fail. The device's
    /// offer is taken off the desktop's clipboard, but Mutter keeps the copy it made of text
    /// or a picture, and that stays there to paste. Does nothing if it is off.
    pub async fn disable(&self) -> Result<()> {
        let _turn = self.turn.lock().await;
        if !self.shared.on.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        self.shared.offer.lock().unwrap().take();
        self.shared.theirs.lock().unwrap().clear();
        self.session.call::<_, _, ()>("DisableClipboard", &()).await.context("turn clipboard sharing off")
    }

    /// Read what an app on the desktop copied, as one of the types last reported. Fails with
    /// [`TooLarge`] (find it with `downcast_ref`) if there is more than the cap; no more than
    /// the cap is ever held in memory. Also fails if the clipboard doesn't hold that type any
    /// more, or if the app doesn't hand it over in 15 seconds.
    pub async fn read(&self, mime: &str) -> Result<Vec<u8>> {
        if !self.shared.on.load(Ordering::SeqCst) {
            bail!("clipboard sharing is off");
        }
        // Mutter would hand back nothing for a type that isn't there, and refuses to read
        // back what we offered ourselves.
        if !self.shared.theirs.lock().unwrap().iter().any(|t| t == mime) {
            bail!("the desktop's clipboard doesn't hold {mime}");
        }
        let fd: zvariant::OwnedFd = self.session.call("SelectionRead", &(mime,)).await.context("ask for the clipboard's content")?;
        let from = pipe::Receiver::from_owned_fd(OwnedFd::from(fd)).context("the clipboard's pipe")?;
        let mut data = Vec::new();
        // One byte past the cap tells too much from exactly enough. Dropping the pipe then
        // tells the app to stop writing.
        let mut capped = from.take((self.cap as u64).saturating_add(1));
        tokio::time::timeout(PATIENCE, capped.read_to_end(&mut data)).await.context("the app didn't hand over what it copied")?.context("read the clipboard's content")?;
        if data.len() > self.cap {
            return Err(TooLarge { cap: self.cap }.into());
        }
        Ok(data)
    }

    /// Put the device's content on the desktop's clipboard: each item is a type and its bytes,
    /// and whatever pastes gets the bytes of the type it asks for. A new offer replaces the
    /// one before. There is no offering nothing: Mutter refuses an empty list.
    ///
    /// One item of UTF-8 text (as `text/plain;charset=utf-8`, or any of `UTF8_STRING`,
    /// `text/plain`, `STRING`, `TEXT`) answers for all five names, since apps differ in which
    /// they ask for. Other types go through untouched.
    pub async fn offer(&self, items: Vec<(String, Vec<u8>)>) -> Result<()> {
        if items.is_empty() {
            bail!("nothing to offer");
        }
        let _turn = self.turn.lock().await;
        if !self.shared.on.load(Ordering::SeqCst) {
            bail!("clipboard sharing is off");
        }
        let offer = Arc::new(Offer::new(items));
        let mut options = HashMap::new();
        options.insert("mime-types", Value::from(offer.types()));
        // In place before Mutter hears of it: its first paste follows at once.
        let before = self.shared.offer.lock().unwrap().replace(offer.clone());
        if let Err(e) = self.session.call::<_, _, ()>("SetSelection", &(options,)).await {
            *self.shared.offer.lock().unwrap() = before;
            return Err(e).context("offer to the desktop's clipboard");
        }
        Ok(())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        self.listener.abort();
        // With nobody listening, a paste of what we offered would hang until Mutter gives up.
        if self.shared.on.swap(false, Ordering::SeqCst) {
            let session = self.session.clone();
            self.runtime.spawn(async move {
                let _ = session.call::<_, _, ()>("DisableClipboard", &()).await;
            });
        }
    }
}

enum Signal {
    Owner(zbus::Message),
    Transfer(zbus::Message),
    Closed,
}

/// Follow the session's clipboard signals until the session closes.
async fn listen(session: Proxy<'static>, shared: Arc<Shared>, changed: watch::Sender<Vec<String>>, mut events: impl stream::Stream<Item = Signal> + Unpin) {
    while let Some(signal) = events.next().await {
        match signal {
            Signal::Owner(msg) => {
                let Ok(options) = msg.body().deserialize::<HashMap<String, OwnedValue>>() else { continue };
                // Held to the end, so that once `disable` has returned nothing more is reported.
                let mut theirs = shared.theirs.lock().unwrap();
                if !shared.on.load(Ordering::SeqCst) {
                    continue;
                }
                // Our own offer coming back. Not news, and not ours to read.
                if options.get("session-is-owner").and_then(|v| bool::try_from(v).ok()).unwrap_or(false) {
                    theirs.clear();
                    continue;
                }
                // No types at all: the clipboard is empty.
                let types = options.get("mime-types").map(|v| strings(v)).unwrap_or_default();
                *theirs = types.clone();
                let _ = changed.send(types);
            }
            Signal::Transfer(msg) => {
                let Ok((mime, serial)) = msg.body().deserialize::<(String, u32)>() else { continue };
                let offer = shared.offer.lock().unwrap().clone();
                // Each paste on its own, so an app slow to take one doesn't hold up the rest.
                tokio::spawn(answer(session.clone(), offer, mime, serial));
            }
            Signal::Closed => break,
        }
    }
    // The session took its clipboard with it. Dropping `changed` ends `Changes`.
    shared.on.store(false, Ordering::SeqCst);
    shared.offer.lock().unwrap().take();
    shared.theirs.lock().unwrap().clear();
}

/// Answer one paste: write the bytes of the type asked for into the pipe Mutter gives for it.
/// Every paste gets an answer, a "no" included, or the app pasting would wait out Mutter's
/// 15 seconds.
async fn answer(session: Proxy<'static>, offer: Option<Arc<Offer>>, mime: String, serial: u32) {
    let sent = async {
        let data = offer.as_deref().and_then(|o| o.get(&mime))?;
        let fd: zvariant::OwnedFd = session.call("SelectionWrite", &(serial,)).await.ok()?;
        let mut to = pipe::Sender::from_owned_fd(OwnedFd::from(fd)).ok()?;
        tokio::time::timeout(PATIENCE, to.write_all(data)).await.ok()?.ok()
        // The pipe closes here, which is how the app knows it has everything.
    };
    let sent = sent.await.is_some();
    let _ = session.call::<_, _, ()>("SelectionWriteDone", &(serial, sent)).await;
}

/// The strings in a list, or in a list wrapped in a struct: Mutter 51.1 sends the types of
/// `SelectionOwnerChanged` as `(as)` where its interface description says `as`.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::Array(list) => list.inner().iter().filter_map(|v| v.downcast_ref::<&str>().ok().map(str::to_owned)).collect(),
        Value::Structure(wrapped) => wrapped.fields().first().map(strings).unwrap_or_default(),
        Value::Value(inner) => strings(inner),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_text_item_answers_for_every_text_name() {
        let offer = Offer::new(vec![("image/png".into(), vec![1, 2]), ("UTF8_STRING".into(), b"hi".to_vec())]);
        assert_eq!(offer.types(), ["image/png", "UTF8_STRING", "text/plain;charset=utf-8", "text/plain", "STRING", "TEXT"]);
        for name in TEXT_TYPES {
            assert_eq!(offer.get(name), Some(&b"hi"[..]));
        }
        assert_eq!(offer.get("image/png"), Some(&[1u8, 2][..]));
        assert_eq!(offer.get("text/html"), None);
    }

    #[test]
    fn a_text_name_given_itself_keeps_its_own_bytes() {
        let offer = Offer::new(vec![("text/plain".into(), b"plain".to_vec()), ("text/plain;charset=utf-8".into(), b"utf8".to_vec())]);
        assert_eq!(offer.types(), ["text/plain", "text/plain;charset=utf-8", "UTF8_STRING", "STRING", "TEXT"]);
        assert_eq!(offer.get("text/plain"), Some(&b"plain"[..]));
        assert_eq!(offer.get("STRING"), Some(&b"utf8"[..]));
    }

    #[test]
    fn no_text_no_text_names() {
        let offer = Offer::new(vec![("image/png".into(), vec![1])]);
        assert_eq!(offer.types(), ["image/png"]);
        assert_eq!(offer.get("text/plain"), None);
    }

    #[test]
    fn text_is_read_as_utf8_where_offered() {
        let types = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(text_type(&types(&["text/plain", "text/plain;charset=utf-8"])), Some("text/plain;charset=utf-8"));
        assert_eq!(text_type(&types(&["STRING", "text/html", "UTF8_STRING"])), Some("UTF8_STRING"));
        assert_eq!(text_type(&types(&["image/png"])), None);
    }

    #[test]
    fn types_come_bare_or_wrapped() {
        let bare = Value::from(vec!["a", "b"]);
        assert_eq!(strings(&bare), ["a", "b"]);
        let wrapped = Value::from(zvariant::StructureBuilder::new().add_field(vec!["a", "b"]).build().unwrap());
        assert_eq!(strings(&wrapped), ["a", "b"]);
        assert!(strings(&Value::from(7u32)).is_empty());
    }
}
