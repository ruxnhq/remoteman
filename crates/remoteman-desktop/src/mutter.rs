//! GNOME: a virtual monitor through `org.gnome.Mutter.ScreenCast` and input through
//! `org.gnome.Mutter.RemoteDesktop`, both on the session bus. Checked against Mutter 51.1
//! (ScreenCast API version 4, RemoteDesktop version 1).

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use remoteman_proto::input::{Button, Event, TouchPhase};
use remoteman_proto::keys::hid_to_linux;
use zbus::zvariant::{OwnedObjectPath, Value};
use zbus::{Connection, Proxy};

const SCREENCAST: &str = "org.gnome.Mutter.ScreenCast";
const REMOTE: &str = "org.gnome.Mutter.RemoteDesktop";

/// A virtual monitor that lives as long as this does, and the session that takes input for it.
pub struct VirtualDisplay {
    remote: Proxy<'static>,
    stream_path: String,
    /// The PipeWire node carrying the monitor's picture.
    pub node: u32,
}

impl VirtualDisplay {
    /// Make a virtual monitor. Its size and refresh rate come from the PipeWire stream format
    /// the caller then asks for on `node`. With `cursor_apart` the cursor is left out of the
    /// picture and comes as metadata on the stream; otherwise it is drawn into the picture.
    pub async fn create(cursor_apart: bool) -> Result<Self> {
        let conn = Connection::session().await.context("session bus")?;
        let version: i32 = Proxy::new(&conn, SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST)
            .await?
            .get_property("Version")
            .await
            .context("Mutter's ScreenCast interface (is this a GNOME session?)")?;
        if version < 3 {
            bail!("Mutter's ScreenCast API is version {version}; virtual monitors as real monitors need 3");
        }

        let remote_path: OwnedObjectPath = Proxy::new(&conn, REMOTE, "/org/gnome/Mutter/RemoteDesktop", REMOTE).await?.call("CreateSession", &()).await?;
        let remote = Proxy::new(&conn, REMOTE, remote_path, "org.gnome.Mutter.RemoteDesktop.Session").await?;
        let session_id: String = remote.get_property("SessionId").await?;

        // A screencast tied to the remote-desktop session starts and stops with it.
        let mut props = HashMap::new();
        props.insert("remote-desktop-session-id", Value::from(session_id));
        let cast_path: OwnedObjectPath = Proxy::new(&conn, SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST).await?.call("CreateSession", &(props,)).await?;
        let cast = Proxy::new(&conn, SCREENCAST, cast_path, "org.gnome.Mutter.ScreenCast.Session").await?;

        let mut props = HashMap::new();
        // 1: drawn into the picture. 2: as metadata.
        props.insert("cursor-mode", Value::from(if cursor_apart { 2u32 } else { 1u32 }));
        // A real monitor to the desktop, not "the screen is being shared".
        props.insert("is-platform", Value::from(true));
        let stream_path: OwnedObjectPath = cast.call("RecordVirtual", &(props,)).await?;
        let stream = Proxy::new(&conn, SCREENCAST, stream_path.clone(), "org.gnome.Mutter.ScreenCast.Stream").await?;
        let mut added = stream.receive_signal("PipeWireStreamAdded").await?;

        remote.call::<_, _, ()>("Start", &()).await.context("start the remote-desktop session (is the session locked?)")?;
        let msg = tokio::time::timeout(Duration::from_secs(5), added.next()).await.context("Mutter didn't announce the stream")?.context("stream signal ended")?;
        let node: u32 = msg.body().deserialize()?;
        // Mutter makes its virtual keyboard on the first key event and that event is lost
        // (seen on 51.1), so spend a key that does nothing on it: KEY_UNKNOWN.
        for down in [true, false] {
            let _ = remote.call::<_, _, ()>("NotifyKeyboardKeycode", &(240u32, down)).await;
        }
        Ok(Self { remote, stream_path: stream_path.to_string(), node })
    }

    /// Put one input event into the desktop. `size` is the virtual monitor's size in pixels and
    /// `scale` the scale Mutter draws it at: Mutter places pointers and touches in the desktop's
    /// scaled units, so in pixels divided by the scale.
    pub async fn inject(&self, event: &Event, size: (u32, u32), scale: f32) -> Result<()> {
        let r = &self.remote;
        let scale = if scale > 0.0 { scale as f64 } else { 1.0 };
        let logical = (size.0 as f64 / scale, size.1 as f64 / scale);
        let at = |x: f32, y: f32| (x.clamp(0.0, 1.0) as f64 * logical.0, y.clamp(0.0, 1.0) as f64 * logical.1);
        match event {
            Event::PointerTo { x, y } => {
                let (x, y) = at(*x, *y);
                r.call::<_, _, ()>("NotifyPointerMotionAbsolute", &(self.stream_path.as_str(), x, y)).await?
            }
            Event::PointerBy { dx, dy } => r.call::<_, _, ()>("NotifyPointerMotionRelative", &(*dx as f64, *dy as f64)).await?,
            Event::Button { button, down } => {
                // Linux BTN_LEFT, BTN_RIGHT, BTN_MIDDLE.
                let code: i32 = match button {
                    Button::Left => 0x110,
                    Button::Right => 0x111,
                    Button::Middle => 0x112,
                };
                r.call::<_, _, ()>("NotifyPointerButton", &(code, *down)).await?
            }
            Event::Scroll { dx, dy } => r.call::<_, _, ()>("NotifyPointerAxis", &(*dx as f64, *dy as f64, 0u32)).await?,
            Event::Key { usage, down } => {
                if let Some(code) = hid_to_linux(*usage) {
                    r.call::<_, _, ()>("NotifyKeyboardKeycode", &(code as u32, *down)).await?
                }
            }
            Event::Touch { phase, slot, x, y } => {
                let (x, y) = at(*x, *y);
                match phase {
                    TouchPhase::Down => r.call::<_, _, ()>("NotifyTouchDown", &(self.stream_path.as_str(), *slot as u32, x, y)).await?,
                    TouchPhase::Move => r.call::<_, _, ()>("NotifyTouchMotion", &(self.stream_path.as_str(), *slot as u32, x, y)).await?,
                    TouchPhase::Up => r.call::<_, _, ()>("NotifyTouchUp", &(*slot as u32,)).await?,
                }
            }
            Event::Text(text) => {
                for c in text.chars() {
                    let sym = keysym(c);
                    r.call::<_, _, ()>("NotifyKeyboardKeysym", &(sym, true)).await?;
                    r.call::<_, _, ()>("NotifyKeyboardKeysym", &(sym, false)).await?;
                }
            }
        }
        Ok(())
    }

    /// End the session; the virtual monitor goes with it.
    pub async fn close(self) {
        let _ = self.remote.call::<_, _, ()>("Stop", &()).await;
    }
}

/// The X keysym for a character: Latin-1 as itself, control keys by name, the rest as Unicode.
fn keysym(c: char) -> u32 {
    match c {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\u{8}' => 0xff08,
        c if (c as u32) < 0x100 => c as u32,
        c => 0x0100_0000 | c as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::keysym;

    #[test]
    fn keysyms() {
        assert_eq!(keysym('a'), 0x61);
        assert_eq!(keysym('é'), 0xe9);
        assert_eq!(keysym('\n'), 0xff0d);
        assert_eq!(keysym('ა'), 0x0100_10d0);
        assert_eq!(keysym('🙂'), 0x0101_f642);
    }
}
