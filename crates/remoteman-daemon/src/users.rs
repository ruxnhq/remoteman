//! Whose each paired device is, and the one rule about where a device may go.
//!
//! The trust list (the core's, `host/trusted`) says which devices may connect at all. Beside
//! it the daemon keeps `device-users`: one line for each device that has a user of its own,
//!
//!     FINGERPRINT UID
//!
//! the fingerprint as the trust list has it (64 hex characters), the user as a number. A
//! device with no line is the `--user` user's (every device paired before users were kept).
//! The file is replaced whole by a rename, so it is never read half written.
//!
//! Sharing switched off is remembered next to it as empty files: `sharing-off.UID` for one
//! user's devices, `sharing-off.all` for everybody's (root's word). A bare `sharing-off`, from
//! before users were kept, is the `--user` user's, or everybody's when there is no such user.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::gdm::Session;

/// May a device that belongs to `user` be given to a session running as `agent_uid`?
/// Only to its own user's session, or to a login screen (`login_screen`: a session GDM itself
/// put behind this device's display that isn't anybody's). A device with no user at all (a
/// daemon run without `--user`) goes wherever it is sent, as it always did.
pub fn may_serve(user: Option<u32>, agent_uid: u32, login_screen: bool) -> bool {
    login_screen || user.is_none_or(|u| u == agent_uid)
}

/// Of the sessions GDM has behind a device's display, the one the device goes to: its own
/// user's if it is there, else the login screen. Never somebody else's.
pub fn session_for(sessions: &[Session], user: u32) -> Option<&Session> {
    sessions.iter().find(|s| s.class == "user" && s.uid == user).or_else(|| sessions.iter().find(|s| s.class != "user"))
}

/// What a login on a device's login screen came to.
#[derive(Debug, PartialEq, Eq)]
pub enum LoggedIn<'a> {
    /// Nobody has logged in yet.
    Nobody,
    /// The device's own user: it follows them into this session.
    Owner(&'a Session),
    /// Somebody else (their uid): the device must not follow.
    Stranger(u32),
}

pub fn logged_in(sessions: &[Session], user: u32) -> LoggedIn<'_> {
    let mut users = sessions.iter().filter(|s| s.class == "user");
    match (sessions.iter().find(|s| s.class == "user" && s.uid == user), users.next()) {
        (Some(own), _) => LoggedIn::Owner(own),
        (None, Some(other)) => LoggedIn::Stranger(other.uid),
        (None, None) => LoggedIn::Nobody,
    }
}

/// The daemon's own notes about users: which device is whose, and whose sharing is off.
pub struct Users {
    dir: Option<PathBuf>,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    of: BTreeMap<String, u32>,
    off_all: bool,
    off: BTreeSet<u32>,
}

impl Users {
    /// `dir`: the daemon's state folder (none: nothing is kept). `owner`: the `--user` user.
    pub fn load(dir: Option<&Path>, owner: Option<u32>) -> Self {
        let mut inner = Inner::default();
        if let Some(dir) = dir {
            for line in std::fs::read_to_string(dir.join("device-users")).unwrap_or_default().lines() {
                if let Some((fp, uid)) = line.trim().split_once(' ') {
                    if let (64, Ok(uid)) = (fp.len(), uid.trim().parse()) {
                        inner.of.insert(fp.to_owned(), uid);
                    }
                }
            }
            inner.off_all = dir.join("sharing-off.all").exists();
            if dir.join("sharing-off").exists() {
                match owner {
                    Some(uid) => drop(inner.off.insert(uid)),
                    None => inner.off_all = true,
                }
            }
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let name = entry.file_name();
                if let Some(uid) = name.to_str().and_then(|n| n.strip_prefix("sharing-off.")).and_then(|n| n.parse().ok()) {
                    inner.off.insert(uid);
                }
            }
        }
        Self { dir: dir.map(Path::to_owned), inner: Mutex::new(inner) }
    }

    /// The user a device was paired by, if that was kept.
    pub fn get(&self, fingerprint: &str) -> Option<u32> {
        self.inner.lock().unwrap().of.get(fingerprint).copied()
    }

    /// Keep `fingerprint` as `user`'s, or (`None`) as nobody's in particular. Says what it
    /// was before.
    pub fn set(&self, fingerprint: &str, user: Option<u32>) -> Option<u32> {
        let mut inner = self.inner.lock().unwrap();
        let was = match user {
            Some(uid) => inner.of.insert(fingerprint.to_owned(), uid),
            None => inner.of.remove(fingerprint),
        };
        if was != user {
            if let Some(dir) = &self.dir {
                let text: String = inner.of.iter().map(|(fp, uid)| format!("{fp} {uid}\n")).collect();
                let new = dir.join("device-users.new");
                if let Err(e) = std::fs::write(&new, text).and_then(|()| std::fs::rename(&new, dir.join("device-users"))) {
                    tracing::warn!("whose the devices are couldn't be saved in {}: {e}", dir.display());
                }
            }
        }
        was
    }

    /// Is sharing off for a device of `user`'s?
    pub fn off(&self, user: Option<u32>) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.off_all || user.is_some_and(|u| inner.off.contains(&u))
    }

    /// Switch sharing off or on: for `user`'s devices, or (`None`) for everybody's. The note
    /// on disk is changed under the same lock, so two requests at once can't leave the note
    /// saying one thing and the daemon another.
    pub fn set_off(&self, user: Option<u32>, off: bool, owner: Option<u32>) {
        let mut inner = self.inner.lock().unwrap();
        match user {
            Some(uid) if off => drop(inner.off.insert(uid)),
            Some(uid) => drop(inner.off.remove(&uid)),
            None => inner.off_all = off,
        }
        let Some(dir) = &self.dir else { return };
        let note = dir.join(match user {
            Some(uid) => format!("sharing-off.{uid}"),
            None => "sharing-off.all".to_owned(),
        });
        let _ = if off { std::fs::write(note, "") } else { std::fs::remove_file(note) };
        // The note from before users were kept says the same thing for whoever it was.
        if !off && (user == owner || (user.is_none() && owner.is_none())) {
            let _ = std::fs::remove_file(dir.join("sharing-off"));
        }
    }
}

