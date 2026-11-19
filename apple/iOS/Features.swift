// What remoteman can do today. Every control for something on this list reads its flag, so a
// feature that lands is switched on here and nowhere else. A control whose flag is false is
// drawn as designed, dimmed, and does nothing.
enum Features {
    /// Computers on this network that aren't paired yet ("Nearby").
    static let discovery = true
    /// Pairing by scanning the computer's QR code, and by comparing six digits.
    static let qrPairing = true
    static let codePairing = true
    /// Knowing whether a computer is online, asleep or out of reach before connecting.
    static let presence = true
    /// Waking a sleeping computer over the network.
    static let wake = true
    /// Face ID before connecting, and filling the login password with it.
    static let faceID = true
    /// The computer's login screen when nobody is logged in, and the choice of way in.
    static let remoteLogin = false
    /// The computer's sound on this device, and this device's microphone and camera on it.
    static let sound = true
    static let microphone = true
    static let camera = true
    static let clipboard = true
    static let files = true
    /// Picture quality, bitrate limit, size ("More space"), HDR.
    static let pictureQuality = true
    static let bitrateLimit = true
    static let displaySize = true
    static let hdr = false
    /// Choosing where the sound plays and whether it starts by itself; a lower limit on mobile data.
    static let soundChoice = true
    static let mobileData = true
    /// A question each time before the camera is shared.
    static let cameraAsk = true
    /// A display on an external monitor, and mirroring one of the computer's monitors.
    static let moreDisplays = false
    /// The computer's main monitor as it is ("mirror"), on a computer that lists it.
    static let mirror = true
    /// The finger as a trackpad, and the pointer options that go with it.
    static let trackpadMode = true
    static let pointerOptions = true
    /// Pinching to zoom this device's view of the picture.
    static let viewZoom = true
    /// Remapping Command, Control and Option, stand-ins for shortcuts iPadOS keeps, editing the key strip.
    static let keyRemapping = true
    static let keyStripEditing = true
    static let shortcutsMenu = false
    /// A second session in a window of its own, and choosing where a window's controls sit.
    static let multiWindow = true
    static let windowControlsChoice = false
    /// Choosing several computers at once.
    static let multiSelect = true

    // sound, microphone, clipboard, files, bitrateLimit and displaySize are built here; each is
    // offered only when the connected computer lists it (ViewerSession.has).
    // What works: adding a computer by address, pairing on first contact, the picture, touch,
    // trackpad and mouse, hardware and on-screen keyboard, the key strip, display placement,
    // performance figures, renaming and forgetting a computer, last-picture thumbnails.
}
