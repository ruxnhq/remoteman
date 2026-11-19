//! A second display in the same session: a window of its own showing a second monitor that
//! the computer makes beside the first. The computer sends its picture as a stream of its own
//! (docs/PROTOCOL.md, "A second display"); the pointer is on whichever window it was last
//! moved in, and the computer is told when that changes.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, glib};
use remoteman_core::{Button, Event, Message, Placement, keys};

use crate::video::Video;
use crate::window::App;

pub struct Second {
    pub window: gtk::Window,
    pub video: Rc<Video>,
}

impl App {
    /// The pointer is on this display of the session from here on (0: the first window's).
    pub fn point_at(&self, index: u8) {
        if self.pointer_on.replace(index) != index {
            self.send(Event::Display(index));
        }
    }

    /// The Display menu's line: add the second display, or take it away.
    pub fn toggle_second(self: &Rc<Self>) {
        if self.second.borrow().is_some() {
            self.close_second(true);
        } else {
            self.open_second();
        }
    }

    /// Take the second display away. `tell`: the computer doesn't know yet.
    pub fn close_second(&self, tell: bool) {
        let Some(second) = self.second.borrow_mut().take() else { return };
        if tell {
            if let Some(s) = self.session.borrow().as_ref() {
                s.client.send_message(Message::Display { index: 1, on: false, display: None, message: String::new() });
            }
        }
        second.video.detach();
        second.video.free_gl();
        second.window.destroy();
        self.point_at(0);
        self.view.show_second(false);
    }

    fn open_second(self: &Rc<Self>) {
        let (name, offered, shared) = match self.session.borrow().as_ref() {
            Some(s) => (self.target.borrow().as_ref().map(|t| t.name.clone()).unwrap_or_default(), s.offers("displays"), s.second.clone()),
            None => return,
        };
        if !offered {
            return self.toast("This computer gives a device one display");
        }
        // The size of the monitor this window is on, like the first.
        let mut display = self.display(Placement::Extend);
        display.placement = Placement::Extend;
        let video = Video::new(None);
        let window = gtk::Window::builder().title(format!("{name}: second display")).default_width(1280).default_height(720).child(&video.stack).build();
        window.set_application(self.window.application().as_ref());
        video.attach(shared);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion({
            let (app, video) = (self.clone(), video.clone());
            move |_, x, y| {
                app.point_at(1);
                let (x, y) = video.fraction(x, y);
                app.send(Event::PointerTo { x, y });
            }
        });
        video.stack.add_controller(motion);
        let click = gtk::GestureClick::builder().button(0).build();
        let button = |g: &gtk::GestureClick| match g.current_button() {
            2 => Button::Middle,
            3 => Button::Right,
            _ => Button::Left,
        };
        click.connect_pressed({
            let (app, video) = (self.clone(), video.clone());
            move |g, _, x, y| {
                app.point_at(1);
                let (x, y) = video.fraction(x, y);
                app.send(Event::PointerTo { x, y });
                app.send(Event::Button { button: button(g), down: true });
            }
        });
        click.connect_released({
            let app = self.clone();
            move |g, _, _, _| app.send(Event::Button { button: button(g), down: false })
        });
        video.stack.add_controller(click);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll({
            let app = self.clone();
            move |c, dx, dy| {
                app.point_at(1);
                let k = if c.unit() == gdk::ScrollUnit::Wheel { 40.0 } else { 1.0 };
                let k = if app.store.borrow().settings.input.scroll_reversed { -k } else { k };
                app.send(Event::Scroll { dx: (dx * k) as f32, dy: (dy * k) as f32 });
                glib::Propagation::Stop
            }
        });
        video.stack.add_controller(scroll);
        // Keys typed in this window go to the computer as the first window's do.
        let typed = gtk::EventControllerKey::new();
        typed.set_propagation_phase(gtk::PropagationPhase::Capture);
        typed.connect_key_pressed({
            let app = self.clone();
            move |_, _, keycode, _| {
                if let Some(own) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                    let usage = app.key_for(own);
                    app.held.borrow_mut().insert(usage);
                    app.send(Event::Key { usage, down: true });
                }
                glib::Propagation::Stop
            }
        });
        typed.connect_key_released({
            let app = self.clone();
            move |_, _, keycode, _| {
                if let Some(own) = keycode.checked_sub(8).and_then(|c| keys::linux_to_hid(c as u16)) {
                    let usage = app.key_for(own);
                    if app.held.borrow_mut().remove(&usage) {
                        app.send(Event::Key { usage, down: false });
                    }
                }
            }
        });
        window.add_controller(typed);
        // Closing the window takes the display away on the computer too.
        window.connect_close_request({
            let app = self.clone();
            move |_| {
                app.close_second(true);
                glib::Propagation::Stop
            }
        });
        window.present();
        if let Some(s) = self.session.borrow().as_ref() {
            s.client.send_message(Message::Display { index: 1, on: true, display: Some(display), message: String::new() });
        }
        *self.second.borrow_mut() = Some(Second { window, video });
        self.view.show_second(true);
    }

    /// Once per frame: the second display's newest picture, and word that it is gone.
    pub fn tick_second(&self) {
        let (picture, gone) = match self.session.borrow().as_ref() {
            Some(s) => (s.second.picture.lock().unwrap().take(), s.shared.second_gone.lock().unwrap().take()),
            None => return,
        };
        if let Some(why) = gone {
            if self.second.borrow().is_some() {
                self.toast(&if why.is_empty() { "The second display is gone".to_owned() } else { format!("The second display couldn't be made: {why}") });
                self.close_second(false);
            }
            return;
        }
        if let (Some(p), Some(second)) = (picture, self.second.borrow().as_ref()) {
            second.video.show(p);
        }
    }
}
