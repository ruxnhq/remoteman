//! KDE Plasma: a virtual monitor and input through the desktop portals
//! (`org.freedesktop.portal.RemoteDesktop` with a `ScreenCast` source of the virtual kind),
//! and the monitor's place, refresh rate and scale through KScreen (`kscreen-doctor`).
//! Checked against Plasma 6.8.3 (xdg-desktop-portal-kde and KWin 6.8.3, xdg-desktop-portal 1.22).
//!
//! What KDE does, as read in its sources and seen on 6.8.3:
//! - The portal's virtual monitor starts at 1920 by 1080, 60 Hz, scale 1, whatever is asked:
//!   the portal has no option for its size. KWin then resizes it to the size the PipeWire
//!   stream's consumer fixes, as Mutter does; refresh rate and scale are set afterwards
//!   through KScreen, as for any monitor.
//! - The monitor is called `Virtual-virtual-xdp-kde-<app id>` (with `-1`, `-2` added while
//!   one of that name exists); the stream's `mapping_id` says which.
//! - Without a dialog only if the app is "mega-authorised": the permission store's table
//!   `kde-authorized`, id `remote-desktop`, maps the app id (empty for programs that aren't
//!   sandboxed and haven't registered one; we register `dev.remoteman.Host`) to `["yes"]`. Otherwise the portal asks on the
//!   computer's screen, which nobody may be at.
//! - Pointer positions given with the stream are in the desktop's units (pixels divided by
//!   the scale) from the monitor's corner; touch positions are in the whole desktop's.
//! - The vertical wheel is turned around on its way to KWin: a positive `dy` scrolls up.
//! - The session, and the monitor with it, goes when our bus connection does, even if this
//!   program dies.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use remoteman_proto::control::{Placement, Side};
use remoteman_proto::input::{Button, Event, TouchPhase};
use remoteman_proto::keys::hid_to_linux;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

use crate::mutter::Mirrored;

pub(crate) const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub(crate) const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub(crate) const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const SCREENCAST: &str = "org.freedesktop.portal.ScreenCast";
pub(crate) const CLIPBOARD: &str = "org.freedesktop.portal.Clipboard";
/// Who the host is to the portal, and so whom the owner allows: the host app's id.
pub const APP_ID: &str = "dev.remoteman.Host";

/// Whether this session says it is KDE's, by its environment: for where asking the bus
/// can't wait. [`present`] is the one to trust.
pub fn likely() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("KDE")))
}

/// Whether this session's desktop is KDE's: KWin is on the bus.
pub async fn present() -> bool {
    let Ok(conn) = Connection::session().await else { return false };
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else { return false };
    let Ok(name) = "org.kde.KWin".try_into() else { return false };
    dbus.name_has_owner(name).await.unwrap_or(false)
}

/// Where the client's monitor is now, in the desktop's units, and the scale it is drawn at.
#[derive(Clone, Copy, Debug, Default)]
struct Geometry {
    x: f64,
    y: f64,
}

/// A virtual monitor that lives as long as this does, and the session that takes input for it.
pub struct VirtualDisplay {
    conn: Connection,
    session: OwnedObjectPath,
    /// The remote-desktop portal, for input.
    portal: Proxy<'static>,
    /// Absolute pointer positions and touches, when KWin's input server let us in.
    eis: Option<crate::eis::Eis>,
    /// KScreen's name for the monitor.
    output: String,
    /// The computer's own screens as they were before the monitor was made.
    before: Arc<Vec<Output>>,
    geometry: Arc<Mutex<Geometry>>,
    /// The scale the client asked for, set once the monitor is there.
    want_scale: Arc<Mutex<f64>>,
    /// The PipeWire node carrying the monitor's picture.
    pub node: u32,
    /// Always `None` for now: mirroring isn't offered on KDE yet.
    pub mirrored: Option<Mirrored>,
    /// Whether the portal let the clipboard in for this session.
    pub(crate) clipboard: bool,
}

static TOKENS: AtomicU32 = AtomicU32::new(0);

pub(crate) fn token() -> String {
    format!("remoteman{}_{}", std::process::id(), TOKENS.fetch_add(1, Ordering::Relaxed))
}

