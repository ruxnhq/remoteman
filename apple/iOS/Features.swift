// What remoteman can do today. Every control for something on this list reads its flag, so a
// feature that lands is switched on here and nowhere else. A control whose flag is false is
// drawn as designed, dimmed, and does nothing.
enum Features {
    /// Computers on this network that aren't paired yet ("Nearby").
    static let discovery = false
    /// Pairing by scanning the computer's QR code, and by comparing six digits.
    static let qrPairing = false
    static let codePairing = false
    /// Knowing whether a computer is online, asleep or out of reach before connecting.
    static let presence = false
    /// Waking a sleeping computer over the network.
    static let wake = false
    /// Face ID before connecting, and filling the login password with it.
    static let faceID = false
    /// The computer's login screen when nobody is logged in, and the choice of way in.
    static let remoteLogin = false
    /// The computer's sound on this device, and this device's microphone and camera on it.
    static let sound = false
    static let microphone = false
    static let camera = false
    static let clipboard = false
    static let files = false
    /// Picture quality, bitrate limit, size ("More space"), HDR.
    static let pictureQuality = false
    static let bitrateLimit = false
    static let displaySize = false
    static let hdr = false
    /// A display on an external monitor, and mirroring one of the computer's monitors.
    static let moreDisplays = false
    /// The finger as a trackpad, and the pointer options that go with it.
    static let trackpadMode = false
    static let pointerOptions = false
    /// Remapping Command, Control and Option, stand-ins for shortcuts iPadOS keeps, editing the key strip.
    static let keyRemapping = false
    static let keyStripEditing = false
    static let shortcutsMenu = false
    /// A second session in a window of its own, and choosing where a window's controls sit.
    static let multiWindow = false
    static let windowControlsChoice = false
    /// Choosing several computers at once.
    static let multiSelect = false

    // What works: adding a computer by address, pairing on first contact, the picture, touch,
    // trackpad and mouse, hardware and on-screen keyboard, the key strip, display placement,
    // performance figures, renaming and forgetting a computer, last-picture thumbnails.
}
