//! Absolute pointer positions and touches through libei, for KDE.
//!
//! The portal's own `NotifyPointerMotionAbsolute` and `NotifyTouch*` can't serve a virtual
//! monitor: xdg-desktop-portal refuses any position outside the stream's size as the KDE
//! portal reported it when the stream was made (1920 by 1080, before KWin resized the monitor),
//! and KDE's portal sends touches without the frame that ends them. So these go through a
//! second remote-desktop session with no screen, connected to KWin's EIS server
//! (`ConnectToEIS`), where positions are in the desktop's units over KWin's own regions.
//! A session that has connected to EIS can't use the `Notify*` calls, which is why keys,
//! text, buttons and the wheel stay on the first session.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use reis::ei;
use reis::event::{DeviceCapability, EiEvent, Region};
use zbus::zvariant::{OwnedFd, OwnedObjectPath, Value};
use zbus::{Connection, Proxy};

/// A device KWin lets us drive, while it does.
struct Live {
    device: ei::Device,
    pointer: Option<ei::PointerAbsolute>,
    touch: Option<ei::Touchscreen>,
    /// For its regions: the monitors it covers.
    info: reis::event::Device,
    serial: u32,
}

#[derive(Default)]
struct State {
    devices: Vec<Live>,
}

/// The EIS connection; it ends with its session.
pub(crate) struct Eis {
    context: ei::Context,
    state: Arc<Mutex<State>>,
    session: OwnedObjectPath,
    conn: Connection,
}

impl Eis {
    /// Open a screenless remote-desktop session for the pointer and touchscreen, and connect
    /// it to KWin's EIS server.
    pub(crate) async fn open(conn: &Connection) -> Result<Self> {
        let session_token = super::kde::token();
        let created = super::kde::request(conn, super::kde::REMOTE, "CreateSession", |mut o| {
            o.insert("session_handle_token", Value::from(session_token));
            (o,)
        })
        .await?;
        let session: String = created.get("session_handle").and_then(|v| String::try_from(v.try_clone().ok()?).ok()).context("no session handle")?;
        let session = OwnedObjectPath::try_from(session)?;
        let s = session.clone();
        // Pointer and touchscreen.
        super::kde::request(conn, super::kde::REMOTE, "SelectDevices", move |mut o| {
            o.insert("types", Value::from(6u32));
            (s, o)
        })
        .await?;
        let s = session.clone();
        super::kde::request(conn, super::kde::REMOTE, "Start", move |o| (s, "", o)).await.context("start the input session")?;
        let portal = Proxy::new(conn, super::kde::PORTAL, super::kde::PORTAL_PATH, super::kde::REMOTE).await?;
        let fd: OwnedFd = portal.call("ConnectToEIS", &(&session, HashMap::<&str, Value>::new())).await.context("connect to KWin's input server")?;
        let stream = UnixStream::from(std::os::fd::OwnedFd::from(fd));
        stream.set_nonblocking(true)?;
        let context = ei::Context::new(stream)?;
        let state = Arc::new(Mutex::new(State::default()));
        // The event stream isn't Send: it gets a thread and a runtime of its own, and ends
        // when KWin hangs up, which it does when the session closes.
        let (st, ctx) = (state.clone(), context.clone());
        let (said, hear) = tokio::sync::oneshot::channel::<Result<()>>();
        std::thread::Builder::new().name("eis".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                let shaken = tokio::time::timeout(Duration::from_secs(5), ctx.handshake_tokio("remoteman", ei::handshake::ContextType::Sender)).await;
                let mut events = match shaken {
                    Ok(Ok((_connection, events))) => events,
                    Ok(Err(e)) => return drop(said.send(Err(anyhow::anyhow!("the handshake with KWin's input server: {e}")))),
                    Err(_) => return drop(said.send(Err(anyhow::anyhow!("KWin's input server didn't answer")))),
                };
                let _ = said.send(Ok(()));
                read(&mut events, &st, &ctx).await;
            });
        })?;
        hear.await.context("the input thread ended")??;
        let eis = Self { context, state, session, conn: conn.clone() };
        // The devices come a moment after the handshake.
        for _ in 0..30 {
            if eis.state.lock().unwrap().devices.iter().any(|l| l.serial != 0) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(eis)
    }

    /// Where on the desktop a point of `output` is, as fractions of it: KWin's region for that
    /// monitor, found by its name, or else by `fallback` (its place and size in the desktop's
    /// units).
    fn place(regions: &[Region], output: &str, fallback: (f64, f64, f64, f64), x: f32, y: f32) -> (f32, f32) {
        let (rx, ry, rw, rh) = regions
            .iter()
            .find(|r| r.mapping_id.as_deref() == Some(output))
            .or_else(|| regions.iter().find(|r| (r.x as f64 - fallback.0).abs() < 1.0 && (r.y as f64 - fallback.1).abs() < 1.0))
            .map_or(fallback, |r| (r.x as f64, r.y as f64, r.width as f64, r.height as f64));
        // Just inside the far edges: a region ends before x + width.
        let x = rx + (x.clamp(0.0, 1.0) as f64 * rw).min(rw - 0.5);
        let y = ry + (y.clamp(0.0, 1.0) as f64 * rh).min(rh - 0.5);
        (x as f32, y as f32)
    }

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
    }

    /// Move the pointer to a point of `output`. False if KWin gives us no device for it.
    pub(crate) fn pointer_to(&self, output: &str, fallback: (f64, f64, f64, f64), x: f32, y: f32) -> bool {
        let s = self.state.lock().unwrap();
        let Some(l) = s.devices.iter().find(|l| l.serial != 0 && l.pointer.is_some()) else { return false };
        let (x, y) = Self::place(l.info.regions(), output, fallback, x, y);
        l.pointer.as_ref().unwrap().motion_absolute(x, y);
        l.device.frame(l.serial, Self::now());
        let _ = self.context.flush();
        true
    }

    /// A touch going down, moving (`Some` place) or lifting (`None`).
    pub(crate) fn touch(&self, output: &str, fallback: (f64, f64, f64, f64), slot: u32, down: bool, at: Option<(f32, f32)>) -> bool {
        let s = self.state.lock().unwrap();
        let Some(l) = s.devices.iter().find(|l| l.serial != 0 && l.touch.is_some()) else { return false };
        let touch = l.touch.as_ref().unwrap();
        match at {
            Some((x, y)) => {
                let (x, y) = Self::place(l.info.regions(), output, fallback, x, y);
                if down { touch.down(slot, x, y) } else { touch.motion(slot, x, y) }
            }
            None => touch.up(slot),
        }
        l.device.frame(l.serial, Self::now());
        let _ = self.context.flush();
        true
    }

    pub(crate) async fn close(self) {
        if let Ok(s) = Proxy::new(&self.conn, super::kde::PORTAL, self.session.as_str(), "org.freedesktop.portal.Session").await {
            let _ = s.call::<_, _, ()>("Close", &()).await;
        }
    }
}

