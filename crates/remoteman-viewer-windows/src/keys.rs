//! Keys by where they are on the keyboard: a Windows scancode (set 1, as `WM_KEYDOWN` carries
//! it) to the USB HID usage the protocol speaks.

use remoteman_core::keys;

/// `extended`: the key message's extended bit (the E0 prefix), which tells the right-hand and
/// navigation keys from the ones they share a code with.
pub fn usage(scancode: u16, extended: bool) -> Option<u16> {
    if extended {
        return Some(match scancode {
            0x1C => 0x58, // keypad Enter
            0x1D => 0xE4, // Right Control
            0x35 => 0x54, // keypad /
            0x37 => 0x46, // Print Screen
            0x38 => 0xE6, // Right Alt
            0x45 => 0x53, // Num Lock (Windows marks it extended; Pause is the plain 0x45)
            0x47 => 0x4A, // Home
            0x48 => 0x52, // Up
            0x49 => 0x4B, // Page Up
            0x4B => 0x50, // Left
            0x4D => 0x4F, // Right
            0x4F => 0x4D, // End
            0x50 => 0x51, // Down
            0x51 => 0x4E, // Page Down
            0x52 => 0x49, // Insert
            0x53 => 0x4C, // Delete
            0x5B => 0xE3, // Left Windows
            0x5C => 0xE7, // Right Windows
            0x5D => 0x65, // Menu
            _ => return None,
        });
    }
    match scancode {
        0x45 => Some(0x48), // Pause
        0x54 => Some(0x46), // Print Screen with Alt held
        // The main block: set 1 and Linux's key codes are the same numbers there (Linux took
        // them from the PC keyboard), and the core already knows Linux's.
        0x01..=0x58 => keys::linux_to_hid(scancode),
        // F13 to F24.
        0x64..=0x6E => Some(0x68 + (scancode - 0x64)),
        0x76 => Some(0x73),
        _ => None,
    }
}