/// Call a portal method that answers through a Request object, and wait for its answer.
/// `options` gets the handle token added.
pub(crate) async fn request<B>(conn: &Connection, iface: &str, method: &str, body: impl FnOnce(HashMap<&'static str, Value<'static>>) -> B) -> Result<HashMap<String, OwnedValue>>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let token = token();
    let sender = conn.unique_name().context("no bus name")?.trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let req = Proxy::new(conn, PORTAL, path, "org.freedesktop.portal.Request").await?;
    let mut answers = req.receive_signal("Response").await?;
    let mut options = HashMap::new();
    options.insert("handle_token", Value::from(token));
    let portal = Proxy::new(conn, PORTAL, PORTAL_PATH, iface).await?;
    let _: OwnedObjectPath = portal.call(method, &body(options)).await.with_context(|| format!("{iface}.{method}"))?;
    // A dialog may be up on the computer's screen: give whoever is there time to answer.
    let msg = tokio::time::timeout(Duration::from_secs(60), answers.next()).await.with_context(|| format!("{method}: the portal didn't answer"))?.context("request ended")?;
    let (response, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    match response {
        0 => Ok(results),
        1 => bail!("{method}: refused on this computer (KDE asks on its own screen before a program may control the desktop; turning sharing on in Remoteman Host allows it without asking)"),
        _ => bail!("{method}: the portal failed"),
    }
}

impl VirtualDisplay {
    /// Make a virtual monitor. Its size comes from the PipeWire stream format the caller then
    /// asks for on `node`. With `cursor_apart` the cursor is left out of the picture and comes
    /// as metadata on the stream; otherwise it is drawn into the picture.
    /// Mirroring isn't built for KDE yet, so a client that asks for it gets a display of its
    /// own beside the others, as the wire spec has it for a host that doesn't offer `mirror`.
    pub async fn create(cursor_apart: bool, _mirror: bool) -> Result<Self> {
        Self::make(cursor_apart, false).await
    }

    /// A second virtual monitor for the same client.
    pub async fn create_beside(cursor_apart: bool) -> Result<Self> {
        Self::make(cursor_apart, true).await
    }

