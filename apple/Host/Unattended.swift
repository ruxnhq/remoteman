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

    /// Keep the password (both copies) and install the login window's agent, or take all of it
    /// away. One administrator prompt. Returns what came of it ("" when the owner cancelled).
    static func set(_ password: String?, stateDir: String, listen: String, name: String) -> String {
        let base: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service]
        var commands: [String] = []
        var temp: URL?
        if let password {
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
        var seen: [String: Int] = [:]
        guard let field = find(app, role: "AXTextField", subrole: "AXSecureTextField", depth: 0, seen: &seen) else {
            let summary = seen.sorted { $0.key < $1.key }.map { "\($0.key)×\($0.value)" }.joined(separator: " ")
            return "no password field was found (\(summary.isEmpty ? "nothing readable: is Accessibility allowed?" : summary))"
        }
        AXUIElementSetAttributeValue(field, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        let set = AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, password as CFString)
        guard set == .success else { return "the password field refused the text (\(set.rawValue))" }
        // Confirm: the field's own action first, then Return as a key.
        if AXUIElementPerformAction(field, kAXConfirmAction as CFString) != .success {
            for down in [true, false] {
                CGEvent(keyboardEventSource: CGEventSource(stateID: .hidSystemState), virtualKey: 36, keyDown: down)?.post(tap: .cghidEventTap)
            }
        }
        return nil
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