/// A user's name, from the system's user database.
pub fn name_of(uid: u32) -> Option<String> {
    // SAFETY: the entry is read at once, before any other such call, and its name is a valid
    // C string for as long.
    unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() { None } else { Some(std::ffi::CStr::from_ptr((*entry).pw_name).to_string_lossy().into_owned()) }
    }
}

/// Is `uid` somebody who logs in here, as far as the user database says: an account with a
/// shell that isn't one of the two that turn everybody away (the accounts of services, and of
/// GDM's login screen, have those).
pub fn is_person(uid: u32) -> bool {
    // SAFETY: as in `name_of`.
    let shell = unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() || (*entry).pw_shell.is_null() {
            return false;
        }
        std::ffi::CStr::from_ptr((*entry).pw_shell).to_string_lossy().into_owned()
    };
    !shell.is_empty() && !shell.ends_with("/nologin") && !shell.ends_with("/false")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, uid: u32, class: &str) -> Session {
        Session { id: id.into(), uid, class: class.into() }
    }

    #[test]
    fn a_device_goes_only_to_its_own_users_session_or_a_login_screen() {
        assert!(may_serve(Some(1000), 1000, false));
        assert!(!may_serve(Some(1000), 1001, false));
        // The login screen runs as GDM's user, whoever the device belongs to.
        assert!(may_serve(Some(1000), 120, true));
        // No user at all: the daemon as it was before users.
        assert!(may_serve(None, 1001, false));
    }

    #[test]
    fn who_the_user_database_makes_out_to_be_a_person() {
        // SAFETY: no arguments, no state.
        let me = unsafe { libc::getuid() };
        // Whoever runs the tests logs in; `nobody` doesn't, and nor does a number with no account.
        assert!(is_person(me) || std::env::var_os("CI").is_some());
        assert!(!is_person(65534));
        assert!(!is_person(2009));
    }

    #[test]
    fn behind_a_devices_display_only_its_user_or_the_login_screen_counts() {
        let greeter = session("c1", 120, "greeter");
        let theirs = session("5", 1000, "user");
        let other = session("6", 1001, "user");
        assert_eq!(session_for(&[greeter.clone(), theirs.clone()], 1000), Some(&theirs));
        assert_eq!(session_for(&[greeter.clone(), other.clone()], 1000), Some(&greeter));
        // Somebody else's session alone is nowhere to go.
        assert_eq!(session_for(&[other.clone()], 1000), None);
        assert_eq!(session_for(&[], 1000), None);

        assert_eq!(logged_in(&[greeter.clone()], 1000), LoggedIn::Nobody);
        assert_eq!(logged_in(&[greeter.clone(), theirs.clone()], 1000), LoggedIn::Owner(&theirs));
        assert_eq!(logged_in(&[greeter.clone(), other.clone()], 1000), LoggedIn::Stranger(1001));
        assert_eq!(logged_in(&[other, theirs.clone()], 1000), LoggedIn::Owner(&theirs));
    }

    #[test]
    fn whose_a_device_is_and_whose_sharing_is_off_outlive_the_daemon() {
        let dir = std::env::temp_dir().join(format!("remotemand-users-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = ("a".repeat(64), "b".repeat(64));
        {
            let users = Users::load(Some(&dir), Some(1000));
            assert_eq!(users.get(&a), None);
            assert_eq!(users.set(&a, Some(1001)), None);
            assert_eq!(users.set(&b, Some(1002)), None);
            assert_eq!(users.set(&b, None), Some(1002));
            users.set_off(Some(1001), true, Some(1000));
            assert!(users.off(Some(1001)) && !users.off(Some(1000)) && !users.off(None));
        }
        assert_eq!(std::fs::read_to_string(dir.join("device-users")).unwrap(), format!("{a} 1001\n"));
        let users = Users::load(Some(&dir), Some(1000));
        assert_eq!((users.get(&a), users.get(&b)), (Some(1001), None));
        assert!(users.off(Some(1001)) && !users.off(Some(1000)));
        users.set_off(Some(1001), false, Some(1000));
        // The note from before users were kept is the `--user` user's, and theirs to clear.
        std::fs::write(dir.join("sharing-off"), "").unwrap();
        let users = Users::load(Some(&dir), Some(1000));
        assert!(users.off(Some(1000)) && !users.off(Some(1001)));
        users.set_off(Some(1000), false, Some(1000));
        assert!(!dir.join("sharing-off").exists());
        // With no such user it is everybody's.
        std::fs::write(dir.join("sharing-off"), "").unwrap();
        assert!(Users::load(Some(&dir), None).off(Some(1001)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
