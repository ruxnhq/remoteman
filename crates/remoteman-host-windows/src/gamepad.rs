//! A controller held at the device, as a virtual Xbox-style controller here: Windows' own
//! `InputInjector` (Windows.UI.Input.Preview.Injection) with its gamepad injection. Windows takes
//! one injected gamepad, so pad 0 of the device is what is passed on.
//!
//! Where it works: see `available` (it is tried once, and the host lists `gamepad` only if it
//! worked).

use remoteman_core::Event;
use windows::Gaming::Input::GamepadButtons;
use windows::UI::Input::Preview::Injection::{InjectedInputGamepadInfo, InputInjector};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

/// XInput's button mask (what the device sends) to Windows.Gaming.Input's. The Guide button has
/// no place in it.
fn buttons(mask: u16) -> GamepadButtons {
    const MAP: [(u16, GamepadButtons); 14] = [
        (0x0001, GamepadButtons::DPadUp),
        (0x0002, GamepadButtons::DPadDown),
        (0x0004, GamepadButtons::DPadLeft),
        (0x0008, GamepadButtons::DPadRight),
        (0x0010, GamepadButtons::Menu),
        (0x0020, GamepadButtons::View),
        (0x0040, GamepadButtons::LeftThumbstick),
        (0x0080, GamepadButtons::RightThumbstick),
        (0x0100, GamepadButtons::LeftShoulder),
        (0x0200, GamepadButtons::RightShoulder),
        (0x1000, GamepadButtons::A),
        (0x2000, GamepadButtons::B),
        (0x4000, GamepadButtons::X),
        (0x8000, GamepadButtons::Y),
    ];
    let mut out = GamepadButtons::None;
    for (bit, b) in MAP {
        if mask & bit != 0 {
            out |= b;
        }
    }
    out
}

/// Whether the injector can be made and its gamepad initialized in this process. Made once and
/// asked once: the answer is the same for the whole run.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| match Pad::start() {
        Ok(_) => true,
        Err(e) => {
            tracing::info!("no virtual game controller ({e}); `gamepad` is not offered");
            false
        }
    })
}

/// The virtual controller: initialized when a pad connects, uninitialized when it goes.
struct Pad(InputInjector);

// SAFETY: InputInjector is a free-threaded (MTA, agile) Windows runtime class; the session's
// handler thread makes it and uses it, and nothing else holds it.
unsafe impl Send for Pad {}

impl Pad {
    fn start() -> windows::core::Result<Self> {
        // SAFETY: plain call. (Already initialized in another mode is fine: the result is ignored.)
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let injector = InputInjector::TryCreate()?;
        injector.InitializeGamepadInjection()?;
        Ok(Self(injector))
    }

    fn inject(&self, buttons_: u16, lt: f32, rt: f32, lx: f32, ly: f32, rx: f32, ry: f32) -> windows::core::Result<()> {
        let info = InjectedInputGamepadInfo::new()?;
        info.SetButtons(buttons(buttons_))?;
        info.SetLeftTrigger(lt.clamp(0.0, 1.0) as f64)?;
        info.SetRightTrigger(rt.clamp(0.0, 1.0) as f64)?;
        info.SetLeftThumbstickX(lx.clamp(-1.0, 1.0) as f64)?;
        info.SetLeftThumbstickY(ly.clamp(-1.0, 1.0) as f64)?;
        info.SetRightThumbstickX(rx.clamp(-1.0, 1.0) as f64)?;
        info.SetRightThumbstickY(ry.clamp(-1.0, 1.0) as f64)?;
        self.0.InjectGamepadInput(&info)
    }
}

impl Drop for Pad {
    fn drop(&mut self) {
        let _ = self.0.UninitializeGamepadInjection();
    }
}

/// What the device's controllers do here, for one session.
#[derive(Default)]
pub struct Gamepad {
    pad: Option<Pad>,
    /// Said once: another pad than the first, or an injection that failed.
    said_extra: bool,
    said_failed: bool,
}

impl Gamepad {
    /// A state from the device (an `Event::Gamepad`; any other event is ignored).
    pub fn inject(&mut self, event: &Event) {
        let Event::Gamepad { pad, flags, buttons, left_trigger, right_trigger, left_x, left_y, right_x, right_y } = *event else { return };
        if pad != 0 {
            if !self.said_extra {
                self.said_extra = true;
                tracing::info!(pad, "Windows takes one injected game controller: only the first of the device's is passed on. Said once.");
            }
            return;
        }
        if flags & 1 == 0 {
            self.pad = None;
            return;
        }
        if self.pad.is_none() {
            match Pad::start() {
                Ok(p) => self.pad = Some(p),
                Err(e) => {
                    if !self.said_failed {
                        self.said_failed = true;
                        tracing::warn!("game controller injection can't start ({e}). Said once.");
                    }
                    return;
                }
            }
        }
        if let Some(p) = &self.pad {
            if let Err(e) = p.inject(buttons, left_trigger, right_trigger, left_x, left_y, right_x, right_y) {
                if !self.said_failed {
                    self.said_failed = true;
                    tracing::warn!("Windows didn't take a game controller state ({e}). Said once.");
                }
            }
        }
    }

