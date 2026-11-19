// What the Mac host can do today. Every control for something it can't do yet reads its flag
// here, and is drawn disabled (and does nothing) while the flag is false. Turn a feature on in
// this one place when the engine has it.
enum Features {
    /// The QR code in "Pair a Device", for iPad and iPhone.
    static let qrPairing = false
    /// The 6-digit code in "Pair a Device", compared on both sides.
    static let codePairing = true
    /// The list of paired devices and "Revoke…" (the core keeps the list to itself so far).
    static let trustedDevices = true
    /// "Open remoteman at login".
    static let openAtLogin = true
    /// "Default for a new device" and "Devices may choose" (today the device chooses, freely).
    static let placementPolicy = true
    /// "Lock this Mac when the device leaves".
    static let lockOnLeave = true
    /// "Notify me when a device connects".
    static let notifyOnConnect = true
    /// "Use the device's camera and microphone on this Mac", and their lines in the panel.
    static let deviceCameraAndMicrophone = true
    /// This Mac's sound on the device.
    static let sound = true
    /// "Bitrate limit".
    static let bitrateLimit = true
    /// Sharp text (4:4:4): the Mac's hardware encoder has no 4:4:4 HEVC, so this is never offered.
    static let sharpText = false
    /// "10-bit colour" as a choice (the picture is always 10-bit today).
    static let tenBitChoice = true
}
