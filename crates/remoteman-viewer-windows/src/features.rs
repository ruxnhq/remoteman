//! What this build can do. The design has controls for things the product doesn't do yet; they
//! are in the window where the design puts them, switched off, and this is the one place that
//! switches them on. Set a field to `true` once the core (and the code behind the control) can
//! do it. As the Linux viewer's `features.rs`; where the two differ it is because one viewer
//! has the thing built and the other hasn't yet.

use crate::ui::En;

pub struct Features {
    /// Find computers running remoteman on this network (DNS-SD, `nearby.rs`): the list's
    /// "Nearby" group, and the pairing dialog's.
    pub discovery: bool,
    /// Know that a computer is online without connecting to it: a paired one whose key is
    /// advertised on this network. (Asleep can't be told from off: a computer that didn't
    /// answer and can be woken is offered waking instead.)
    pub presence: bool,
    /// Wake a sleeping computer over the network (the magic packet to its advertised cards).
    pub wake: bool,
    /// Pairing by comparing a six-digit code that both sides show.
    pub pairing_code: bool,
    /// The computer's sound played here: the button in the session's bar, and the settings.
    pub sound: bool,
    /// This PC's microphone used by the computer (`mic.rs`, `media.rs`).
    pub microphone: bool,
    /// This PC's camera used by the computer (`camera.rs`, `media.rs`).
    pub camera: bool,
    /// One clipboard for both sides (text): the line in the session's sharing menu, and the
    /// settings.
    pub clipboard: bool,
    /// Files sent to the computer and taken from it.
    pub files: bool,
    /// Choosing where received files go (they go to Downloads).
    pub folder_choice: bool,
    /// HDR pictures.
    pub hdr: bool,
    /// Each frame shown when it arrives, where the monitor can: in full screen, which is a
    /// window of the engine's own with the keyboard and an HWND swap chain that may tear
    /// (`full.rs`, `draw.rs`). Measured on an RTX 5090 with a 240 Hz VRR monitor (`probe.rs`,
    /// and that window with `--watch-vblank`); the app's `SwapChainPanel` never makes the
    /// monitor vary its refresh, so in a window it stays at the monitor's rate.
    #[allow(dead_code)] // (The old program's window doesn't present that way.)
    pub variable_refresh: bool,
    /// A choice of whether the stream follows this monitor's refresh rate (it always does).
    pub refresh_choice: bool,
    /// The other computer's display resized to follow this window.
    pub follow_window: bool,
    /// Full colour detail (4:4:4): HEVC 4:4:4 decoded on the processor (FFmpeg has no D3D11VA
    /// path for it), offered unasked only where the processor keeps up.
    pub sharp_text: bool,
    /// A captured pointer that sends raw movement.
    pub pointer_capture: bool,
    /// "Ctrl acts as Command on a Mac" (the rest of `key_mapping` isn't built).
    pub ctrl_as_command: bool,
    /// A choice of scroll direction.
    pub scroll_direction: bool,
    /// Touchscreen touches sent as touches (`touch.rs`, `engine::touch`).
    pub touch: bool,
    /// Alt+Tab, the Windows key and the like sent to the other computer.
    pub system_shortcuts: bool,
    /// Choosing which keys act as Super, Ctrl and Command.
    pub key_mapping: bool,
    /// A shortcut that gives the keyboard back to this PC.
    pub release_shortcut: bool,
    /// A second display in the same session.
    pub second_display: bool,
    /// Mirroring one of the computer's own monitors.
    pub mirror: bool,
    /// A choice between attaching to a running session and a remote login (the computer
    /// decides).
    pub way_in: bool,
}

pub const FEATURES: Features = Features {
    discovery: true,
    presence: true,
    wake: true,
    pairing_code: true,
    sound: true,
    microphone: true,
    camera: true,
    clipboard: true,
    files: true,
    folder_choice: true,
    hdr: true,
    variable_refresh: true,
    refresh_choice: true,
    follow_window: true,
    sharp_text: true,
    pointer_capture: true,
    ctrl_as_command: true,
    scroll_direction: true,
    touch: true,
    system_shortcuts: true,
    key_mapping: false,
    release_shortcut: true,
    second_display: true,
    mirror: true,
    way_in: false,
};

pub const NOT_YET: &str = "Not available yet";

/// For a control of something that may not exist yet: usable if it does, otherwise greyed out
/// and saying so.
pub fn gate(available: bool) -> En {
    En { on: available, why: if available { "" } else { NOT_YET } }
}
