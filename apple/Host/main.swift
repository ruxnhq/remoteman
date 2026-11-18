// remoteman-host: the Mac as a remoteman host. An app with no window; start it with
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

var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    func value() -> String { args.isEmpty ? "" : args.removeFirst() }
    func yes() -> Bool { ["yes", "1", "true"].contains(value()) }
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
    default:
        // AppKit passes arguments of its own (-NSDocumentRevisionsDebugMode and the like).
        if a.hasPrefix("--") { hostLog("host: unknown option \(a)") }
    }
}
try? FileManager.default.createDirectory(atPath: options.stateDir, withIntermediateDirectories: true)

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

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

let host = MacHost(options: options)
do { try host.start() } catch {
    hostLog("host: failed to start: \(error.localizedDescription)")
    exit(1)
}

// Ctrl-C or a kill: take the display away and say goodbye before going.
var signals: [DispatchSourceSignal] = []
for sig in [SIGINT, SIGTERM] {
    signal(sig, SIG_IGN)
    let s = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    s.setEventHandler {
        hostLog("host: stopping")
        host.stop()
        exit(0)
    }
    s.resume()
    signals.append(s)
}

app.run()
