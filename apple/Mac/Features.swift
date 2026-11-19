// What the engine can do today. Every control for something it can't do yet reads its flag
// here, and is drawn disabled (and does nothing) while the flag is false. Turn a feature on in
// this one place when the engine has it.
enum Features {
    /// Finding computers on the network: the sidebar's "Nearby" section.
    static let discovery = true
    /// Pairing by comparing a 6-digit code ("Codes Match").
    static let codePairing = false
    /// Online / asleep / not reachable, "last seen", and "What remoteman tried".
    static let reachability = true
    /// "Wake", "Wake and Connect", "Send Wake Signal", "Wake it over the network".
    static let wake = true
    /// The last picture as a thumbnail in the detail header.
    static let thumbnails = true
    /// The computer's sound on this Mac.
    static let sound = true
    static let microphone = true
    static let camera = true
    static let clipboard = true
    /// "Send files", "Save received files to".
    static let fileTransfer = true
    /// The settings that say what sound, clipboard and received files do by default (in a
    /// session each is switched from its control, and remembered for that computer).
    static let defaultsForSharing = true
    /// Automatic / Sharp Text (4:4:4) as a choice ("Picture", "Default quality").
    static let pictureQuality = true
    static let hdr = false
    /// 120 Hz / 60 Hz as a choice, "Use this display's full refresh rate".
    static let refreshRateChoice = true
    /// "Limit Bitrate…".
    static let bitrateLimit = true
    /// The stream's bitrate among the figures.
    static let bitrateFigure = true
    /// "Require Touch ID to connect" and the Touch ID prompt while connecting.
    static let touchID = true
    /// The computer's own login screen when nobody is logged in, and "Fill Password with Touch ID".
    static let remoteLogin = false
    /// Mirroring one of the computer's monitors.
    static let mirrorMonitor = false
    /// "Add a Second Display…".
    static let secondDisplay = false
    /// Which of this Mac's displays to use ("Display: This Mac's display").
    static let displayChoice = false
    /// "Send keys and shortcuts".
    static let sendKeys = true
    /// "⌘ Command sends", "⌃ Control sends", "⌥ Option sends", "Keys are sent".
    static let keyboardMapping = true
    /// "Send system shortcuts to the computer" as a choice.
    static let systemShortcuts = false
    /// The Pointer menu, "Draw the pointer on this Mac", "Pointer movement".
    static let pointerOptions = true
    /// "Scroll direction", "Scroll speed", "Trackpad pinch".
    static let scrollOptions = true
    /// Forgetting a computer also removing its key from this Mac (today only the list entry goes).
    static let forgetKey = true
}
