//! Logging in from a device: asking GDM for a headless login screen (or, for a device set to
//! get in by its key alone, straight for the user's session), and following which desktop
//! session GDM puts behind it.
//!
//! How GDM does it (read from its source and gnome-remote-desktop's, and tried in spike S5):
//! `CreateRemoteDisplay` takes an id of our choosing. Every display GDM has is listed at
//! `/org/gnome/DisplayManager/Displays`; a remote one carries that id (`RemoteId`) and its
//! logind session (`SessionId`). After a login on the headless login screen a second display
//! appears with the same id and the user's new session; when the user already has a session,
//! that session's display is given the id instead. So the list says where a client belongs at
//! any moment, and the method's own answer (which in GDM 51.1 can be an error although the
//! display was made) is not relied on.

use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const GDM: &str = "org.gnome.DisplayManager";
const LOGIND: &str = "org.freedesktop.login1";

/// A desktop session GDM has put behind one of our ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// logind's id for it.
    pub id: String,
    /// Who it runs as.
    pub uid: u32,
    /// `greeter` for a login screen, `user` for someone's session.
    pub class: String,
}

pub struct Gdm {
    system: Connection,
}

type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

impl Gdm {
    pub async fn connect() -> Result<Self> {
        Ok(Self { system: Connection::system().await.context("the system bus")? })
    }

    /// Ask for a display under `remote_id` (an object path of our own making; nothing is
    /// exported there). With `user`, straight into that user's session with no password asked
    /// (GDM's "preauthenticated" path: only for devices their owner set up that way). The
    /// answer is logged and otherwise ignored; [`Self::sessions`] is what tells.
    pub async fn create_display(&self, remote_id: &str, hostname: &str, user: Option<&str>) {
        let call = async {
            let factory = Proxy::new(&self.system, GDM, "/org/gnome/DisplayManager/RemoteDisplayFactory", "org.gnome.DisplayManager.RemoteDisplayFactory").await?;
            let mut properties = HashMap::<&str, Value>::new();
            properties.insert("remote-id", Value::from(ObjectPath::try_from(remote_id)?));
            properties.insert("hostname", Value::from(hostname));
            if let Some(user) = user {
                properties.insert("preauthenticated-user", Value::from(user));
            }
            factory.call::<_, _, ()>("CreateRemoteDisplay", &(properties,)).await?;
            anyhow::Ok(())
        };
        if let Err(e) = call.await {
            tracing::info!("GDM answered the request for a display with: {e:#} (looking for the display all the same)");
        }
    }

    /// The sessions GDM has behind `remote_id` right now: a login screen's, a user's, or (for a
    /// moment, after a login) both.
    pub async fn sessions(&self, remote_id: &str) -> Result<Vec<Session>> {
        let manager = Proxy::new(&self.system, GDM, "/org/gnome/DisplayManager/Displays", "org.freedesktop.DBus.ObjectManager").await?;
        let objects: Managed = manager.call("GetManagedObjects", &()).await.context("GDM's list of displays")?;
        let mut found = Vec::new();
        for interfaces in objects.values() {
            let Some(display) = interfaces.get("org.gnome.DisplayManager.RemoteDisplay") else { continue };
            let id = display.get("RemoteId").and_then(|v| v.downcast_ref::<ObjectPath>().ok()).map(|p| p.to_string());
            if id.as_deref() != Some(remote_id) {
                continue;
            }
            let Some(session) = display.get("SessionId").and_then(|v| v.downcast_ref::<&str>().ok()).filter(|s| !s.is_empty()) else { continue };
            // A session GDM lists but logind no longer knows is on its way out.
            if let Ok(s) = self.session(session).await {
                found.push(s);
            }
        }
        Ok(found)
    }

    /// Who a logind session runs as, and what kind it is.
    async fn session(&self, id: &str) -> Result<Session> {
        let manager = Proxy::new(&self.system, LOGIND, "/org/freedesktop/login1", "org.freedesktop.login1.Manager").await?;
        let path: OwnedObjectPath = manager.call("GetSession", &(id,)).await?;
        let session = Proxy::new(&self.system, LOGIND, path, "org.freedesktop.login1.Session").await?;
        let (uid, _user): (u32, OwnedObjectPath) = session.get_property("User").await?;
        let class: String = session.get_property("Class").await?;
        Ok(Session { id: id.to_owned(), uid, class })
    }
}

/// An id for a display asked of GDM: an object path nobody else would pick.
pub fn new_remote_id() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("/dev/remoteman/client/{:x}{:x}", std::process::id(), now)
}
