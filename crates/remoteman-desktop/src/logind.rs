//! What kind of desktop session this process serves, from logind, and ending it. A login
//! screen's session is treated differently from a user's: it shares no clipboard, takes no
//! files, and a headless one is logged out when its client has gone (GDM leaves that to
//! whoever asked for it).

use anyhow::{Context, Result};
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

const LOGIND: &str = "org.freedesktop.login1";

/// The graphical session of the user this process runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionKind {
    /// `greeter` for a login screen, `user` for someone's session.
    pub class: String,
    /// Made for a remote client (GDM's headless displays are), not for a seat's own screens.
    pub remote: bool,
}

impl SessionKind {
    pub fn is_login_screen(&self) -> bool {
        self.class == "greeter"
    }
}

/// Ask logind about this user's main session. A program started by the user's systemd, as the
/// agent is, belongs to no session of its own, so it is the user's "display" session that is
/// asked about.
pub async fn session_kind() -> Result<SessionKind> {
    let system = Connection::system().await.context("the system bus")?;
    let user = Proxy::new(&system, LOGIND, "/org/freedesktop/login1/user/self", "org.freedesktop.login1.User").await?;
    let (_id, path): (String, OwnedObjectPath) = user.get_property("Display").await.context("this user has no graphical session")?;
    let session = Proxy::new(&system, LOGIND, path, "org.freedesktop.login1.Session").await?;
    Ok(SessionKind { class: session.get_property("Class").await?, remote: session.get_property("Remote").await? })
}

/// This user's graphical session, as logind knows it.
async fn own_session(system: &Connection) -> Result<Proxy<'static>> {
    let user = Proxy::new(system, LOGIND, "/org/freedesktop/login1/user/self", "org.freedesktop.login1.User").await?;
    let (_id, path): (String, OwnedObjectPath) = user.get_property("Display").await.context("this user has no graphical session")?;
    Ok(Proxy::new(system, LOGIND, path, "org.freedesktop.login1.Session").await?)
}

/// Whether this user's session is locked (as its desktop last told logind).
pub async fn locked() -> Result<bool> {
    let system = Connection::system().await.context("the system bus")?;
    Ok(own_session(&system).await?.get_property("LockedHint").await?)
}

/// Lock this user's session, or unlock it: logind passes the request to the desktop, which
/// does it (a user may ask this of their own session; nobody else's). Unlocking waits, for a
/// few seconds at most, until the desktop says it is unlocked, and fails if it doesn't.
pub async fn set_locked(lock: bool) -> Result<()> {
    let system = Connection::system().await.context("the system bus")?;
    let session = own_session(&system).await?;
    session.call::<_, _, ()>(if lock { "Lock" } else { "Unlock" }, &()).await.context(if lock { "ask for the session to be locked" } else { "ask for the session to be unlocked" })?;
    if !lock {
        for _ in 0..40 {
            if !session.get_property::<bool>("LockedHint").await.unwrap_or(true) {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        anyhow::bail!("the desktop stayed locked");
    }
    Ok(())
}

/// End this desktop session, with no questions asked on screen.
pub async fn log_out() -> Result<()> {
    let bus = Connection::session().await.context("session bus")?;
    let manager = Proxy::new(&bus, "org.gnome.SessionManager", "/org/gnome/SessionManager", "org.gnome.SessionManager").await?;
    // 1: no confirmation.
    manager.call::<_, _, ()>("Logout", &(1u32,)).await.context("ask the session to end")?;
    Ok(())
}
