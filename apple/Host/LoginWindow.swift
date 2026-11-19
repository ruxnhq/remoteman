// Reaching this Mac while nobody is logged in. The host is an app in its owner's session, and
// goes when they log out; a launch agent that macOS starts in the login window's own session
// runs the same host there (as root), so that a paired device is shown the login window and
// can type into it. It listens only while the login window is what the Mac shows, and the
// owner's own host lets go of the port whenever its session isn't the one on screen, so the
// two never want the port at once. A device is not carried across: it connects again after
// logging in.
//
// What this can't do: after a restart with FileVault on, the Mac waits for its disk to be
// unlocked before any of this exists.
import AppKit
import Foundation
import SystemConfiguration

enum LoginWindow {
    static let label = "com.xnixy.remoteman.host.login"
    static let plist = "/Library/LaunchAgents/\(label).plist"
    static let log = "/Library/Logs/remoteman-host-login.log"

    /// Started by launchd in the login window's session.
    static let running = CommandLine.arguments.contains("--login-window")

    static var installed: Bool { FileManager.default.fileExists(atPath: plist) }

    /// Nobody's desktop is on screen: the login window is.
    static var showing: Bool {
        var uid: uid_t = 0
        guard let user = SCDynamicStoreCopyConsoleUser(nil, &uid, nil) as String? else { return true }
        return user.isEmpty || user == "loginwindow"
    }

    /// This process's session is the one on screen (false while another user, or the login
    /// window after a fast switch, has it).
    static var sessionOnConsole: Bool {
        guard let d = CGSessionCopyCurrentDictionary() as? [String: Any] else { return true }
        return (d[kCGSessionOnConsoleKey as String] as? Bool) ?? true
    }

    /// Install or remove the launch agent. macOS asks for an administrator's password.
    /// Returns what came of it ("" when the owner cancelled).
    static func setInstalled(_ install: Bool, stateDir: String, listen: String, name: String) -> String {
        var commands: [String]
        if install {
            guard let binary = Bundle.main.executablePath, binary.hasPrefix("/Applications/") else {
                return "Move remoteman host into the Applications folder first: the login window starts it from there."
            }
            let dict: [String: Any] = [
                "Label": label,
                "ProgramArguments": [binary, "--login-window", "yes", "--state", stateDir, "--listen", listen, "--name", name],
                "LimitLoadToSessionType": ["LoginWindow"],
                "RunAtLoad": true,
                "KeepAlive": true,
                "StandardOutPath": log,
                "StandardErrorPath": log,
            ]
            let temp = FileManager.default.temporaryDirectory.appendingPathComponent("\(label).plist")
            guard let data = try? PropertyListSerialization.data(fromPropertyList: dict, format: .xml, options: 0),
                (try? data.write(to: temp)) != nil
            else { return "It didn't work: the agent's description couldn't be written." }
            commands = ["cp '\(temp.path)' '\(plist)'", "chown root:wheel '\(plist)'", "chmod 644 '\(plist)'"]
        } else {
            commands = ["rm -f '\(plist)'"]
        }
        let shell = commands.joined(separator: " && ").replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        var error: NSDictionary?
        NSAppleScript(source: "do shell script \"\(shell)\" with administrator privileges")?.executeAndReturnError(&error)
        if let error {
            let cancelled = (error[NSAppleScript.errorNumber] as? Int) == -128
            return cancelled ? "" : "It didn't work: \(error[NSAppleScript.errorMessage] as? String ?? "unknown")"
        }
        hostLog("host: login window agent \(install ? "installed" : "removed")")
        return install ? "Installed. It starts the next time this Mac shows its login window." : "Removed."
    }

    /// The agent runs as root in the owner's state folder: whatever it made there goes back to
    /// the folder's owner, so that the owner's own host can still read and write it.
    static func giveBack(_ dir: String) {
        guard getuid() == 0, let attrs = try? FileManager.default.attributesOfItem(atPath: dir),
            let uid = attrs[.ownerAccountID] as? NSNumber, let gid = attrs[.groupOwnerAccountID] as? NSNumber, uid.intValue != 0
        else { return }
        for name in (try? FileManager.default.subpathsOfDirectory(atPath: dir)) ?? [] {
            chown("\(dir)/\(name)", uid.uint32Value, gid.uint32Value)
        }
    }
}
