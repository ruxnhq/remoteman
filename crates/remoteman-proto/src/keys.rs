//! USB HID keyboard usages (page 7) to Linux input key codes, for hosts that inject by key code.

/// The Linux `KEY_*` code for a HID keyboard usage, if there is one.
pub fn hid_to_linux(usage: u16) -> Option<u16> {
    Some(match usage {
        // Letters: HID is alphabetical, Linux follows the keyboard's rows.
        0x04..=0x1D => [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44][(usage - 0x04) as usize],
        0x1E..=0x26 => usage - 0x1E + 2, // 1..9
        0x27 => 11,                      // 0
        0x28 => 28,                      // Enter
        0x29 => 1,                       // Escape
        0x2A => 14,                      // Backspace
        0x2B => 15,                      // Tab
        0x2C => 57,                      // Space
        0x2D => 12,                      // -
        0x2E => 13,                      // =
        0x2F => 26,                      // [
        0x30 => 27,                      // ]
        0x31 => 43,                      // backslash
        0x32 => 43,                      // non-US #
        0x33 => 39,                      // ;
        0x34 => 40,                      // '
        0x35 => 41,                      // `
        0x36 => 51,                      // ,
        0x37 => 52,                      // .
        0x38 => 53,                      // /
        0x39 => 58,                      // Caps Lock
        0x3A..=0x43 => usage - 0x3A + 59, // F1..F10
        0x44 => 87,                      // F11
        0x45 => 88,                      // F12
        0x46 => 99,                      // Print Screen
        0x47 => 70,                      // Scroll Lock
        0x48 => 119,                     // Pause
        0x49 => 110,                     // Insert
        0x4A => 102,                     // Home
        0x4B => 104,                     // Page Up
        0x4C => 111,                     // Delete
        0x4D => 107,                     // End
        0x4E => 109,                     // Page Down
        0x4F => 106,                     // Right
        0x50 => 105,                     // Left
        0x51 => 108,                     // Down
        0x52 => 103,                     // Up
        0x53 => 69,                      // Num Lock
        0x54 => 98,                      // keypad /
        0x55 => 55,                      // keypad *
        0x56 => 74,                      // keypad -
        0x57 => 78,                      // keypad +
        0x58 => 96,                      // keypad Enter
        0x59..=0x5B => usage - 0x59 + 79, // keypad 1..3
        0x5C..=0x5E => usage - 0x5C + 75, // keypad 4..6
        0x5F..=0x61 => usage - 0x5F + 71, // keypad 7..9
        0x62 => 82,                      // keypad 0
        0x63 => 83,                      // keypad .
        0x64 => 86,                      // non-US backslash
        0x65 => 127,                     // Menu
        0x68..=0x73 => usage - 0x68 + 183, // F13..F24
        0xE0 => 29,                      // Left Control
        0xE1 => 42,                      // Left Shift
        0xE2 => 56,                      // Left Alt
        0xE3 => 125,                     // Left Super
        0xE4 => 97,                      // Right Control
        0xE5 => 54,                      // Right Shift
        0xE6 => 100,                     // Right Alt
        0xE7 => 126,                     // Right Super
        _ => return None,
    })
}

/// The HID keyboard usage for a Linux `KEY_*` code, if there is one (for clients on Linux).
pub fn linux_to_hid(code: u16) -> Option<u16> {
    // 0x32 (non-US #) shares a Linux code with backslash; the search finds backslash first.
    (0x04..=0xE7).find(|&usage| hid_to_linux(usage) == Some(code))
}

#[cfg(test)]
mod tests {
    use super::hid_to_linux;

    #[test]
    fn known_keys() {
        assert_eq!(hid_to_linux(0x04), Some(30)); // A
        assert_eq!(hid_to_linux(0x1D), Some(44)); // Z
        assert_eq!(hid_to_linux(0x1E), Some(2)); // 1
        assert_eq!(hid_to_linux(0x27), Some(11)); // 0
        assert_eq!(hid_to_linux(0x28), Some(28)); // Enter
        assert_eq!(hid_to_linux(0x3A), Some(59)); // F1
        assert_eq!(hid_to_linux(0x52), Some(103)); // Up
        assert_eq!(hid_to_linux(0xE3), Some(125)); // Super
        assert_eq!(hid_to_linux(0x00), None);
        assert_eq!(super::linux_to_hid(30), Some(0x04));
        assert_eq!(super::linux_to_hid(43), Some(0x31));
        assert_eq!(super::linux_to_hid(125), Some(0xE3));
        assert_eq!(super::linux_to_hid(0), None);
    }
}
