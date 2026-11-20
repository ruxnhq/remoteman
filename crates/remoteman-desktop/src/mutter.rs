//! GNOME: a virtual monitor through `org.gnome.Mutter.ScreenCast` and input through
//! `org.gnome.Mutter.RemoteDesktop`, both on the session bus. Checked against Mutter 51.1
//! (ScreenCast API version 4, RemoteDesktop version 1).

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use remoteman_proto::control::{Placement, Side};
use remoteman_proto::input::{Button, Event, TouchPhase};
use remoteman_proto::keys::hid_to_linux;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

const SCREENCAST: &str = "org.gnome.Mutter.ScreenCast";
const REMOTE: &str = "org.gnome.Mutter.RemoteDesktop";
const DISPLAYS: &str = "org.gnome.Mutter.DisplayConfig";

// What `DisplayConfig.GetCurrentState` returns: a serial, the monitors with their modes, the
// logical monitors (how they are laid out), and properties.
type MonitorId = (String, String, String, String);
type ModeInfo = (String, i32, i32, f64, f64, Vec<f64>, HashMap<String, OwnedValue>);
type MonitorInfo = (MonitorId, Vec<ModeInfo>, HashMap<String, OwnedValue>);
type LogicalInfo = (i32, i32, f64, u32, bool, Vec<MonitorId>, HashMap<String, OwnedValue>);
type DisplayState = (u32, Vec<MonitorInfo>, Vec<LogicalInfo>, HashMap<String, OwnedValue>);

async fn display_state(conn: &Connection) -> Result<DisplayState> {
    let config = Proxy::new(conn, DISPLAYS, "/org/gnome/Mutter/DisplayConfig", DISPLAYS).await?;
    Ok(config.call("GetCurrentState", &()).await?)
}

/// A virtual monitor that lives as long as this does, and the session that takes input for it.
pub struct VirtualDisplay {
    conn: Connection,
    /// The computer's own displays as they were before this one.
    before: Before,
    remote: Proxy<'static>,
    stream_path: String,
    /// The PipeWire node carrying the monitor's picture.
    pub node: u32,
    /// Not a monitor of the client's own but one of the computer's, shown as it is: its size
    /// in pixels, its refresh rate and the scale the desktop draws it at.
    pub mirrored: Option<Mirrored>,
}

/// The monitor a mirroring client is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mirrored {
    pub width: u32,
    pub height: u32,
    pub refresh: u32,
    pub scale: f32,
}

/// The computer's main monitor (or its first, if none is marked so): its connector and what a
/// client mirroring it needs to know.
fn main_monitor(state: &DisplayState) -> Option<(String, Mirrored)> {
    let (_, monitors, logical, _) = state;
    let chosen = logical.iter().find(|l| l.4).or_else(|| logical.first())?;
    let connector = &chosen.5.first()?.0;
    let monitor = monitors.iter().find(|m| &m.0.0 == connector)?;
    let mode = monitor.1.iter().find(|mode| mode.6.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false))?;
    // A monitor turned on its side is as tall as its mode is wide.
    let turned = matches!(chosen.3, 1 | 3 | 5 | 7);
    let (width, height) = if turned { (mode.2, mode.1) } else { (mode.1, mode.2) };
    Some((connector.clone(), Mirrored { width: width as u32, height: height as u32, refresh: mode.3.round().max(1.0) as u32, scale: chosen.2 as f32 }))
}

/// What a client that asks to mirror would be shown now: the computer's main monitor.
pub async fn mirror_target() -> Result<Mirrored> {
    let conn = Connection::session().await.context("session bus")?;
    let state = display_state(&conn).await.context("the computer's displays")?;
    main_monitor(&state).map(|(_, what)| what).context("this computer has no monitor to mirror")
}

impl VirtualDisplay {
    /// Make a virtual monitor. Its size and refresh rate come from the PipeWire stream format
    /// the caller then asks for on `node`. With `cursor_apart` the cursor is left out of the
    /// picture and comes as metadata on the stream; otherwise it is drawn into the picture.
    /// With `mirror` no monitor is made: the computer's main monitor is recorded as it is
    /// (`mirrored` says what it is), and the client's input lands on it.
    pub async fn create(cursor_apart: bool, mirror: bool) -> Result<Self> {
        Self::make(cursor_apart, mirror, false).await
    }

