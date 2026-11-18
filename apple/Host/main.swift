// remoteman-host: the Mac as a remoteman host, a menu-bar app. Opened by hand it shares this
// Mac unless sharing was paused, and its menu-bar item has the rest. To run it by itself:
//   open -n remoteman-host.app --args [--listen 0.0.0.0:7791] [--name NAME] [--pair yes]
//        [--state DIR] [--main yes] [--capture x420|l10r|420v] [--in-flight N] [--bitrate BITS_PER_SECOND]
//        [--test-window yes]
// and read ~/Library/Logs/remoteman-host.log. Every option takes a value. It needs Screen
// Recording (to capture) and Accessibility (to put input in), granted to "remoteman-host" in
// System Settings. `pkill -TERM remoteman-host` (or Ctrl-C when run from a shell) leaves cleanly.
import AppKit
import Foundation

setvbuf(stdout, nil, _IOLBF, 0)

var options = MacHost.Options()
let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("remoteman-host", isDirectory: true)
options.stateDir = support.path

// Any of the options below makes this an unattended run: sharing starts at once, the system
// asks for missing permissions, and a failure to start ends the app.
var unattended = false
var installCamera = false
var probeDisplay = false
var installMicrophone = false
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    func value() -> String { args.isEmpty ? "" : args.removeFirst() }
    func yes() -> Bool { ["yes", "1", "true"].contains(value()) }
    // Asking macOS to install the camera is something the owner does by hand; it starts nothing else.
    if a == "--probe-display" {
        probeDisplay = yes()
        continue
    }
    if a == "--install-microphone" {
        installMicrophone = yes()
        continue
    }
    if a == "--install-camera" {
        installCamera = yes()
        continue
    }
    if a.hasPrefix("--") { unattended = true }
    switch a {
    case "--listen": options.listen = value()
    case "--name": options.name = value()
    case "--state": options.stateDir = value()
    case "--pair": options.pair = yes()
    case "--main": options.makeMain = yes()
    case "--capture": options.capture = value()
    case "--bitrate": options.bitrate = Int(value()) ?? 0
    case "--test-window": options.testWindow = yes()
    case "--in-flight": options.inFlight = Int(value()) ?? 2
    // For tests: so many seconds after a device connects, drop it or stop trusting it.
    case "--sound": options.sound = yes()
    // What paired devices may do: --allow clipboard,files
    case "--allow":
        let list = value().split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
        options.allowClipboard = list.contains("clipboard")
        options.allowFiles = list.contains("files")
    case "--test-tone": options.testTone = yes()
    // For tests: so many seconds after a device connects, allow the clipboard and files; 4 s later, take them away.
    case "--test-allow": options.testAllow = Double(value())
    case "--test-drop": options.testDrop = Double(value())
    case "--test-revoke": options.testRevoke = Double(value())
    default:
        // AppKit passes arguments of its own (-NSDocumentRevisionsDebugMode and the like).
        if a.hasPrefix("--") { hostLog("host: unknown option \(a)") }
    }
}
try? FileManager.default.createDirectory(atPath: options.stateDir, withIntermediateDirectories: true)

if unattended {
    if !CGPreflightScreenCaptureAccess() {
        hostLog("host: no Screen Recording permission yet; clients are refused until it is granted. If macOS doesn't ask, turn on remoteman-host in System Settings > Privacy & Security > Screen & System Audio Recording.")
        CGRequestScreenCaptureAccess()
    } else {
        hostLog("host: Screen Recording permission: granted")
    }
    if AXIsProcessTrusted() { hostLog("host: Accessibility permission: granted") }
    // Asks with the system's own prompt the first time, and points at System Settings after that.
    if !AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary) {
        hostLog("host: no Accessibility permission: the picture will work, input will not. Turn on remoteman-host in System Settings > Privacy & Security > Accessibility.")
    }
} else {
    // Opened by hand: the first-run window and Settings say what is missing; nothing is asked unbidden.
    hostLog("host: Screen Recording permission: \(CGPreflightScreenCaptureAccess() ? "granted" : "not granted"), Accessibility permission: \(AXIsProcessTrusted() ? "granted" : "not granted")")
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

// Sharing starts in applicationDidFinishLaunching (HostController.launch).
let controller = HostController(options: options, unattended: unattended)
let delegate = HostAppDelegate(controller: controller)
app.delegate = delegate
if probeDisplay { DispatchQueue.main.asyncAfter(deadline: .now() + 1) { DisplayProbe.run() } }
if installMicrophone {
    DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
        hostLog("microphone: asking for the password to install remoteman Microphone")
        controller.installMicrophone()
        hostLog("microphone: \(controller.microphoneNote.isEmpty ? "cancelled" : controller.microphoneNote)")
    }
}
if installCamera {
    DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
        hostLog("camera: asking macOS to install remoteman Camera")
        controller.installCamera()
    }
}

// Ctrl-C or a kill: take the display away and say goodbye before going.
var signals: [DispatchSourceSignal] = []
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let s = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    s.setEventHandler {
        hostLog("host: stopping")
        controller.shutdown()
        exit(0)
    }
    s.resume()
    signals.append(s)
}

app.run()