    /// `beside`: a second display for a client, whose first is among the screens to sit beside.
    async fn make(cursor_apart: bool, beside: bool) -> Result<Self> {
        let conn = Connection::session().await.context("session bus")?;
        // Say who we are before anything else on this connection: otherwise the portal names
        // us after our systemd unit, which for a service is nobody.
        // (A plain call: a proxy would read the portal's properties first, and the portal
        // would name us on that.)
        if let Err(e) = conn.call_method(Some(PORTAL), PORTAL_PATH, Some("org.freedesktop.host.portal.Registry"), "Register", &(APP_ID, HashMap::<&str, Value>::new())).await {
            tracing::warn!("couldn't register as {APP_ID} with the portal (is the host app installed?): {e}");
        }
        // The computer's own screens, before ours: KWin keeps a layout for each set of
        // screens, and ours always has the same name, so a session that turned them off would
        // otherwise leave them off for the next one. Every placement puts them back as they are now.
        let before: Vec<Output> = outputs().await.unwrap_or_default().into_iter().filter(|o| o.enabled && (beside || !o.name.starts_with("Virtual-virtual-xdp-kde-"))).collect();
        let session_token = token();
        let created = request(&conn, REMOTE, "CreateSession", |mut o| {
            o.insert("session_handle_token", Value::from(session_token));
            (o,)
        })
        .await?;
        let session: String = created.get("session_handle").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).context("no session handle")?;
        let session = OwnedObjectPath::try_from(session)?;
        let s = session.clone();
        // Keyboard, pointer and touchscreen.
        request(&conn, REMOTE, "SelectDevices", move |mut o| {
            o.insert("types", Value::from(7u32));
            (s, o)
        })
        .await?;
        let s = session.clone();
        // Virtual (4); the cursor drawn in (2) or as metadata (4).
        request(&conn, SCREENCAST, "SelectSources", move |mut o| {
            o.insert("types", Value::from(4u32));
            o.insert("cursor_mode", Value::from(if cursor_apart { 4u32 } else { 2u32 }));
            o.insert("multiple", Value::from(false));
            (s, o)
        })
        .await?;
        // The clipboard can only be asked for before the session starts; whether anything
        // goes through it is decided later, as on GNOME.
        let clip = Proxy::new(&conn, PORTAL, PORTAL_PATH, CLIPBOARD).await?;
        let clipboard_asked = clip.call::<_, _, ()>("RequestClipboard", &(&session, HashMap::<&str, Value>::new())).await.is_ok();
        let s = session.clone();
        let started = request(&conn, REMOTE, "Start", move |o| (s, "", o)).await.context("start the remote-desktop session")?;
        let clipboard = clipboard_asked && started.get("clipboard_enabled").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> =
            started.get("streams").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::try_from(v).ok()).context("the portal gave no stream (was the screen left out?)")?;
        let (node, props) = streams.into_iter().next().context("the portal gave no stream")?;
        let output = props.get("mapping_id").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).context("the stream doesn't say which monitor it is")?;
        let eis = match crate::eis::Eis::open(&conn).await {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!("no touch, and the pointer only on the monitor's first 1920 by 1080 (KWin's input server: {e:#})");
                None
            }
        };
        let portal = Proxy::new(&conn, PORTAL, PORTAL_PATH, REMOTE).await?;
        Ok(Self { conn, session, portal, eis, output, before: Arc::new(before), geometry: Arc::default(), want_scale: Arc::new(Mutex::new(1.0)), node, mirrored: None, clipboard })
    }

    /// Put this display where the client wants it among the computer's own: beside them, as
    /// the main display, or as the only one, with the others off. Also gives it the refresh
    /// rate and scale the client asked for. Call it once the stream is running.
    ///
    /// KWin remembers each set of monitors' layout, so when this one goes the computer's own
    /// screens return to how they were for themselves, even if this program has died.
    pub fn place(&self, placement: Placement) -> impl Future<Output = Result<()>> + Send + 'static {
        self.place_at(placement, Side::Right)
    }

    /// The same for a display that sits beside the others, on the side the client names.
    pub fn place_on(&self, side: Side) -> impl Future<Output = Result<()>> + Send + 'static {
        self.place_at(Placement::Extend, side)
    }

    /// Give the monitor the client's size, refresh rate and scale. Call it before the stream
    /// is connected: KWin offers the stream no faster than the monitor's refresh rate at the
    /// moment it is negotiated, and its virtual monitors start at 60 Hz. The mode is one of
    /// our own (KScreen's custom modes), added once for each size and rate.
    pub async fn prepare(&self, width: u32, height: u32, scale: f32, refresh: u32) -> Result<()> {
        let scale = scale.clamp(1.0, 4.0) as f64;
        *self.want_scale.lock().unwrap() = scale;
        let ours = wait_for(&self.output).await?;
        let mut args = vec![format!("output.{}.scale.{scale}", ours.id)];
        if refresh > 60 || ours.pixels != (width as i32, height as i32) {
            if find_mode(&self.output, width, height, refresh).await?.is_none() {
                kscreen(&[format!("output.{}.addCustomMode.{width}.{height}.{}.full", ours.id, refresh * 1000)]).await?;
            }
            let mode = find_mode(&self.output, width, height, refresh).await?.context("KScreen didn't add the mode")?;
            args.push(format!("output.{}.mode.{mode}", ours.id));
        }
        kscreen(&args).await.map(|_| ())
    }

    fn place_at(&self, placement: Placement, side: Side) -> impl Future<Output = Result<()>> + Send + 'static {
        let placement = if placement == Placement::Mirror { Placement::Extend } else { placement };
        let (output, geometry, scale, before) = (self.output.clone(), self.geometry.clone(), *self.want_scale.lock().unwrap(), self.before.clone());
        async move {
            let result = place_once(&output, &before, placement, side, scale).await;
            let result = match result {
                Ok(()) => Ok(()),
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    place_once(&output, &before, placement, side, scale).await
                }
            };
            // Wherever it ended up, input goes there.
            if let Ok(outputs) = outputs().await
                && let Some(o) = outputs.iter().find(|o| o.name == output)
            {
                *geometry.lock().unwrap() = Geometry { x: o.x as f64, y: o.y as f64 };
            }
            result
        }
    }

    /// Put one input event into the desktop. `size` is the virtual monitor's size in pixels and
    /// `scale` the scale it is drawn at.
    pub async fn inject(&self, event: &Event, size: (u32, u32), scale: f32) -> Result<()> {
        let portal = &self.portal;
        let s = &self.session;
        let none = HashMap::<&str, Value>::new;
        let scale = if scale > 0.0 { scale as f64 } else { 1.0 };
        let logical = (size.0 as f64 / scale, size.1 as f64 / scale);
        let at = |x: f32, y: f32| (x.clamp(0.0, 1.0) as f64 * logical.0, y.clamp(0.0, 1.0) as f64 * logical.1);
        let g = *self.geometry.lock().unwrap();
        // Where the monitor is, in case KWin's regions don't say.
        let fallback = (g.x, g.y, logical.0, logical.1);
        match event {
            Event::PointerTo { x, y } => {
                if self.eis.as_ref().is_some_and(|e| e.pointer_to(&self.output, fallback, *x, *y)) {
                    return Ok(());
                }
                let (x, y) = at(*x, *y);
                portal.call::<_, _, ()>("NotifyPointerMotionAbsolute", &(s, none(), self.node, x, y)).await?
            }
            Event::PointerBy { dx, dy } => portal.call::<_, _, ()>("NotifyPointerMotion", &(s, none(), *dx as f64, *dy as f64)).await?,
            Event::Display(_) | Event::Pen { .. } | Event::Gamepad { .. } => {}
            Event::Button { button, down } => {
                let code: i32 = match button {
                    Button::Left => 0x110,
                    Button::Right => 0x111,
                    Button::Middle => 0x112,
                };
                portal.call::<_, _, ()>("NotifyPointerButton", &(s, none(), code, *down as u32)).await?
            }
            // KDE turns the vertical axis around (see above).
            Event::Scroll { dx, dy } => portal.call::<_, _, ()>("NotifyPointerAxis", &(s, none(), *dx as f64, -(*dy as f64))).await?,
            Event::Key { usage, down } => {
                if let Some(code) = hid_to_linux(*usage) {
                    portal.call::<_, _, ()>("NotifyKeyboardKeycode", &(s, none(), code as i32, *down as u32)).await?
                }
            }
            Event::Touch { phase, slot, x, y } => {
                let place = (*phase != TouchPhase::Up).then_some((*x, *y));
                if self.eis.as_ref().is_some_and(|e| e.touch(&self.output, fallback, *slot as u32, *phase == TouchPhase::Down, place)) {
                    return Ok(());
                }
                // Touches are placed in the whole desktop, not the monitor (and the portal
                // refuses them past 1920 by 1080).
                let (x, y) = at(*x, *y);
                let (x, y) = (x + g.x, y + g.y);
                match phase {
                    TouchPhase::Down => portal.call::<_, _, ()>("NotifyTouchDown", &(s, none(), self.node, *slot as u32, x, y)).await?,
                    TouchPhase::Move => portal.call::<_, _, ()>("NotifyTouchMotion", &(s, none(), self.node, *slot as u32, x, y)).await?,
                    TouchPhase::Up => portal.call::<_, _, ()>("NotifyTouchUp", &(s, none(), *slot as u32)).await?,
                }
            }
            Event::Text(text) => {
                for c in text.chars() {
                    let sym = crate::mutter::keysym(c) as i32;
                    portal.call::<_, _, ()>("NotifyKeyboardKeysym", &(s, none(), sym, 1u32)).await?;
                    portal.call::<_, _, ()>("NotifyKeyboardKeysym", &(s, none(), sym, 0u32)).await?;
                }
            }
        }
        Ok(())
    }

    /// The bus connection and the session, for the clipboard.
    pub(crate) fn session(&self) -> (&Connection, &OwnedObjectPath) {
        (&self.conn, &self.session)
    }

    /// End the session; the virtual monitor goes with it.
    pub async fn close(self) {
        if let Some(e) = self.eis {
            e.close().await;
        }
        if let Ok(s) = Proxy::new(&self.conn, PORTAL, self.session.as_str(), "org.freedesktop.portal.Session").await {
            let _ = s.call::<_, _, ()>("Close", &()).await;
        }
    }
}