    /// A second virtual monitor for the same client, beside its first: the first is one of
    /// the displays that are there before, and is not waited on to go.
    pub async fn create_beside(cursor_apart: bool) -> Result<Self> {
        Self::make(cursor_apart, false, true).await
    }

    async fn make(cursor_apart: bool, mirror: bool, beside: bool) -> Result<Self> {
        let conn = Connection::session().await.context("session bus")?;
        let version: i32 = Proxy::new(&conn, SCREENCAST, "/org/gnome/Mutter/ScreenCast", SCREENCAST)
            .await?
            .get_property("Version")
            .await
            .context("Mutter's ScreenCast interface (is this a GNOME session?)")?;
        if version < 3 {
            bail!("Mutter's ScreenCast API is version {version}; virtual monitors as real monitors need 3");
        }

        // A device that leaves and comes straight back finds the monitor of its last session
        // still listed for a moment. Counted among the displays that were there before, it
        // would hide the new one, which gets the same name: wait for it to go (two seconds
        // at most; a virtual monitor that stays is someone else's and is left alone).
        let mut before = Before::default();
        for _ in 0..20 {
            let Ok(state) = display_state(&conn).await else { break };
            let stale = state.1.iter().any(|m| m.0.0.starts_with("Meta-"));
            before = Before::of(&state);
            if !stale || beside {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
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
        let mut mirrored = None;
        let stream_path: OwnedObjectPath = if mirror {
            let state = display_state(&conn).await.context("the computer's displays")?;
            let (connector, what) = main_monitor(&state).context("this computer has no monitor to mirror")?;
            mirrored = Some(what);
            cast.call("RecordMonitor", &(connector.as_str(), props)).await.context("record the monitor")?
        } else {
            // A real monitor to the desktop, not "the screen is being shared".
            props.insert("is-platform", Value::from(true));
            cast.call("RecordVirtual", &(props,)).await?
        };
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
        // The same goes for its virtual pointer: a click with no movement before it went
        // nowhere (seen on a login screen, where the first thing a user does is click a name).
        // A move by nothing makes the pointer without moving it.
        let _ = remote.call::<_, _, ()>("NotifyPointerMotionRelative", &(0.0f64, 0.0f64)).await;
        Ok(Self { conn, before, remote, stream_path: stream_path.to_string(), node, mirrored })
    }

    /// Put this display where the client wants it among the computer's own: beside them, as
    /// the main display, or as the only one, with the others off. Call it once the stream is
    /// running, since the monitor only exists from then.
    ///
    /// Beside them needs doing too. When a monitor appears that Mutter has no stored layout
    /// for (and it never has one for these, each gets a new serial number), Mutter lays all
    /// monitors out afresh in a row at each one's preferred mode: a 240 Hz screen drops to
    /// 60 Hz and screens may change sides. So the computer's own displays are always put back
    /// exactly as they were (place, mode, scale, rotation, colour mode), with this one added.
    ///
    /// The change is a temporary configuration for exactly this set of monitors. When this
    /// display goes, the set changes back and Mutter returns to the layout it has stored for
    /// the computer's own screens, without being asked, even if this program has died.
    pub fn place(&self, placement: Placement) -> impl Future<Output = Result<()>> + Send + 'static {
        self.place_at(placement, Side::Right)
    }

    /// The same for a display that sits beside the others ([`Placement::Extend`]), on the
    /// side of them the client names. The others keep their places among themselves.
    pub fn place_on(&self, side: Side) -> impl Future<Output = Result<()>> + Send + 'static {
        self.place_at(Placement::Extend, side)
    }

