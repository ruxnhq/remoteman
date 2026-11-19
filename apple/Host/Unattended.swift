// Logging in, and unlocking, for a paired device. The Mac's owner gives the host their Mac
// password once; after that a paired device that connects while the Mac is locked, or at its
// login window, is let in without anyone typing: the host fills in the password field itself,
// through the accessibility interface, and the device is shown the desktop. Neither the lock
// screen nor the login window is ever captured, so macOS has nothing to ask.
//
// Where the password is kept, and only if the owner turns this on:
// - for the lock screen: in the owner's login keychain, readable by this app;
// - for the login window (nobody logged in): in a file only root can read, because the agent
//   that runs there is root and no user's keychain is open. Writing it takes an administrator's
//   password.
// It is never sent anywhere and never logged.
import AppKit
import ApplicationServices
import Foundation
import OpenDirectory
import Security

enum Unattended {
    private static let service = "com.xnixy.remoteman.host.unattended"
    static let rootFile = "/Library/Application Support/remoteman-host/unattended"

    // MARK: The password

    /// Whether the owner has turned this on (the lock-screen copy is there).
    static var enabled: Bool {
        let q: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service]
        return SecItemCopyMatching(q as CFDictionary, nil) == errSecSuccess
    }

    /// Whether the login window's copy is there too.
    static var coversLoginWindow: Bool { FileManager.default.fileExists(atPath: rootFile) }

    private static func keychainPassword() -> String? {
        let q: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecReturnData: true]
        var out: CFTypeRef?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let d = out as? Data else { return nil }
        return String(data: d, encoding: .utf8)
    }

    /// The password for where this process runs: root at the login window reads the file.
    private static func password() -> String? {
        if getuid() == 0 {
            guard let d = FileManager.default.contents(atPath: rootFile) else { return nil }
            return String(data: d, encoding: .utf8)
        }
        return keychainPassword()
    }

    /// Whether this is the account's password, asked of the system's own directory. Nothing is
    /// typed anywhere and nothing is kept by asking.
    static func isRight(_ password: String, for user: String = NSUserName()) -> Bool? {
        guard let node = try? ODNode(session: ODSession.default(), type: ODNodeType(kODNodeTypeAuthentication)),
            let record = try? node.record(withRecordType: kODRecordTypeUsers, name: user, attributes: nil)
        else { return nil }
        return (try? record.verifyPassword(password)) != nil
    }

    /// For the log: whether the kept password is still the account's.
    static var keptIsRight: String {
        guard getuid() != 0, let p = keychainPassword() else { return "not checked" }
        switch isRight(p) {
        case .some(true): return "is this account's password"
        case .some(false): return "is NOT this account's password"
        case .none: return "couldn't be checked"
        }
    }

    /// Keep the password (both copies) and install the login window's agent, or take all of it
    /// away. One administrator prompt. Returns what came of it ("" when the owner cancelled).
    static func set(_ password: String?, stateDir: String, listen: String, name: String) -> String {
        let base: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service]
        var commands: [String] = []
        var temp: URL?
        if let password {
            // A wrong password would be typed into the lock screen again and again: refused here.
            if isRight(password) == false { return "That isn't this Mac account's password. Nothing was changed." }
            guard let binary = Bundle.main.executablePath, binary.hasPrefix("/Applications/") else {
                return "Move remoteman host into the Applications folder first: the login window starts it from there."
            }
            // The root copy goes through a file only this user can read, then only root.
            let t = FileManager.default.temporaryDirectory.appendingPathComponent("remoteman-\(UUID().uuidString)")
            guard FileManager.default.createFile(atPath: t.path, contents: Data(password.utf8), attributes: [.posixPermissions: 0o600]) else {
                return "It didn't work: the password couldn't be handed over."
            }
            temp = t
            guard let agent = LoginWindow.agentDescription(binary: binary, stateDir: stateDir, listen: listen, name: name) else {
                try? FileManager.default.removeItem(at: t)
                return "It didn't work: the agent's description couldn't be written."
            }
            commands = [
                "mkdir -p '/Library/Application Support/remoteman-host'",
                "install -m 600 -o root -g wheel '\(t.path)' '\(rootFile)'",
                "cp '\(agent.path)' '\(LoginWindow.plist)'", "chown root:wheel '\(LoginWindow.plist)'", "chmod 644 '\(LoginWindow.plist)'",
            ]
        } else {
            commands = ["rm -f '\(rootFile)' '\(LoginWindow.plist)'"]
        }
        let shell = commands.joined(separator: " && ").replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        var error: NSDictionary?
        NSAppleScript(source: "do shell script \"\(shell)\" with administrator privileges")?.executeAndReturnError(&error)
        if let temp { try? FileManager.default.removeItem(at: temp) }
        if let error {
            let cancelled = (error[NSAppleScript.errorNumber] as? Int) == -128
            return cancelled ? "" : "It didn't work: \(error[NSAppleScript.errorMessage] as? String ?? "unknown")"
        }
        SecItemDelete(base as CFDictionary)
        if let password {
            var add = base
            add[kSecAttrAccount] = NSUserName()
            add[kSecValueData] = Data(password.utf8)
            add[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            let status = SecItemAdd(add as CFDictionary, nil)
            if status != errSecSuccess { return "Kept for the login window, but not for the lock screen (keychain error \(status))." }
        }
        hostLog("host: logging in for paired devices is \(password == nil ? "off" : "on")")
        return password == nil ? "Off. The password was removed from this Mac." : "On."
    }

    // MARK: The screens

    static var screenLocked: Bool {
        guard let d = CGSessionCopyCurrentDictionary() as? [String: Any] else { return false }
        return (d["CGSSessionScreenIsLocked"] as? Bool) ?? false
    }

    /// Fill in the password field the Mac is showing (its lock screen, or its login window) and
    /// confirm it. Returns nil when a field was found and filled, or why not. The words say
    /// what was found, never what was typed.
    static func submit() -> String? {
        guard let password = password() else { return "no password is kept for this" }
        guard let pid = NSWorkspace.shared.runningApplications.first(where: { $0.bundleIdentifier == "com.apple.loginwindow" })?.processIdentifier
            ?? pidOfLoginWindow()
        else { return "the login window's process wasn't found" }
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(app, 2)
        // A screen that has gone dark, or a lock screen showing only the clock, has no field
        // yet: wake it as a key press would, and look again for a few seconds.
        var seen: [String: Int] = [:]
        var found: AXUIElement?
        for attempt in 0..<8 {
            seen = [:]
            found = find(app, role: "AXTextField", subrole: "AXSecureTextField", depth: 0, seen: &seen)
            if found != nil { break }
            // A lock screen left alone fades and puts its form away: activity, a pointer move
            // and a click in the middle of the screen bring it back.
            HostInput.wake()
            if attempt % 2 == 0 {
                let b = CGDisplayBounds(CGMainDisplayID())
                click(CGPoint(x: b.midX, y: b.midY))
                stroke(app, 53, shift: false)
            }
            usleep(500_000)
        }
        if found == nil {
            // A lock screen that shows no field to the accessibility interface (only its clock,
            // or nothing readable) still takes keys: Escape brings the form up and empties it,
            // then the password is typed as a keyboard would.
            let was = whoIsOnScreen()
            if let strokes = keystrokes(for: password) {
                stroke(app, 53, shift: false)
                usleep(400_000)
                for k in strokes { stroke(app, k.code, shift: k.shift) }
                usleep(150_000)
                stroke(app, 36, shift: false)
                if waitForChange(from: was, seconds: 4) {
                    hostLog("host: the lock screen took the password as keys to its own process, with no field in sight")
                    return nil
                }
            }
        }
        guard let field = found else {
            let summary = seen.sorted { $0.key < $1.key }.map { "\($0.key)×\($0.value)" }.joined(separator: " ")
            return "no password field was found (\(summary.isEmpty ? "nothing readable: is Accessibility allowed?" : summary))"
        }
        AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        let was = whoIsOnScreen()
        var tried: [String] = []
        // First, as a person at a keyboard does it (and as it is known to work on a lock
        // screen): a click in the field, the password as ordinary key presses, Return.
        if let strokes = keystrokes(for: password), let at = centre(of: field) {
            HostInput.wake()
            click(at)
            usleep(250_000)
            typeKeys(strokes)
            usleep(120_000)
            var held: CFTypeRef?
            AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &held)
            pressReturn()
            if waitForChange(from: was, seconds: 3) {
                hostLog("host: the lock screen took a click and key presses")
                return nil
            }
            var after: CFTypeRef?
            AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &after)
            tried.append("click at \(Int(at.x)),\(Int(at.y)) and key presses: field held \((held as? String)?.count ?? -1) of \(password.count), then \((after as? String)?.count ?? -1), nothing")
        }
        // Then: keys sent to the login window's own process through the accessibility
        // interface. Text put into the field, and keys posted to the system, show in the field
        // but are not taken as typing by the lock screen; these are.
        if let strokes = keystrokes(for: password) {
            stroke(app, 53, shift: false)  // Escape empties the field
            usleep(200_000)
            for k in strokes { stroke(app, k.code, shift: k.shift) }
            usleep(150_000)
            var held: CFTypeRef?
            AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &held)
            stroke(app, 36, shift: false)  // Return
            if waitForChange(from: was, seconds: 3) {
                hostLog("host: the lock screen took the password as keys to its own process")
                return nil
            }
            tried.append("keys to the login window's process: field held \((held as? String)?.count ?? -1) of \(password.count), nothing")
        } else {
            tried.append("keys to the login window's process: the password has characters outside a US keyboard")
        }
        let set = AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, password as CFString)
        var back: CFTypeRef?
        AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &back)
        // How much the field says it holds (a secure field gives back dots), never what.
        tried.append("set \(set == .success ? "accepted" : "refused \(set.rawValue)"), field holds \((back as? String)?.count ?? -1) of \(password.count)")
        // Each way of confirming in turn, so that the log says which one this macOS takes.
        let source = CGEventSource(stateID: .hidSystemState)
        // The form's own button, on every screen the form is shown on.
        var buttons: [AXUIElement] = []
        collect(app, depth: 0, into: &buttons) { role, label in role == "AXButton" && label.lowercased() == "login" }
        let ways: [(String, () -> Void)] = [
            ("the login button (\(buttons.count) found)", { for b in buttons { AXUIElementPerformAction(b, kAXPressAction as CFString) } }),
            ("the field's confirm", { AXUIElementPerformAction(field, kAXConfirmAction as CFString) }),
            ("Return to the system", { pressReturn() }),
            ("Return to the session", {
                for down in [true, false] {
                    CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: down)?.post(tap: .cgSessionEventTap)
                    usleep(12_000)
                }
            }),
            ("Return to the login window itself", {
                for down in [true, false] {
                    CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: down)?.postToPid(pid)
                    usleep(12_000)
                }
            }),
        ]
        for (name, act) in ways {
            act()
            if waitForChange(from: was, seconds: 1.5) {
                hostLog("host: the password field was confirmed by \(name) (\(tried.joined(separator: "; ")))")
                return nil
            }
            // A form that took the submission and turned it down empties its field.
            AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &back)
            tried.append("\(name): nothing, field then holds \((back as? String)?.count ?? -1)")
        }
        // Last: the field emptied and the password typed as keys, as a keyboard would.
        AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, "" as CFString)
        AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        usleep(150_000)
        type(password)
        AXUIElementCopyAttributeValue(field, kAXValueAttribute as CFString, &back)
        tried.append("typed as keys, field holds \((back as? String)?.count ?? -1)")
        for b in buttons { AXUIElementPerformAction(b, kAXPressAction as CFString) }
        if !waitForChange(from: was, seconds: 1.5) { pressReturn() }
        if waitForChange(from: was, seconds: 2.5) {
            hostLog("host: the password field took typed keys (\(tried.joined(separator: "; ")))")
            return nil
        }
        var labels: [String] = []
        describe(app, depth: 0, into: &labels)
        return "the screen didn't change. Tried: \(tried.joined(separator: "; ")). On screen: \(labels.prefix(40).joined(separator: ", "))"
    }

    private static func collect(_ e: AXUIElement, depth: Int, into out: inout [AXUIElement], where wanted: (String, String) -> Bool) {
        guard depth < 14 else { return }
        var r: CFTypeRef?, d: CFTypeRef?, t: CFTypeRef?
        AXUIElementCopyAttributeValue(e, kAXRoleAttribute as CFString, &r)
        AXUIElementCopyAttributeValue(e, kAXDescriptionAttribute as CFString, &d)
        AXUIElementCopyAttributeValue(e, kAXTitleAttribute as CFString, &t)
        let label = [(t as? String), (d as? String)].compactMap { $0 }.first { !$0.isEmpty } ?? ""
        if wanted(r as? String ?? "", label) { out.append(e) }
        var kids: CFTypeRef?
        for attribute in [kAXChildrenAttribute, kAXWindowsAttribute] {
            guard AXUIElementCopyAttributeValue(e, attribute as CFString, &kids) == .success, let list = kids as? [AXUIElement] else { continue }
            for k in list { collect(k, depth: depth + 1, into: &out, where: wanted) }
        }
    }

    /// The controls the login window shows, by kind and label (labels are not secrets), so the
    /// log can say what there was to press.
    private static func describe(_ e: AXUIElement, depth: Int, into out: inout [String]) {
        guard depth < 14, out.count < 60 else { return }
        var r: CFTypeRef?, s: CFTypeRef?, d: CFTypeRef?, t: CFTypeRef?
        AXUIElementCopyAttributeValue(e, kAXRoleAttribute as CFString, &r)
        AXUIElementCopyAttributeValue(e, kAXSubroleAttribute as CFString, &s)
        AXUIElementCopyAttributeValue(e, kAXDescriptionAttribute as CFString, &d)
        AXUIElementCopyAttributeValue(e, kAXTitleAttribute as CFString, &t)
        let role = r as? String ?? "?"
        var v: CFTypeRef?
        // What a line of text on the form says (never a text field's content).
        if role == "AXStaticText" { AXUIElementCopyAttributeValue(e, kAXValueAttribute as CFString, &v) }
        if ["AXButton", "AXTextField", "AXStaticText", "AXImage"].contains(role) {
            let label = [(s as? String), (t as? String), (d as? String), (v as? String)].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: "/")
            out.append("\(role.dropFirst(2))(\(label.prefix(30)))")
        }
        var kids: CFTypeRef?
        for attribute in [kAXChildrenAttribute, kAXWindowsAttribute] {
            guard AXUIElementCopyAttributeValue(e, attribute as CFString, &kids) == .success, let list = kids as? [AXUIElement] else { continue }
            for k in list { describe(k, depth: depth + 1, into: &out) }
        }
    }

    private static func centre(of e: AXUIElement) -> CGPoint? {
        var p: CFTypeRef?, z: CFTypeRef?
        guard AXUIElementCopyAttributeValue(e, kAXPositionAttribute as CFString, &p) == .success,
            AXUIElementCopyAttributeValue(e, kAXSizeAttribute as CFString, &z) == .success
        else { return nil }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(z as! AXValue, .cgSize, &size), size.width > 0 else { return nil }
        return CGPoint(x: point.x + size.width / 2, y: point.y + size.height / 2)
    }

    private static func click(_ at: CGPoint) {
        let source = CGEventSource(stateID: .hidSystemState)
        for type in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
            let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: at, mouseButton: .left)
            if type != .mouseMoved { e?.setIntegerValueField(.mouseEventClickState, value: 1) }
            post(e)
            usleep(30_000)
        }
    }

    /// Key presses with their own key codes, Shift held where a character needs it.
    private static func typeKeys(_ strokes: [(code: CGKeyCode, shift: Bool)]) {
        let source = CGEventSource(stateID: .hidSystemState)
        for k in strokes {
            if k.shift {
                let e = CGEvent(keyboardEventSource: source, virtualKey: 56, keyDown: true)
                e?.type = .flagsChanged
                e?.flags = .maskShift
                post(e)
            }
            for down in [true, false] {
                let e = CGEvent(keyboardEventSource: source, virtualKey: k.code, keyDown: down)
                e?.flags = k.shift ? .maskShift : []
                post(e)
            }
            if k.shift {
                let e = CGEvent(keyboardEventSource: source, virtualKey: 56, keyDown: false)
                e?.type = .flagsChanged
                e?.flags = []
                post(e)
            }
            usleep(20_000)
        }
    }

    /// One key, down and up, to an application through the accessibility interface.
    private static func stroke(_ app: AXUIElement, _ code: CGKeyCode, shift: Bool) {
        if shift { RMPostKey(app, 56, true) }
        RMPostKey(app, code, true)
        usleep(15_000)
        RMPostKey(app, code, false)
        if shift { RMPostKey(app, 56, false) }
        usleep(25_000)
    }

    /// The keys of a US keyboard that type this text, or nil if it has a character that isn't on one.
    private static func keystrokes(for text: String) -> [(code: CGKeyCode, shift: Bool)]? {
        let plain: [Character: CGKeyCode] = [
            "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15,
            "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
            "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "l": 37, "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44,
            "n": 45, "m": 46, ".": 47, "`": 50, " ": 49,
        ]
        let shifted: [Character: Character] = [
            "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0", "_": "-", "+": "=",
            "{": "[", "}": "]", "|": "\\", ":": ";", "\"": "'", "<": ",", ">": ".", "?": "/", "~": "`",
        ]
        var out: [(code: CGKeyCode, shift: Bool)] = []
        for ch in text {
            if let c = plain[ch] {
                out.append((c, false))
            } else if ch.isLetter, ch.isUppercase, let c = plain[Character(ch.lowercased())] {
                out.append((c, true))
            } else if let base = shifted[ch], let c = plain[base] {
                out.append((c, true))
            } else {
                return nil
            }
        }
        return out
    }

    /// What tells a successful unlock or log in: the lock goes, or someone is on the console.
    private static func whoIsOnScreen() -> String { "\(screenLocked) \(LoginWindow.showing)" }

    private static func waitForChange(from was: String, seconds: Double) -> Bool {
        var waited = 0.0
        while waited < seconds {
            if whoIsOnScreen() != was { return true }
            usleep(150_000)
            waited += 0.15
        }
        return false
    }

    private static func post(_ e: CGEvent?) {
        e?.post(tap: .cghidEventTap)
        usleep(12_000)
    }

    private static func pressReturn() {
        let source = CGEventSource(stateID: .hidSystemState)
        post(CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: true))
        post(CGEvent(keyboardEventSource: source, virtualKey: 36, keyDown: false))
    }

    /// Each character as a key press carrying that character, so no keyboard layout is assumed.
    private static func type(_ text: String) {
        let source = CGEventSource(stateID: .hidSystemState)
        for ch in text {
            let units = Array(String(ch).utf16)
            for down in [true, false] {
                let e = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                e?.flags = []
                units.withUnsafeBufferPointer { e?.keyboardSetUnicodeString(stringLength: $0.count, unicodeString: $0.baseAddress) }
                post(e)
            }
        }
    }

    private static func pidOfLoginWindow() -> pid_t? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        p.arguments = ["-x", "loginwindow"]
        let out = Pipe()
        p.standardOutput = out
        guard (try? p.run()) != nil else { return nil }
        p.waitUntilExit()
        let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        return text.split(separator: "\n").compactMap { pid_t($0) }.first
    }

    private static func find(_ e: AXUIElement, role: String, subrole: String, depth: Int, seen: inout [String: Int]) -> AXUIElement? {
        guard depth < 14 else { return nil }
        var r: CFTypeRef?, s: CFTypeRef?
        AXUIElementCopyAttributeValue(e, kAXRoleAttribute as CFString, &r)
        AXUIElementCopyAttributeValue(e, kAXSubroleAttribute as CFString, &s)
        let roleName = r as? String ?? "?"
        seen[roleName, default: 0] += 1
        if (s as? String) == subrole || roleName == subrole { return e }
        var kids: CFTypeRef?
        for attribute in [kAXChildrenAttribute, kAXWindowsAttribute] {
            guard AXUIElementCopyAttributeValue(e, attribute as CFString, &kids) == .success, let list = kids as? [AXUIElement] else { continue }
            for k in list {
                if let hit = find(k, role: role, subrole: subrole, depth: depth + 1, seen: &seen) { return hit }
            }
        }
        return nil
    }
}
