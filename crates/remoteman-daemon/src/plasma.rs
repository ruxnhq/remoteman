//! Logging in from a device on a computer whose login manager is Plasma Login (KDE).
//!
//! Plasma Login 6.8 has no headless login screen to ask for, as GDM has: there is one login
//! screen, on the computer's own monitors, run as the user `plasmalogin` in a session of class
//! `greeter`, with its own systemd user manager (`plasma-login-wayland.target`). An agent starts
//! there as in any session. A device whose owner isn't logged in is handed to that agent and
//! sees the login screen (mirrored); once its owner has logged in, its agent takes the device.
//! The login screen's agent is believed only when the kernel and logind say so: its process
//! runs as the login manager's user (the agents' socket is open to every local user, and what a
//! device types on a login screen is a password), and logind has that user's `greeter` session.

use anyhow::{Context, Result};
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, Proxy};

use crate::gdm::Session;

const LOGIND: &str = "org.freedesktop.login1";
/// The user Plasma Login runs its login screen as.
pub const GREETER_USER: &str = "plasmalogin";

/// Whether this computer's login manager is Plasma Login.
pub fn in_use() -> bool {
    std::fs::read_link("/etc/systemd/system/display-manager.service").is_ok_and(|p| p.file_name().is_some_and(|n| n == "plasmalogin.service"))
}

pub struct Plasma {
    system: Connection,
    /// The login screen's user.
    pub greeter_uid: u32,
}

impl Plasma {
    pub async fn connect(greeter_uid: u32) -> Result<Self> {
        Ok(Self { system: Connection::system().await.context("the system bus")?, greeter_uid })
    }

    async fn manager(&self) -> Result<Proxy<'static>> {
        Ok(Proxy::new(&self.system, LOGIND, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?)
    }

    /// Whether the login screen is up: logind has a graphical session of class `greeter` for
    /// the login manager's user. (Its agent doesn't run inside that session but under that
    /// user's own systemd, so the agent is known by its user, which the kernel vouches for and
    /// only root could take, and the session by this.)
    pub async fn login_screen_up(&self) -> bool {
        self.graphical().await.is_ok_and(|sessions| sessions.iter().any(|s| s.uid == self.greeter_uid && s.class == "greeter"))
    }

    /// The graphical sessions on the computer's own seat, the login screen's included. A
    /// session over ssh or on a text console is not one.
    pub async fn graphical(&self) -> Result<Vec<Session>> {
        let manager = self.manager().await?;
        let list: Vec<(String, u32, String, String, OwnedObjectPath)> = manager.call("ListSessions", &()).await.context("logind's sessions")?;
        let mut found = Vec::new();
        for (id, uid, _user, seat, path) in list {
            if seat != "seat0" {
                continue;
            }
            let Ok(session) = Proxy::new(&self.system, LOGIND, path, "org.freedesktop.login1.Session").await else { continue };
            let kind: String = session.get_property("Type").await.unwrap_or_default();
            let state: String = session.get_property("State").await.unwrap_or_default();
            if !matches!(kind.as_str(), "wayland" | "x11") || state == "closing" {
                continue;
            }
            let class: String = session.get_property("Class").await.unwrap_or_default();
            found.push(Session { id, uid, class });
        }
        Ok(found)
    }
}
