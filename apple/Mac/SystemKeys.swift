// Shortcuts macOS keeps for itself (⌘Tab, ⌘Space, ⌘Q, ⌘`), sent to the computer instead while
// a session is in full screen, for those who turn that on. Catching them takes an event tap,
// which macOS allows only to apps granted Accessibility; that is asked for when the setting is
// switched on, never before.
import AppKit
import ApplicationServices

final class SystemKeys {
    static let settingKey = "input.systemShortcuts"
    static var wanted: Bool { UserDefaults.standard.bool(forKey: settingKey) }

    /// Whether the keys should go to the computer right now, and the way to send one.
    var active: () -> Bool = { false }
    var send: (_ usage: UInt16, _ down: Bool) -> Void = { _, _ in }
    private var tap: CFMachPort?
    private var source: CFRunLoopSource?
    private var held: Set<Int64> = []
    // Tab, Space, Q, grave: with Command held.
    private static let codes: [Int64: UInt16] = [48: 0x2B, 49: 0x2C, 12: 0x14, 50: 0x35]

    /// Asks macOS for Accessibility if the app doesn't have it. True if it has.
    static func askForAccess() -> Bool {
        AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
    }

    /// Start or stop catching, as the setting and the permission stand.
    func refresh() {
        let want = Self.wanted && AXIsProcessTrusted()
        if want, tap == nil {
            let mask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue)
            let me = Unmanaged.passUnretained(self).toOpaque()
            guard let t = CGEvent.tapCreate(
                tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap, eventsOfInterest: CGEventMask(mask),
                callback: { _, type, event, me in
                    guard let me else { return Unmanaged.passUnretained(event) }
                    let keys = Unmanaged<SystemKeys>.fromOpaque(me).takeUnretainedValue()
                    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                        if let t = keys.tap { CGEvent.tapEnable(tap: t, enable: true) }
                        return Unmanaged.passUnretained(event)
                    }
                    return keys.took(type, event) ? nil : Unmanaged.passUnretained(event)
                }, userInfo: me)
            else { return }
            tap = t
            source = CFMachPortCreateRunLoopSource(nil, t, 0)
            CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
            CGEvent.tapEnable(tap: t, enable: true)
        } else if !want, let t = tap {
            CGEvent.tapEnable(tap: t, enable: false)
            if let source { CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes) }
            tap = nil
            source = nil
            held = []
        }
    }

    /// True if the key was sent to the computer and is not for this Mac.
    private func took(_ type: CGEventType, _ event: CGEvent) -> Bool {
        let code = event.getIntegerValueField(.keyboardEventKeycode)
        guard let usage = Self.codes[code] else { return false }
        if type == .keyUp {
            guard held.remove(code) != nil else { return false }
            send(usage, false)
            return true
        }
        guard active(), event.flags.contains(.maskCommand) else { return false }
        if held.insert(code).inserted { send(usage, true) }
        return true
    }
}