/// Follow KWin's input server: take the devices it offers and keep track of which we may use.
async fn read(events: &mut reis::tokio::EiConvertEventStream, state: &Mutex<State>, ctx: &ei::Context) {
    let mut sequence = 0;
    while let Some(Ok(event)) = events.next().await {
        let mut s = state.lock().unwrap();
        match event {
            EiEvent::SeatAdded(e) => e.seat.bind_capabilities(DeviceCapability::PointerAbsolute | DeviceCapability::Touch),
            EiEvent::DeviceAdded(e) => {
                let d = &e.device;
                if d.has_capability(DeviceCapability::PointerAbsolute) || d.has_capability(DeviceCapability::Touch) {
                    tracing::debug!(name = ?d.name(), regions = ?d.regions(), "an input device of KWin's");
                    s.devices.push(Live { device: d.device().clone(), pointer: d.interface(), touch: d.interface(), info: d.clone(), serial: 0 });
                }
            }
            EiEvent::DeviceResumed(e) => {
                if let Some(l) = s.devices.iter_mut().find(|l| &l.device == e.device.device()) {
                    l.serial = e.serial;
                    sequence += 1;
                    l.device.start_emulating(e.serial, sequence);
                }
            }
            EiEvent::DevicePaused(e) => {
                if let Some(l) = s.devices.iter_mut().find(|l| &l.device == e.device.device()) {
                    l.serial = 0;
                }
            }
            EiEvent::DeviceRemoved(e) => s.devices.retain(|l| &l.device != e.device.device()),
            EiEvent::Disconnected(_) => {
                s.devices.clear();
                break;
            }
            _ => {}
        }
        let _ = ctx.flush();
    }
    state.lock().unwrap().devices.clear();
}
