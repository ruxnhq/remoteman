// Unlocking, and logging in, for a paired device. The Mac's owner gives the host their Mac
// password once; after that a paired device that connects while the Mac is locked, or while it
// shows its login window, is let in without anyone typing, and is shown the desktop. Neither
// screen is ever captured (macOS would ask at the Mac itself, with a prompt that takes no
// posted clicks).
//
// What each screen takes, found by trying everything on macOS 27:
// - The lock screen (owner logged in, this app in their session): a click in the password
//   field, the password as key presses with their own key codes, Return. Text put into the
//   field through the accessibility interface shows there but is not taken; nor is its button.
// - The login window (nobody logged in; an agent run by the system, LoginWindow.swift): the
//   other way round. The form belongs to the system's SecurityAgent, found as whichever program
//   holds the keyboard's focus in a password field; the field filled through the accessibility
//   interface and the form's own button pressed logs in, in under a second. Key presses of any
//   kind do nothing there, and posting an ordinary key or pointer event from the agent never
//   comes back, so that is not done.
//
// The password is kept in the owner's login keychain, and for the login window in a file only
// the system can read. Only if the owner turns this on; never sent anywhere, never logged.
import AppKit
import Carbon
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

    /// Whether something from the login-window attempt is still on this Mac.
    static var leftovers: Bool { FileManager.default.fileExists(atPath: rootFile) || LoginWindow.installed }

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

    /// Keep the password for unlocking, or take it away. Keeping it needs no administrator:
    /// it goes into the owner's own keychain. Taking it away also removes what an earlier
    /// version put in place for the login window (that asks for an administrator's password).
    /// Returns what came of it ("" when the owner cancelled).
    static func set(_ password: String?, stateDir: String, listen: String, name: String) -> String {
        let base: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service]
        if let password {
            // A wrong password would be typed into the lock screen again and again: refused here.
            if isRight(password) == false { return "That isn't this Mac account's password. Nothing was changed." }
            SecItemDelete(base as CFDictionary)
            var add = base
            add[kSecAttrAccount] = NSUserName()
            add[kSecValueData] = Data(password.utf8)
            add[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            let status = SecItemAdd(add as CFDictionary, nil)
            guard status == errSecSuccess else { return "It didn't work: the keychain refused (\(status))." }
            hostLog("host: unlocking for paired devices is on")
            return "On."
        }
        if FileManager.default.fileExists(atPath: rootFile) || LoginWindow.installed {
            let shell = "rm -f '\(rootFile)' '\(LoginWindow.plist)'"
            var error: NSDictionary?
            NSAppleScript(source: "do shell script \"\(shell)\" with administrator privileges")?.executeAndReturnError(&error)
            if let error, (error[NSAppleScript.errorNumber] as? Int) != -128 {
                return "It didn't work: \(error[NSAppleScript.errorMessage] as? String ?? "unknown")"
            }
        }
        SecItemDelete(base as CFDictionary)
        hostLog("host: unlocking for paired devices is off")
        return "Off. The password was removed from this Mac."
    }

    /// Whether the login window is covered too: its agent and its copy of the password are there.
    static var coversLoginWindow: Bool { FileManager.default.fileExists(atPath: rootFile) && LoginWindow.installed }

    /// Cover the login window as well (nobody logged in), or stop covering it. The agent that
    /// macOS starts there is run by the system and no keychain is open to it, so it gets its
    /// own copy of the password in a file only the system can read. One administrator prompt.
    static func setLoginWindow(_ on: Bool, stateDir: String, listen: String, name: String) -> String {
        var commands: [String]
        var temp: URL?
        if on {
            guard let password = keychainPassword() else { return "Turn on unlocking first." }
            guard let binary = Bundle.main.executablePath, binary.hasPrefix("/Applications/") else {
                return "Move remoteman host into the Applications folder first: the login window starts it from there."
            }
            let t = FileManager.default.temporaryDirectory.appendingPathComponent("remoteman-\(UUID().uuidString)")
            guard FileManager.default.createFile(atPath: t.path, contents: Data(password.utf8), attributes: [.posixPermissions: 0o600]),
                let agent = LoginWindow.agentDescription(binary: binary, stateDir: stateDir, listen: listen, name: name)
            else { return "It didn't work: the password couldn't be handed over." }
            temp = t
            commands = [
                "mkdir -p '/Library/Application Support/remoteman-host'", "install -m 600 -o root -g wheel '\(t.path)' '\(rootFile)'",
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
            return (error[NSAppleScript.errorNumber] as? Int) == -128 ? "" : "It didn't work: \(error[NSAppleScript.errorMessage] as? String ?? "unknown")"
        }
        hostLog("host: logging in at the login window for paired devices is \(on ? "on" : "off")")
        return on ? "On. It starts the next time this Mac shows its login window." : "Off."
    }

    // MARK: The screens

    static var screenLocked: Bool {
        guard let d = CGSessionCopyCurrentDictionary() as? [String: Any] else { return false }
        return (d["CGSSessionScreenIsLocked"] as? Bool) ?? false
    }

    /// Fill in the password field the Mac is showing (its lock screen, or its login window) and
    /// confirm it. Returns nil when a field was found and filled, or why not. The words say
    /// what was found, never what was typed.
    /// At the login window (run by the system, nobody logged in): the same thing a person
    /// does, with every step given a time limit and said in the log before it is taken, because
    /// the first version of this stopped somewhere without a word. The form's field is looked
    /// for only briefly; the login window puts the cursor in it by itself, so the password is
    /// typed whether or not the field was found to click.
    static func logInAtLoginWindow() -> String? {
        guard let password = password() else { return "no password is kept for the login window" }
        guard let strokes = keystrokes(for: password) else { return "the password has characters outside a US keyboard" }
        let was = whoIsOnScreen()
        hostLog("host: login window: waking the screen")
        HostInput.wake()
        describeScreen()
        // The form is whichever program holds the keyboard's focus in a password field (at the
        // login window that is the system's SecurityAgent, not the loginwindow program).
        var form: (app: AXUIElement, field: AXUIElement, pid: pid_t)?
        let looked = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            let system = AXUIElementCreateSystemWide()
            AXUIElementSetMessagingTimeout(system, 1)
            var focused: CFTypeRef?
            if AXUIElementCopyAttributeValue(system, kAXFocusedUIElementAttribute as CFString, &focused) == .success, let e = focused {
                let field = e as! AXUIElement
                var sub: CFTypeRef?
                AXUIElementCopyAttributeValue(field, kAXSubroleAttribute as CFString, &sub)
                var pid: pid_t = 0
                AXUIElementGetPid(field, &pid)
                if (sub as? String) == "AXSecureTextField", pid != 0 {
                    let app = AXUIElementCreateApplication(pid)
                    AXUIElementSetMessagingTimeout(app, 1)
                    var labels: [String] = []
                    describe(app, depth: 0, into: &labels)
                    hostLog("host: login window: the form is process \(pid); its controls: \(labels.prefix(24).joined(separator: ", "))")
                    form = (app, field, pid)
                }
            }
            looked.signal()
        }
        if looked.wait(timeout: .now() + 5) == .timedOut { hostLog("host: login window: finding the form took too long") }

        /// One way of typing, given 8 s; a way that never comes back ends the agent (macOS
        /// starts it afresh), so the login window is never left with a stuck helper.
        func attempt(_ name: String, _ work: @escaping () -> String) -> Bool {
            hostLog("host: login window: trying \(name)")
            let done = DispatchSemaphore(value: 0)
            var note = ""
            DispatchQueue.global().async {
                note = work()
                done.signal()
            }
            if done.wait(timeout: .now() + 8) == .timedOut {
                hostLog("host: login window: \(name) didn't come back in 8 s; ending this agent so that it isn't left stuck")
                exit(1)
            }
            if waitForChange(from: was, seconds: 6) {
                hostLog("host: login window: logged in by \(name) (\(note))")
                return true
            }
            hostLog("host: login window: \(name): \(note); no log in")
            return false
        }

        // What the login window is known to take (macOS 27): the field filled and the form's
        // own button pressed. The other two are kept behind it in case another macOS differs.
        if let form {
            if attempt("the field filled and its button pressed", {
                let set = AXUIElementSetAttributeValue(form.field, kAXValueAttribute as CFString, password as CFString)
                var buttons: [AXUIElement] = []
                collect(form.app, depth: 0, into: &buttons) { role, label in
                    role == "AXButton" && ["login", "log in", "unlock", "ok", "continue"].contains(label.lowercased())
                }
                let confirm = AXUIElementPerformAction(form.field, kAXConfirmAction as CFString)
                for b in buttons { AXUIElementPerformAction(b, kAXPressAction as CFString) }
                return "set \(set.rawValue), confirm \(confirm.rawValue), \(buttons.count) button(s) pressed"
            }) { return nil }
        }
        if let form {
            if attempt("keys sent to the form's own process", {
                for k in strokes { stroke(form.app, k.code, shift: k.shift) }
                usleep(150_000)
                var held: CFTypeRef?
                AXUIElementCopyAttributeValue(form.field, kAXValueAttribute as CFString, &held)
                stroke(form.app, 36, shift: false)
                return "the field held \((held as? String)?.count ?? -1) of \(strokes.count) before Return"
            }) { return nil }
        }
        if attempt("the system's keyboard stream", {
            var accepted = 0
            for k in strokes {
                if RMHIDKey(k.code, true, k.shift) { accepted += 1 }
                usleep(15_000)
                RMHIDKey(k.code, false, k.shift)
                usleep(25_000)
            }
            usleep(150_000)
            var held: CFTypeRef?
            if let form { AXUIElementCopyAttributeValue(form.field, kAXValueAttribute as CFString, &held) }
            RMHIDKey(36, true, false)
            usleep(15_000)
            RMHIDKey(36, false, false)
            return "took \(accepted) of \(strokes.count) keys, the field held \((held as? String)?.count ?? -1) before Return"
        }) { return nil }
        return "none of the ways of typing logged this Mac in"
    }

    /// What is on screen, told without looking at any picture: which programs own the windows
    /// in front, what the accessibility interface answers for each login window process, and
    /// where the keyboard's focus is. For the log, to find out where typed keys go.
    static func describeScreen() {
        let done = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            var owners: [String] = []
            if let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] {
                for w in list.prefix(40) {
                    let owner = w[kCGWindowOwnerName as String] as? String ?? "?"
                    let layer = (w[kCGWindowLayer as String] as? NSNumber)?.intValue ?? 0
                    let b = w[kCGWindowBounds as String] as? [String: Any]
                    let size = "\((b?["Width"] as? NSNumber)?.intValue ?? 0)x\((b?["Height"] as? NSNumber)?.intValue ?? 0)"
                    let text = "\(owner)@\(layer) \(size)"
                    if !owners.contains(text) { owners.append(text) }
                }
            }
            hostLog("host: login window: windows in front, by owner@layer: \(owners.prefix(16).joined(separator: ", "))")
            AXUIElementSetMessagingTimeout(AXUIElementCreateSystemWide(), 1)
            for pid in pidsOfLoginWindow() {
                let app = AXUIElementCreateApplication(pid)
                AXUIElementSetMessagingTimeout(app, 1)
                var role: CFTypeRef?
                let answer = AXUIElementCopyAttributeValue(app, kAXRoleAttribute as CFString, &role)
                var seen: [String: Int] = [:]
                _ = find(app, role: "AXTextField", subrole: "AXSecureTextField", depth: 0, seen: &seen)
                var labels: [String] = []
                describe(app, depth: 0, into: &labels)
                hostLog("host: login window: process \(pid): accessibility answers \(answer.rawValue); kinds \(seen.sorted { $0.key < $1.key }.map { "\($0.key)×\($0.value)" }.joined(separator: " ")); controls \(labels.prefix(20).joined(separator: ", "))")
            }
            var focused: CFTypeRef?
            let f = AXUIElementCopyAttributeValue(AXUIElementCreateSystemWide(), kAXFocusedUIElementAttribute as CFString, &focused)
            var what = "none (\(f.rawValue))"
            if f == .success, let e = focused {
                let el = e as! AXUIElement
                var r: CFTypeRef?, s: CFTypeRef?
                AXUIElementCopyAttributeValue(el, kAXRoleAttribute as CFString, &r)
                AXUIElementCopyAttributeValue(el, kAXSubroleAttribute as CFString, &s)
                var pid: pid_t = 0
                AXUIElementGetPid(el, &pid)
                what = "\(r as? String ?? "?")/\(s as? String ?? "-") in process \(pid)"
            }
            hostLog("host: login window: keyboard focus: \(what); this process trusted for accessibility: \(AXIsProcessTrusted()); secure typing on: \(IsSecureEventInputEnabled())")
            done.signal()
        }
        if done.wait(timeout: .now() + 10) == .timedOut { hostLog("host: login window: describing the screen took too long; going on") }
    }

    private static func pidsOfLoginWindow() -> [pid_t] {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        p.arguments = ["-x", "loginwindow"]
        let out = Pipe()
        p.standardOutput = out
        guard (try? p.run()) != nil else { return [] }
        p.waitUntilExit()
        let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        // The newest is the one showing the login window.
        return text.split(separator: "\n").compactMap { pid_t($0) }.sorted(by: >)
    }

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