/// One monitor as KScreen lists it.
#[derive(Clone, Debug)]
struct Output {
    id: u64,
    name: String,
    enabled: bool,
    x: i32,
    y: i32,
    /// Its size in the desktop's units, turned as it is.
    width: i32,
    height: i32,
    scale: f64,
    priority: u32,
    /// Its mode's size in pixels, not turned.
    pixels: (i32, i32),
    /// Its mode's refresh rate. (KScreen's ids for modes change from one layout to the
    /// next, so a mode is kept as its size and rate.)
    refresh: f64,
    /// Every mode it has: KScreen's id, size in pixels, refresh rate.
    modes: Vec<(String, (i32, i32), f64)>,
}

impl Output {
    /// The id its mode of this size and rate has now.
    fn mode_id(&self, pixels: (i32, i32), refresh: f64) -> Option<&str> {
        self.modes.iter().filter(|m| m.1 == pixels).min_by(|a, b| (a.2 - refresh).abs().total_cmp(&(b.2 - refresh).abs())).map(|m| m.0.as_str())
    }
}

async fn kscreen(args: &[String]) -> Result<Vec<u8>> {
    let out = tokio::process::Command::new("kscreen-doctor").args(args).output().await.context("run kscreen-doctor (is KScreen installed?)")?;
    // It says what it couldn't do but still exits with success (6.8.3), and says nothing
    // when a change works.
    let said = format!("{}{}", String::from_utf8_lossy(&out.stderr), if args.first().is_some_and(|a| a.starts_with('-')) { String::new() } else { String::from_utf8_lossy(&out.stdout).into_owned() });
    if !out.status.success() || said.lines().any(|l| { let l = l.to_lowercase(); l.contains("not found") || l.contains("failed") || l.contains("error") || l.contains("invalid") || l.contains("unknown") }) {
        bail!("kscreen-doctor {}: {}", args.join(" "), said.trim());
    }
    Ok(out.stdout)
}

