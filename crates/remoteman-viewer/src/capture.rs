//! A captured pointer: held in the window, hidden, and its raw movement reported, which is what
//! games and 3D programs on the other computer need (a pointer that stops at this window's
//! edge can't turn a camera all the way round).
//!
//! GTK has no way to do this, so Wayland's two protocols for it (pointer constraints and
//! relative pointer) are spoken directly, on GTK's own connection, with an event queue of this
//! module's own. The compositor locks the pointer while the window has it, and lets it go by
//! itself when the window loses focus.

use gdk4_wayland::prelude::*;
use gtk::prelude::*;
use wayland_client::protocol::{wl_pointer, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{zwp_locked_pointer_v1, zwp_pointer_constraints_v1};
use wayland_protocols::wp::relative_pointer::zv1::client::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1};

#[derive(Default)]
struct State {
    seat: Option<wl_seat::WlSeat>,
    constraints: Option<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>,
    relative: Option<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>,
    /// Movement not yet taken, in the device's own units.
    moved: (f64, f64),
    locked: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            // (From version 5 the seat and its pointer can be given back when done with.)
            "wl_seat" if state.seat.is_none() => state.seat = Some(registry.bind(name, version.min(5), qh, ())),
            "zwp_pointer_constraints_v1" => state.constraints = Some(registry.bind(name, 1, qh, ())),
            "zwp_relative_pointer_manager_v1" => state.relative = Some(registry.bind(name, 1, qh, ())),
            _ => {}
        }
    }
}

impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for State {
    fn event(state: &mut Self, _: &zwp_relative_pointer_v1::ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // Unaccelerated: the other computer applies its own pointer speed.
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event {
            state.moved.0 += dx_unaccel;
            state.moved.1 += dy_unaccel;
        }
    }
}

impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, ()> for State {
    fn event(state: &mut Self, _: &zwp_locked_pointer_v1::ZwpLockedPointerV1, event: zwp_locked_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => state.locked = true,
            zwp_locked_pointer_v1::Event::Unlocked => state.locked = false,
            _ => {}
        }
    }
}

/// Objects whose events are of no interest here.
macro_rules! quiet {
    ($($kind:ty),*) => { $(impl Dispatch<$kind, ()> for State {
        fn event(_: &mut Self, _: &$kind, _: <$kind as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
    })* };
}
quiet!(wl_seat::WlSeat, wl_pointer::WlPointer, zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1);

/// The pointer, captured for as long as this lives.
pub struct Capture {
    queue: EventQueue<State>,
    state: State,
    seat: wl_seat::WlSeat,
    constraints: zwp_pointer_constraints_v1::ZwpPointerConstraintsV1,
    relative: zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    pointer: wl_pointer::WlPointer,
    lock: zwp_locked_pointer_v1::ZwpLockedPointerV1,
    motion: zwp_relative_pointer_v1::ZwpRelativePointerV1,
}

impl Capture {
    /// Capture the pointer in this window. The compositor does it once the pointer is in
    /// the window and the window has the keyboard, and again each time that becomes true.
    pub fn start(window: &impl IsA<gtk::Native>) -> Result<Capture, String> {
        let surface = window.surface().ok_or("the window isn't on screen yet")?;
        let display = surface.display().downcast::<gdk4_wayland::WaylandDisplay>().map_err(|_| "pointer capture needs Wayland")?;
        let wl_surface = surface.downcast_ref::<gdk4_wayland::WaylandSurface>().and_then(|s| s.wl_surface()).ok_or("the window has no Wayland surface")?;
        let connection = display.wl_display().and_then(|d| d.backend().upgrade()).map(Connection::from_backend).ok_or("no Wayland connection")?;
        let mut queue = connection.new_event_queue::<State>();
        let handle = queue.handle();
        let _registry = connection.display().get_registry(&handle, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|e| format!("the compositor didn't answer: {e}"))?;
        let (Some(seat), Some(constraints), Some(relative)) = (state.seat.clone(), state.constraints.clone(), state.relative.clone()) else {
            return Err("this desktop can't capture the pointer".into());
        };
        let pointer = seat.get_pointer(&handle, ());
        let motion = relative.get_relative_pointer(&pointer, &handle, ());
        // Persistent: taken again whenever the window gets the pointer back.
        let lock = constraints.lock_pointer(&wl_surface, &pointer, None, zwp_pointer_constraints_v1::Lifetime::Persistent, &handle, ());
        let _ = queue.flush();
        Ok(Capture { queue, state, seat, constraints, relative, pointer, lock, motion })
    }

    /// How far the pointer has moved since this was last asked, and whether it is held now.
    pub fn take(&mut self) -> ((f64, f64), bool) {
        let _ = self.queue.dispatch_pending(&mut self.state);
        (std::mem::take(&mut self.state.moved), self.state.locked)
    }
}

impl Drop for Capture {
    /// Everything made here is given back: a pointer left behind would go on being sent every
    /// movement of the mouse, with nobody reading it.
    fn drop(&mut self) {
        self.lock.destroy();
        self.motion.destroy();
        if self.pointer.version() >= 3 {
            self.pointer.release();
        }
        if self.seat.version() >= 5 {
            self.seat.release();
        }
        self.constraints.destroy();
        self.relative.destroy();
        let _ = self.queue.flush();
    }
}
