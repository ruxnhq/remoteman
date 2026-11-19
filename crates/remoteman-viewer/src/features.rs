//! What this build can do. The design has controls for things the product doesn't do yet; they
//! are in the window where the design puts them, switched off, and this is the one place that
//! switches them on. Set a field to `true` once the core (and the code behind the control) can
//! do it.

use gtk::prelude::*;

pub struct Features {
    /// Find computers running remoteman on this network: the list's "Nearby" group.
    pub discovery: bool,
    /// Know that a computer is online or asleep without connecting to it: the state beside each
    /// computer in the list, and the "is asleep" page.
    pub presence: bool,
    /// Wake a sleeping computer over the network.
    pub wake: bool,
    /// Pairing by comparing a six-digit code that both sides show.
    pub pairing_code: bool,
    /// The computer's sound played here: the button in the session's bar.
    pub sound: bool,
    /// Remembering, for all computers and for each, whether sound starts with the session.
    pub sound_setting: bool,
    /// This device's microphone used by the computer: the button in the session's bar.
    pub microphone: bool,
    /// Remembering, for all computers and for each, whether the computer may use it.
    pub microphone_setting: bool,
    /// This device's camera used by the computer.
    pub camera: bool,
    /// One clipboard for both sides: the line in the session's sharing menu.
    pub clipboard: bool,
    /// Files sent to the computer: the line in the session's sharing menu.
    pub files: bool,
    /// Remembering the clipboard choice.
    pub sharing_settings: bool,
    /// Files from the computer, and choosing where they go.
    pub receive_files: bool,
    /// HDR pictures.
    pub hdr: bool,
    /// A choice of whether the stream follows this display's refresh rate (it always does, up
    /// to 120 Hz).
    pub refresh_choice: bool,
    /// A captured pointer that sends raw movement.
    pub pointer_capture: bool,
    /// A choice of whether the pointer is drawn here (it always is).
    pub pointer_choice: bool,
    /// A choice of scroll direction.
    pub scroll_direction: bool,
    /// Touchscreen touches sent as touches.
    pub touch: bool,
    /// Choosing which keys act as Super, Ctrl and Command.
    pub key_mapping: bool,
    /// A shortcut that gives the keyboard back to this computer.
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
    sound_setting: true,
    microphone: true,
    microphone_setting: true,
    camera: true,
    clipboard: true,
    files: true,
    sharing_settings: true,
    receive_files: true,
    hdr: false,
    refresh_choice: true,
    pointer_capture: true,
    pointer_choice: true,
    scroll_direction: true,
    touch: true,
    key_mapping: true,
    release_shortcut: true,
    second_display: false,
    mirror: true,
    way_in: true,
};

pub const NOT_YET: &str = "Not available yet";

/// A control for something that may not exist yet: left alone if it does, otherwise greyed out
/// and saying so.
pub fn gate<W: IsA<gtk::Widget>>(widget: W, available: bool) -> W {
    if !available {
        widget.set_sensitive(false);
        widget.set_tooltip_text(Some(NOT_YET));
    }
    widget
}