async fn outputs() -> Result<Vec<Output>> {
    let json: serde_json::Value = serde_json::from_slice(&kscreen(&["-j".into()]).await?).context("kscreen-doctor's list")?;
    let mut list = Vec::new();
    for o in json["outputs"].as_array().into_iter().flatten() {
        let mode = o["modes"].as_array().into_iter().flatten().find(|m| m["id"] == o["currentModeId"]);
        let (w, h) = mode.map_or((0, 0), |m| (m["size"]["width"].as_i64().unwrap_or(0), m["size"]["height"].as_i64().unwrap_or(0)));
        let pixels = (w as i32, h as i32);
        let scale = o["scale"].as_f64().unwrap_or(1.0).max(0.1);
        // 2 and 8: turned a quarter (left, right); KScreen's rotation is a bit set.
        let turned = matches!(o["rotation"].as_u64(), Some(2 | 8));
        let (w, h) = if turned { (h, w) } else { (w, h) };
        list.push(Output {
            id: o["id"].as_u64().unwrap_or(0),
            name: o["name"].as_str().unwrap_or_default().to_owned(),
            // (KScreen leaves `enabled` out for virtual screens that are on, 6.8.3.)
            enabled: o["enabled"].as_bool().unwrap_or(true),
            x: o["pos"]["x"].as_i64().unwrap_or(0) as i32,
            y: o["pos"]["y"].as_i64().unwrap_or(0) as i32,
            width: (w as f64 / scale).round() as i32,
            height: (h as f64 / scale).round() as i32,
            scale,
            priority: o["priority"].as_u64().unwrap_or(0) as u32,
            pixels,
            refresh: mode.and_then(|m| m["refreshRate"].as_f64()).unwrap_or(0.0),
            modes: o["modes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| Some((m["id"].as_str()?.to_owned(), (m["size"]["width"].as_i64()? as i32, m["size"]["height"].as_i64()? as i32), m["refreshRate"].as_f64()?)))
                .collect(),
        });
    }
    Ok(list)
}

/// The monitor called `name`, once KScreen lists it (a moment after the portal made it).
async fn wait_for(name: &str) -> Result<Output> {
    for _ in 0..50 {
        if let Some(o) = outputs().await?.into_iter().find(|o| o.name == name) {
            return Ok(o);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("the virtual monitor never appeared")
}

/// The id of the monitor's mode of this size whose rate is nearest `refresh` and within 1 Hz
/// of it (a mode made for 120 Hz runs at 119.95).
async fn find_mode(name: &str, width: u32, height: u32, refresh: u32) -> Result<Option<String>> {
    let json: serde_json::Value = serde_json::from_slice(&kscreen(&["-j".into()]).await?).context("kscreen-doctor's list")?;
    let output = json["outputs"].as_array().into_iter().flatten().find(|o| o["name"] == name).context("the virtual monitor went")?;
    let best = output["modes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["size"]["width"].as_u64() == Some(width as u64) && m["size"]["height"].as_u64() == Some(height as u64))
        .filter_map(|m| Some((m["id"].as_str()?.to_owned(), (m["refreshRate"].as_f64()? - refresh as f64).abs())))
        .filter(|(_, off)| *off < 1.0)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    Ok(best.map(|(id, _)| id))
}

/// One go at [`VirtualDisplay::place`]. `before` is the computer's own screens as they were
/// before ours: they are put back as they were (on, mode, scale, place, order), with ours added,
/// or turned off for [`Placement::Only`].
async fn place_once(name: &str, before: &[Output], placement: Placement, side: Side, scale: f64) -> Result<()> {
    let ours = wait_for(name).await?;
    let list = outputs().await?;
    // Unplugged since? Then they aren't ours to place. (Ids can change; names don't.)
    let rest: Vec<(&Output, &Output)> = before.iter().filter_map(|b| Some((list.iter().find(|o| o.name == b.name)?, b))).collect();
    let (w, h) = ((ours.pixels.0 as f64 / scale).round() as i32, (ours.pixels.1 as f64 / scale).round() as i32);
    let mut args = vec![format!("output.{}.scale.{scale}", ours.id), format!("output.{}.enable", ours.id)];
    if placement == Placement::Only {
        // Every other screen off, whatever turned it on since.
        args.extend(list.iter().filter(|o| o.id != ours.id && o.enabled).map(|o| format!("output.{}.disable", o.id)));
        args.push(format!("output.{}.position.0,0", ours.id));
        args.push(format!("output.{}.priority.1", ours.id));
        return kscreen(&args).await.map(|_| ());
    }
    let (x, y) = match rest.first() {
        None => (0, 0),
        Some((_, first)) => {
            // Beside the rest, level with the outermost one on that side.
            let others = || rest.iter().map(|(_, o)| *o);
            let right = others().max_by_key(|o| o.x + o.width).unwrap_or(first);
            let left = others().min_by_key(|o| o.x).unwrap_or(first);
            let top = others().min_by_key(|o| o.y).unwrap_or(first);
            let bottom = others().max_by_key(|o| o.y + o.height).unwrap_or(first);
            match side {
                Side::Right => (right.x + right.width, right.y),
                Side::Left => (left.x - w, left.y),
                Side::Above => (top.x, top.y - h),
                Side::Below => (bottom.x, bottom.y + bottom.height),
            }
        }
    };
    // KWin refuses a screen at a negative place: with ours to the left or above, everything
    // moves over by its size.
    let (dx, dy) = (rest.iter().map(|(_, o)| o.x).chain([x]).min().unwrap_or(0).min(0), rest.iter().map(|(_, o)| o.y).chain([y]).min().unwrap_or(0).min(0));
    args.push(format!("output.{}.position.{},{}", ours.id, x - dx, y - dy));
    for (now, o) in &rest {
        let id = now.id;
        args.push(format!("output.{id}.enable"));
        if let Some(mode) = now.mode_id(o.pixels, o.refresh) {
            args.push(format!("output.{id}.mode.{mode}"));
        }
        args.push(format!("output.{id}.scale.{}", o.scale));
        args.push(format!("output.{id}.position.{},{}", o.x - dx, o.y - dy));
    }
    // Priority 1 is the main display; the others keep their order behind it, or ahead of
    // ours when ours only sits beside them.
    let mut others = rest.clone();
    others.sort_by_key(|(_, o)| o.priority);
    let ours_first = placement == Placement::Main;
    for (i, (now, _)) in others.iter().enumerate() {
        args.push(format!("output.{}.priority.{}", now.id, i + 1 + ours_first as usize));
    }
    args.push(format!("output.{}.priority.{}", ours.id, if ours_first { 1 } else { others.len() + 1 }));
    kscreen(&args).await.map(|_| ())
}