    fn place_at(&self, placement: Placement, side: Side) -> impl Future<Output = Result<()>> + Send + 'static {
        let (conn, before, mirrored) = (self.conn.clone(), self.before.clone(), self.mirrored.is_some());
        async move {
            // Nothing of the computer's own to put back or to place this one among, or no
            // monitor of the client's own to place.
            if before.layout.is_empty() || mirrored {
                return Ok(());
            }
            // The layout may change under us between reading it and applying ours (the serial
            // goes stale), so a failure gets one more go.
            match place_once(&conn, &before, placement, side).await {
                Ok(()) => Ok(()),
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    place_once(&conn, &before, placement, side).await
                }
            }
        }
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
            // Which display the pointer is on is the caller's to keep track of: it picks the
            // display this is asked of.
            Event::Display(_) => {}
            Event::Pen { .. } => {}
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

    /// The remote-desktop session, for what else goes through it: the clipboard.
    pub(crate) fn session(&self) -> &Proxy<'static> {
        &self.remote
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
    use super::{allowed_scale, keysym};

    #[test]
    fn a_scale_the_desktop_refuses_becomes_the_nearest_it_takes() {
        assert_eq!(allowed_scale(2.0, &[1.0, 1.25, 1.5, 1.75, 2.0]), 2.0);
        assert_eq!(allowed_scale(2.0, &[1.0, 1.25, 1.5]), 1.5);
        assert_eq!(allowed_scale(3.0, &[1.0]), 1.0);
        assert_eq!(allowed_scale(2.0, &[]), 2.0);
    }

    #[test]
    fn keysyms() {
        assert_eq!(keysym('a'), 0x61);
        assert_eq!(keysym('é'), 0xe9);
        assert_eq!(keysym('\n'), 0xff0d);
        assert_eq!(keysym('ა'), 0x0100_10d0);
        assert_eq!(keysym('🙂'), 0x0101_f642);
    }
}

/// One of the computer's own displays (or several showing the same) as it was.
#[derive(Clone)]
struct Was {
    x: i32,
    y: i32,
    scale: f64,
    transform: u32,
    primary: bool,
    /// The size of its mode in pixels.
    size: (i32, i32),
    /// Each monitor in it: connector, mode, colour mode, underscanning.
    outputs: Vec<(String, String, Option<u32>, bool)>,
}

/// The displays as they were before the virtual monitor was made.
#[derive(Clone, Default)]
struct Before {
    connectors: Vec<String>,
    layout: Vec<Was>,
}

impl Before {
    fn of(state: &DisplayState) -> Self {
        let (_, monitors, logical, _) = state;
        let mut layout = Vec::new();
        for l in logical {
            let mut size = (0, 0);
            let mut outputs = Vec::new();
            for id in &l.5 {
                let Some(m) = monitors.iter().find(|m| m.0.0 == id.0) else { continue };
                let Some(mode) = m.1.iter().find(|mode| mode.6.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false)) else { continue };
                size = (mode.1, mode.2);
                let colour = m.2.get("color-mode").and_then(|v| u32::try_from(v).ok());
                let underscan = m.2.get("is-underscanning").and_then(|v| bool::try_from(v).ok()) == Some(true);
                outputs.push((id.0.clone(), mode.0.clone(), colour, underscan));
            }
            if !outputs.is_empty() {
                layout.push(Was { x: l.0, y: l.1, scale: l.2, transform: l.3, primary: l.4, size, outputs });
            }
        }
        Self { connectors: monitors.iter().map(|m| m.0.0.clone()).collect(), layout }
    }
}

/// The scale to lay a monitor out at. Mutter draws a virtual monitor at the scale the device
/// asked for, but takes a layout only with a scale from the mode's own list, which leaves out
/// those that would make the desktop too small to use (2 at 1280 by 720): then the nearest one
/// it does take, so the display is still placed where it was asked to be.
fn allowed_scale(scale: f64, allowed: &[f64]) -> f64 {
    if allowed.is_empty() || allowed.iter().any(|a| (a - scale).abs() < 0.01) {
        return scale;
    }
    allowed.iter().copied().min_by(|a, b| (a - scale).abs().total_cmp(&(b - scale).abs())).unwrap_or(scale)
}