    /// The device leaves: the virtual controller goes, with nothing held on it.
    pub fn release(&mut self) {
        self.pad = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Gaming::Input::Gamepad as WgiPad;
    use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XInputGetState};

    fn xinput() -> Vec<(u32, XINPUT_STATE)> {
        (0..4u32)
            .filter_map(|i| {
                let mut s = XINPUT_STATE::default();
                // SAFETY: a slot number and a state to fill.
                (unsafe { XInputGetState(i, &mut s) } == 0).then_some((i, s))
            })
            .collect()
    }

    /// Makes the virtual controller (the only thing this injects, and only this test sees it),
    /// presses A for 100 ms, reads it back through XInput and Windows.Gaming.Input, and removes it.
    /// Ignored by default: run with `-- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn a_virtual_controller_can_be_made_pressed_and_removed() {
        // SAFETY: plain call.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let added = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let a2 = added.clone();
        let _token = WgiPad::GamepadAdded(&windows::Foundation::EventHandler::<WgiPad>::new(move |_, _| {
            a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }))
        .unwrap();
        // Gaming Input hands controllers to the window that has focus: a small window of this process.
        let _window = {
            use windows::Win32::UI::WindowsAndMessaging::*;
            use windows::core::w;
            // SAFETY: a plain STATIC window, destroyed with the process.
            let w = unsafe { CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("pad test"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 200, 120, None, None, None, None) }.expect("window");
            // SAFETY: the window is ours.
            unsafe {
                let _ = SetForegroundWindow(w);
            }
            w
        };
        let pump = || {
            use windows::Win32::UI::WindowsAndMessaging::*;
            let mut m = MSG::default();
            // SAFETY: draining this thread's queue.
            unsafe {
                while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&m);
                    DispatchMessageW(&m);
                }
            }
        };
        let before = xinput().len();
        let wgi_before = WgiPad::Gamepads().map(|g| g.Size().unwrap_or(0)).unwrap_or(0);
        let pad = Pad::start().expect("TryCreate + InitializeGamepadInjection");
        pad.inject(0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).expect("a neutral state");
        // Windows takes a moment to make the device; keep the state coming meanwhile.
        for _ in 0..40 {
            pad.inject(0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
            pump();
            std::thread::sleep(std::time::Duration::from_millis(100));
            if !xinput().is_empty() || WgiPad::Gamepads().map(|g| g.Size().unwrap_or(0)).unwrap_or(0) > 0 {
                break;
            }
        }
        println!("xinput slots {before} -> {}", xinput().len());
        let wgi = WgiPad::Gamepads().map(|g| g.Size().unwrap_or(0)).unwrap_or(0);
        println!("Windows.Gaming.Input gamepads {wgi_before} -> {wgi}; GamepadAdded fired {} times; raw controllers {}", added.load(std::sync::atomic::Ordering::SeqCst), windows::Gaming::Input::RawGameController::RawGameControllers().map(|g| g.Size().unwrap_or(0)).unwrap_or(0));
        pad.inject(0x1000 | 0x0001, 0.5, 0.0, 0.5, -0.5, 0.0, 0.0).expect("A and dpad up, trigger, stick");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let pressed = xinput();
        for (i, s) in &pressed {
            println!("slot {i}: buttons {:#06x} lt {} lx {} ly {}", s.Gamepad.wButtons.0, s.Gamepad.bLeftTrigger, s.Gamepad.sThumbLX, s.Gamepad.sThumbLY);
        }
        if let Ok(g) = WgiPad::Gamepads() {
            for p in g {
                if let Ok(r) = p.GetCurrentReading() {
                    println!("WGI: buttons {:?} lt {} lx {} ly {}", r.Buttons, r.LeftTrigger, r.LeftThumbstickX, r.LeftThumbstickY);
                }
            }
        }
        pad.inject(0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
        drop(pad);
        std::thread::sleep(std::time::Duration::from_millis(300));
        println!("after uninitialize: xinput {}, WGI {}", xinput().len(), WgiPad::Gamepads().map(|g| g.Size().unwrap_or(0)).unwrap_or(0));
    }

    #[test]
    fn xinput_buttons_map_to_windows_gaming_input() {
        assert_eq!(buttons(0x1000 | 0x0001 | 0x0400), GamepadButtons::A | GamepadButtons::DPadUp);
        assert_eq!(buttons(0), GamepadButtons::None);
        assert_eq!(buttons(0x0010 | 0x0020), GamepadButtons::Menu | GamepadButtons::View);
    }
}
