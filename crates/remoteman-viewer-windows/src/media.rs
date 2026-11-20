//! This PC's microphone and camera lent to the session's computer, and the switches that allow
//! them (and touch, and waking): what the app does with them, as the Linux viewer's
//! `stream.rs` does (`set_microphone`, `set_camera`). The recording is `mic.rs`'s, the camera
//! `camera.rs`'s.
//!
//! Neither is opened before the person switches it on in a session, and only if Settings lets
//! computers use it and the computer takes it (`mic`, `camera` in its features). Both stop when
//! the session ends, when the computer withdraws the feature, and when Settings stops allowing
//! it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use remoteman_core::Message;

use crate::app::App;
use crate::features::FEATURES;
use crate::stream::Shared;

/// Whether the session's computer may be lent the microphone now, and if not why not (for the
/// switch's tip).
pub fn microphone_allowed(app: &App) -> Result<(), &'static str> {
    let Some(s) = &app.session else { return Err("Not connected") };
    if !FEATURES.microphone {
        Err(crate::features::NOT_YET)
    } else if !s.offers("mic") {
        Err("This computer doesn't take a microphone")
    } else if !app.store.settings.allow_microphone {
        Err("Computers may use this microphone once Settings allows it")
    } else {
        Ok(())
    }
}

/// As [`microphone_allowed`], for the camera.
pub fn camera_allowed(app: &App) -> Result<(), &'static str> {
    let Some(s) = &app.session else { return Err("Not connected") };
    if !FEATURES.camera {
        Err(crate::features::NOT_YET)
    } else if !s.offers("camera") {
        Err("This computer doesn't take a camera")
    } else if !app.store.settings.allow_camera {
        Err("Computers may use this camera once Settings allows it")
    } else {
        Ok(())
    }
}

/// Let go of the microphone and the camera of a session that is ending (the core may keep its
/// `Shared` a little longer).
pub fn stop_all(shared: &Shared) {
    shared.camera_turn.fetch_add(1, Ordering::Relaxed);
    let camera = shared.camera.lock().unwrap().take();
    drop(camera);
    let microphone = shared.microphone.lock().unwrap().take();
    drop(microphone);
}

impl App {
    /// A switch of the settings that has no `Flag` of the old pages: `touch`,
    /// `allow-microphone`, `allow-camera`, or with an address `wake` (one computer's "Wake it
    /// over the network"). `on`: what it is to be; `None` flips it. Whether it was understood.
    pub fn set_option(&mut self, what: &str, on: Option<bool>, address: Option<&str>) -> bool {
        let flip = |now: bool| on.unwrap_or(!now);
        match (what, address) {
            ("wake", Some(address)) => {
                let Some(now) = self.store.find(address).map(|c| c.wake) else { return false };
                let to = flip(now);
                self.store.update(address, |c| c.wake = to);
            }
            ("touch", None) => {
                self.store.settings.touch = flip(self.store.settings.touch);
                self.store.save_settings();
            }
            ("game-controllers", None) => {
                self.store.settings.gamepad = flip(self.store.settings.gamepad);
                self.store.save_settings();
            }
            ("allow-microphone", None) => {
                self.store.settings.allow_microphone = flip(self.store.settings.allow_microphone);
                self.store.save_settings();
                if !self.store.settings.allow_microphone {
                    self.set_microphone(false);
                }
            }
            ("allow-camera", None) => {
                self.store.settings.allow_camera = flip(self.store.settings.allow_camera);
                self.store.save_settings();
                if !self.store.settings.allow_camera {
                    self.set_camera(false);
                }
            }
            _ => {
                say!("a setting nobody knows: {what}");
                return false;
            }
        }
        say!("setting: {what}{} is now {}", address.map(|a| format!(" of {a}")).unwrap_or_default(), on.map_or("flipped".into(), |b| b.to_string()));
        self.dirty = true;
        true
    }

    /// Lend the session's computer this PC's microphone, or stop.
    pub fn set_microphone(&mut self, on: bool) {
        if on && let Err(why) = microphone_allowed(self) {
            self.say(why);
            return;
        }
        let Some(s) = self.session.as_mut() else { return };
        // (A made-up session, `--play`, has no computer: the microphone is recorded all the
        // same and its packets go nowhere, for trying the app's whole path.)
        let client = s.client.clone();
        let shared = s.shared.clone();
        let mut microphone = shared.microphone.lock().unwrap();
        if !on {
            if microphone.take().is_some() {
                if let Some(client) = &client {
                    client.send_message(Message::Mic { on: false, codec: String::new(), rate: 0, channels: 0, frame_us: 0 });
                }
                say!("microphone: given back");
            }
            s.mic_on = false;
            self.dirty = true;
            return;
        }
        if microphone.is_some() {
            return;
        }
        let sender = client.as_ref().map(|c| c.mic_sender());
        // Silence is left out by the framer; the first packet after a gap says so.
        let mut last = None::<u64>;
        let opened = crate::mic::Capture::open(crate::mic::Source::Microphone, move |packet| {
            let start = last.is_none_or(|at| packet.capture_us.saturating_sub(at) > 25_000);
            last = Some(packet.capture_us);
            if let Some(sender) = &sender {
                sender.send(start, packet.seq, packet.capture_us, &packet.data);
            }
        });
        match opened {
            Ok(capture) => {
                if let Some(client) = &client {
                    client.send_message(Message::Mic { on: true, codec: "opus".into(), rate: remoteman_audio::SAMPLE_RATE, channels: 1, frame_us: remoteman_audio::FRAME_US as u32 });
                }
                *microphone = Some(capture);
                s.mic_on = true;
            }
            Err(e) => {
                drop(microphone);
                self.say(&format!("The microphone can't be used: {e:#}"));
            }
        }
        self.dirty = true;
    }