/// One go at [`VirtualDisplay::place`].
async fn place_once(conn: &Connection, before: &Before, placement: Placement, side: Side) -> Result<()> {
    // The monitor appears a moment after the stream's format is agreed, and joins the layout
    // a moment after that.
    let mut tries = 0;
    let (state, ours, ours_scale, ours_mode, ours_size) = loop {
        let state = display_state(conn).await?;
        let found = state.1.iter().find(|m| m.0.0.starts_with("Meta-") && !before.connectors.contains(&m.0.0)).and_then(|m| {
            let mode = m.1.iter().find(|mode| mode.6.get("is-current").and_then(|v| bool::try_from(v).ok()).unwrap_or(false))?;
            let scale = state.2.iter().find(|l| l.5.iter().any(|id| id.0 == m.0.0))?.2;
            Some((m.0.0.clone(), allowed_scale(scale, &mode.5), mode.0.clone(), (mode.1, mode.2)))
        });
        match found {
            Some((c, scale, mode, size)) => break (state, c, scale, mode, size),
            None if tries < 50 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            None => bail!("the virtual monitor never appeared"),
        }
    };
    let (serial, monitors, _, globals) = state;
    // Positions are in the desktop's units: pixels divided by the scale, unless Mutter lays
    // out in physical pixels (layout mode 2).
    let physical = globals.get("layout-mode").and_then(|v| u32::try_from(v).ok()) == Some(2);
    let mut layout = Vec::new();
    // The rest's edges, each with where its outermost display starts along it:
    // (left edge, that display's y), (right edge, y), (top edge, x), (bottom edge, x).
    let mut edges = None::<[(i32, i32); 4]>;
    if placement != Placement::Only {
        for was in &before.layout {
            // Unplugged since? Then it isn't ours to place.
            if !was.outputs.iter().all(|o| monitors.iter().any(|m| m.0.0 == o.0)) {
                continue;
            }
            let outputs: Vec<_> = was
                .outputs
                .iter()
                .map(|(connector, mode, colour, underscan)| {
                    // Whatever isn't said goes back to its default, so say what each had:
                    // above all its colour mode, or an HDR screen would drop out of HDR.
                    let mut keep = HashMap::<String, Value>::new();
                    if let Some(c) = colour {
                        keep.insert("color-mode".into(), Value::from(*c));
                    }
                    if *underscan {
                        keep.insert("underscanning".into(), Value::from(true));
                    }
                    (connector.clone(), mode.clone(), keep)
                })
                .collect();
            let turned = matches!(was.transform, 1 | 3 | 5 | 7);
            let (width, height) = if turned { (was.size.1, was.size.0) } else { was.size };
            let units = |pixels: i32| if physical { pixels } else { (pixels as f64 / was.scale).round() as i32 };
            let (width, height) = (units(width), units(height));
            let e = edges.get_or_insert([(was.x, was.y), (was.x + width, was.y), (was.y, was.x), (was.y + height, was.x)]);
            if was.x < e[0].0 {
                e[0] = (was.x, was.y);
            }
            if was.x + width > e[1].0 {
                e[1] = (was.x + width, was.y);
            }
            if was.y < e[2].0 {
                e[2] = (was.y, was.x);
            }
            if was.y + height > e[3].0 {
                e[3] = (was.y + height, was.x);
            }
            layout.push((was.x, was.y, was.scale, was.transform, was.primary && placement == Placement::Extend, outputs));
        }
    }
    // Ours goes beside the rest on the side asked for (the right, unless the client said),
    // level with its neighbour.
    let units = |pixels: i32| if physical { pixels } else { (pixels as f64 / ours_scale).round() as i32 };
    let (x, y) = match (edges, side) {
        (None, _) => (0, 0),
        (Some(e), Side::Right) => e[1],
        (Some(e), Side::Left) => (e[0].0 - units(ours_size.0), e[0].1),
        (Some(e), Side::Above) => (e[2].1, e[2].0 - units(ours_size.1)),
        (Some(e), Side::Below) => (e[3].1, e[3].0),
    };
    // Mutter wants the whole layout to start at 0,0: with ours to the left or above, the
    // rest moves over by its size.
    let (shift_x, shift_y) = (layout.iter().map(|l| l.0).chain([x]).min().unwrap_or(0), layout.iter().map(|l| l.1).chain([y]).min().unwrap_or(0));
    for l in &mut layout {
        (l.0, l.1) = (l.0 - shift_x, l.1 - shift_y);
    }
    let (x, y) = (x - shift_x, y - shift_y);
    let main = placement != Placement::Extend || !layout.iter().any(|l| l.4);
    layout.push((x, y, ours_scale, 0u32, main, vec![(ours, ours_mode, HashMap::<String, Value>::new())]));
    let config = Proxy::new(conn, DISPLAYS, "/org/gnome/Mutter/DisplayConfig", DISPLAYS).await?;
    // Keep the layout mode (logical or physical pixels) as it is.
    let mut properties = HashMap::<String, Value>::new();
    let can_set = globals.get("supports-changing-layout-mode").and_then(|v| bool::try_from(v).ok()) == Some(true);
    if let (true, Some(mode)) = (can_set, globals.get("layout-mode").and_then(|v| u32::try_from(v).ok())) {
        properties.insert("layout-mode".into(), Value::from(mode));
    }
    // 1: temporary (not written to disk).
    config.call::<_, _, ()>("ApplyMonitorsConfig", &(serial, 1u32, layout, properties)).await.context("apply the monitor layout")?;
    Ok(())
}
