//! `--input-demo`: input sent with nobody at the keyboard, to check the way back to the host.
//! It goes through the same `Client::send` the window uses, not through Windows' own input, so
//! it works from a session with no desktop too.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use remoteman_core::{Button, Client, Event};

use crate::stream::Shared;

pub fn start(shared: Arc<Shared>, client: Arc<Client>) {
    let _ = std::thread::Builder::new().name("input-demo".into()).spawn(move || {
        // The host's display has to be there, and whatever is on it has to have drawn once.
        while shared.presented_total.load(Ordering::Relaxed) == 0 {
            if shared.quit.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_secs(2));
        let pause = || std::thread::sleep(Duration::from_millis(20));
        for i in 0..=40 {
            let t = i as f32 / 40.0;
            client.send(Event::PointerTo { x: 0.2 + 0.3 * t, y: 0.3 + 0.2 * t });
            pause();
        }
        for down in [true, false] {
            client.send(Event::Button { button: Button::Left, down });
            pause();
        }
        // "hi", Enter: by position, as HID usages.
        for usage in [0x0B, 0x0C, 0x28] {
            for down in [true, false] {
                client.send(Event::Key { usage, down });
                pause();
            }
        }
        client.send(Event::Scroll { dx: 0.0, dy: 40.0 });
        std::thread::sleep(Duration::from_millis(500));
        // The host reports its cursor's every move, ours included: the echo of where we put it.
        let (x, y) = *shared.cursor_at.lock().unwrap();
        say!("input demo sent: 41 moves ending at 0.500,0.500, a click, h i Enter, a scroll; the host's cursor is now at {x:.3},{y:.3}");
    });
}
