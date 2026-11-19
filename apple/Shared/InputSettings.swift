// How this device's keyboard, pointer and scrolling reach the computer. The choices are kept in
// the app's defaults under these names; the settings screens bind to the same names.
import Foundation

enum InputSettings {
    static let commandKey = "input.commandSends"  // "super" (as it is) or "ctrl"
    static let controlKey = "input.controlSends"  // "ctrl" (as it is) or "super"
    static let naturalScrollKey = "input.naturalScroll"  // true: as on this device
    static let scrollSpeedKey = "input.scrollSpeed"  // 0…1, 0.5 is as it comes
    static let pinchZoomsKey = "input.pinchZooms"
    static let localPointerKey = "input.localPointer"  // the Mac draws the pointer itself
    static let capturedKey = "input.pointerCaptured"
    static let viewZoomKey = "input.viewZoom"  // a pinch magnifies this device's view
    static let touchModeKey = "input.touchMode"  // "direct" or "trackpad"
    static let tapToClickKey = "input.tapToClick"
    static let twoFingerRightKey = "input.twoFingerRight"
    static let pointerSpeedKey = "input.pointerSpeed"  // "slow", "medium", "fast"

    private static var d: UserDefaults { .standard }
    private static func flag(_ key: String, default value: Bool) -> Bool { d.object(forKey: key) as? Bool ?? value }

    static var commandSendsCtrl: Bool { d.string(forKey: commandKey) == "ctrl" }
    static var controlSendsSuper: Bool { d.string(forKey: controlKey) == "super" }
    static var naturalScroll: Bool { flag(naturalScrollKey, default: true) }
    static var pinchZooms: Bool { flag(pinchZoomsKey, default: true) }
    static var localPointer: Bool { flag(localPointerKey, default: true) }
    static var captured: Bool { flag(capturedKey, default: false) }
    static var trackpadTouch: Bool { d.string(forKey: touchModeKey) == "trackpad" }
    static var viewZoom: Bool { flag(viewZoomKey, default: true) }
    static var tapToClick: Bool { flag(tapToClickKey, default: true) }
    static var twoFingerRight: Bool { flag(twoFingerRightKey, default: true) }
    /// 0.25× at the slow end, 4× at the fast end.
    static var scrollFactor: Float {
        let v = d.object(forKey: scrollSpeedKey) as? Double ?? 0.5
        return Float(pow(4, (min(max(v, 0), 1) - 0.5) * 2))
    }
    static var pointerFactor: CGFloat {
        switch d.string(forKey: pointerSpeedKey) {
        case "slow": return 0.7
        case "fast": return 2.0
        default: return 1.25
        }
    }

    /// The key that goes to the computer for a key pressed here. A Mac gets the keys unchanged;
    /// elsewhere Command and Control are sent as chosen (left stays left, right stays right).
    static func map(_ usage: UInt16, hostOS: String) -> UInt16 {
        guard hostOS != "macos" else { return usage }
        switch usage {
        case 0xE3 where commandSendsCtrl: return 0xE0
        case 0xE7 where commandSendsCtrl: return 0xE4
        case 0xE0 where controlSendsSuper: return 0xE3
        case 0xE4 where controlSendsSuper: return 0xE7
        default: return usage
        }
    }
}

/// What a session starts with, and what guards starting one.
enum ViewerPrefs {
    static let qualityKey = "picture.quality"  // "auto" or "sharp" (4:4:4 first)
    static let fullRefreshKey = "picture.fullRefresh"  // the Mac: ask for the display's own rate, not 60
    static let soundDefaultKey = "sound.default"  // for a computer where it was never chosen
    static let clipboardDefaultKey = "clipboard.default"
    static let rememberMediaKey = "media.remember"  // microphone and camera come back on as they were left
    static let saveDataKey = "mobile.saveData"
    static let requireAuthKey = "security.requireAuth"
    static let askCameraKey = "camera.ask"
    /// What the picture may use on mobile data when data is being saved.
    static let mobileLimit = 5_000_000

    private static var d: UserDefaults { .standard }
    static var sharp: Bool { d.string(forKey: qualityKey) == "sharp" }
    static var fullRefresh: Bool { d.object(forKey: fullRefreshKey) as? Bool ?? true }
    static var soundDefault: Bool { d.bool(forKey: soundDefaultKey) }
    static var clipboardDefault: Bool { d.bool(forKey: clipboardDefaultKey) }
    static var rememberMedia: Bool { d.bool(forKey: rememberMediaKey) }
    static var saveData: Bool { d.object(forKey: saveDataKey) as? Bool ?? true }
    static var requireAuth: Bool { d.bool(forKey: requireAuthKey) }
}
