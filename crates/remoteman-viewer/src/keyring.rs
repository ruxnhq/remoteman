//! The desktop's keyring, for the one secret this program may keep: the password of a
//! computer's user, which that computer asks for when it is locked. It is kept only if the
//! person says so, under the computer's key, by the keyring service every desktop has (the
//! Secret Service on the session bus: GNOME Keyring, KWallet), which protects it with the
//! login password. Nothing is ever written to this program's own files.
//!
//! Every call gives up quietly if there is no such service: the password is then simply asked
//! for each time.

use std::collections::HashMap;

use gtk::prelude::*;
use gtk::{gio, glib};

const SERVICE: &str = "org.freedesktop.secrets";
const PATH: &str = "/org/freedesktop/secrets";

fn bus() -> Option<gio::DBusConnection> {
    gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()
}

fn call(bus: &gio::DBusConnection, path: &str, interface: &str, method: &str, arguments: Option<&glib::Variant>) -> Option<glib::Variant> {
    bus.call_sync(Some(SERVICE), path, interface, method, arguments, None, gio::DBusCallFlags::NONE, 3000, gio::Cancellable::NONE).ok()
}

/// What an entry is filed under: this program's name, and the computer's key.
fn attributes(computer: &str) -> HashMap<String, String> {
    HashMap::from([("application".to_owned(), "dev.remoteman.Viewer".to_owned()), ("computer".to_owned(), computer.to_ascii_lowercase())])
}

fn object_path(text: &str) -> Option<glib::Variant> {
    glib::variant::ObjectPath::try_from(text.to_owned()).ok().map(|p| p.to_variant())
}

/// A session with the service: secrets pass as they are (the bus is this user's own).
fn session(bus: &gio::DBusConnection) -> Option<String> {
    let opened = call(bus, PATH, "org.freedesktop.Secret.Service", "OpenSession", Some(&("plain", "".to_variant()).to_variant()))?;
    opened.child_value(1).str().map(str::to_owned)
}

/// The entries for this computer that can be read now (a locked keyring's are asked to be
/// unlocked, which the desktop may do with a question of its own).
fn items(bus: &gio::DBusConnection, computer: &str) -> Vec<String> {
    let Some(found) = call(bus, PATH, "org.freedesktop.Secret.Service", "SearchItems", Some(&(attributes(computer),).to_variant())) else { return Vec::new() };
    let paths = |v: glib::Variant| -> Vec<String> { (0..v.n_children()).filter_map(|i| v.child_value(i).str().map(str::to_owned)).collect() };
    let mut open = paths(found.child_value(0));
    let locked = paths(found.child_value(1));
    if open.is_empty() && !locked.is_empty() {
        let asked: Vec<glib::Variant> = locked.iter().filter_map(|p| object_path(p)).collect();
        let list = glib::Variant::array_from_iter_with_type(glib::VariantTy::OBJECT_PATH, asked);
        if let Some(unlocked) = call(bus, PATH, "org.freedesktop.Secret.Service", "Unlock", Some(&glib::Variant::tuple_from_iter([list]))) {
            open = paths(unlocked.child_value(0));
        }
    }
    open
}

/// The password kept for the computer with this key, if one is.
pub fn find(computer: &str) -> Option<String> {
    let bus = bus()?;
    let item = items(&bus, computer).into_iter().next()?;
    let session = session(&bus)?;
    let secret = call(&bus, &item, "org.freedesktop.Secret.Item", "GetSecret", Some(&glib::Variant::tuple_from_iter([object_path(&session)?])))?;
    // ((session, parameters, value, content type),)
    let value = secret.child_value(0).child_value(2);
    let bytes = value.fixed_array::<u8>().ok()?;
    String::from_utf8(bytes.to_vec()).ok().filter(|p| !p.is_empty())
}

/// Keep the password for the computer with this key (replacing one kept before). `name` is for
/// the keyring's own list. Says whether it was kept.
pub fn keep(computer: &str, name: &str, password: &str) -> bool {
    let kept = || -> Option<()> {
        let bus = bus()?;
        let session = session(&bus)?;
        let properties = glib::VariantDict::new(None);
        properties.insert_value("org.freedesktop.Secret.Item.Label", &format!("remoteman: the password of {name}'s user").to_variant());
        properties.insert_value("org.freedesktop.Secret.Item.Attributes", &attributes(computer).to_variant());
        let secret = glib::Variant::tuple_from_iter([object_path(&session)?, glib::Variant::array_from_fixed_array::<u8>(&[]), glib::Variant::array_from_fixed_array(password.as_bytes()), "text/plain".to_variant()]);
        let made = call(&bus, "/org/freedesktop/secrets/aliases/default", "org.freedesktop.Secret.Collection", "CreateItem", Some(&glib::Variant::tuple_from_iter([properties.end(), secret, true.to_variant()])))?;
        // An item, or "/" and a question the desktop has to ask first (not waited for).
        (made.child_value(0).str() != Some("/")).then_some(())
    };
    kept().is_some()
}

/// Throw away what is kept for the computer with this key.
pub fn forget(computer: &str) {
    let Some(bus) = bus() else { return };
    for item in items(&bus, computer) {
        let _ = call(&bus, &item, "org.freedesktop.Secret.Item", "Delete", None);
    }
}