    /// Lend the session's computer this PC's camera, or stop. The camera is opened on a thread
    /// of its own (one that doesn't answer takes seconds to give up on); if it can't be, the
    /// app hears why in [`App::media_tick`].
    pub fn set_camera(&mut self, on: bool) {
        if on && let Err(why) = camera_allowed(self) {
            self.say(why);
            return;
        }
        let Some(s) = self.session.as_mut() else { return };
        s.camera_on = on;
        self.dirty = true;
        // (A made-up session, `--play`: the camera is read and encoded, and its frames go
        // nowhere.)
        let client = s.client.clone();
        let shared = s.shared.clone();
        // Each change of mind has a number, so a camera that opens after it was switched off
        // again is let go of.
        let turn = shared.camera_turn.fetch_add(1, Ordering::Relaxed) + 1;
        if !on {
            let had = shared.camera.lock().unwrap().take();
            drop(had);
            if shared.camera_told.swap(false, Ordering::Relaxed) {
                if let Some(client) = &client {
                    client.send_message(Message::Camera { on: false, codec: String::new(), width: 0, height: 0, fps: 0 });
                }
                say!("camera: given back");
            }
            return;
        }
        if shared.camera_told.swap(true, Ordering::Relaxed) {
            return;
        }
        // Said first: frames that reach the computer before it are thrown away there.
        if let Some(client) = &client {
            client.send_message(Message::Camera { on: true, codec: "h264".into(), width: crate::camera::MAX.0, height: crate::camera::MAX.1, fps: crate::camera::FPS });
        }
        let began = std::time::Instant::now();
        let sender = client.as_ref().map(|c| c.camera_sender());
        let started = std::thread::Builder::new().name("camera-start".into()).spawn(move || {
            let opened = crate::camera::Camera::start(move |data, key| {
                if let Some(sender) = &sender {
                    sender.send(data, key, began.elapsed().as_micros() as u64);
                }
            });
            let current = || shared.camera_turn.load(Ordering::Relaxed) == turn;
            match opened {
                Ok(camera) => {
                    let mut slot = shared.camera.lock().unwrap();
                    if current() {
                        *slot = Some(camera);
                    }
                    // (Otherwise it was switched off meanwhile, and the camera is let go of here.)
                }
                Err(e) if current() => {
                    say!("camera: can't be used: {e:#}");
                    *shared.camera_failed.lock().unwrap() = Some(format!("{e:#}"));
                    shared.scene.wake_window();
                }
                Err(_) => {}
            }
        });
        if let Err(e) = started {
            *s.shared.camera_failed.lock().unwrap() = Some(e.to_string());
        }
    }

    /// What became of the camera, and what the computer and Settings allow now: what may no
    /// longer be used stops.
    pub fn media_tick(&mut self) {
        let Some(s) = &self.session else { return };
        let failed = s.shared.camera_failed.lock().unwrap().take();
        let (mic, camera) = (s.mic_on, s.camera_on);
        if let Some(why) = failed {
            self.say(&format!("The camera can't be used: {why}"));
            self.set_camera(false);
        }
        if mic && microphone_allowed(self).is_err() {
            self.set_microphone(false);
        }
        if camera && camera_allowed(self).is_err() {
            self.set_camera(false);
        }
    }
}

/// The camera's and microphone's part of the state the view shows, for the session.
#[allow(dead_code)] // The engine's (the old program draws its own bar).
pub fn session_state(app: &App) -> serde_json::Value {
    let Some(s) = &app.session else { return serde_json::Value::Null };
    let (mic, camera) = (microphone_allowed(app), camera_allowed(app));
    let device = |shared: &Arc<Shared>| shared.microphone.lock().unwrap().as_ref().map(|m| m.device.clone()).unwrap_or_default();
    serde_json::json!({
        "mic": s.mic_on, "mic_can": mic.is_ok(), "mic_why": mic.err().unwrap_or_default(), "mic_device": device(&s.shared),
        "camera": s.camera_on, "camera_can": camera.is_ok(), "camera_why": camera.err().unwrap_or_default(),
    })
}
