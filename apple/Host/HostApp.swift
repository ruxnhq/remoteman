// The host as an app: what the interface knows about the host (HostController), and the
// menu-bar item with its panel and windows (HostAppDelegate). MacHost does the work.
import AppKit
import ApplicationServices
import Combine
import SwiftUI

final class HostController: ObservableObject {
    let options: MacHost.Options
    /// Started with options, to run by itself: sharing starts at once, whatever was chosen
    /// in the interface before, and a failure to start ends the app, as it always has.
    let unattended: Bool
    private let host: MacHost

    @Published private(set) var sharing = false
    @Published private(set) var startError: String?
    @Published private(set) var client: MacHost.Client?
    @Published private(set) var fps = 0
    @Published private(set) var bitsPerSecond = 0
    @Published private(set) var fingerprint = ""
    @Published private(set) var port = "7791"
    @Published private(set) var addresses: [String] = []
    @Published private(set) var pairing = false
    /// What paired devices may do with this Mac beyond seeing and using it. Both off until the
    /// owner turns them on; a device is only offered what is on.
    @Published private(set) var allowClipboard = false
    @Published private(set) var allowFiles = false
    static let clipboardKey = "allowClipboard", filesKey = "allowFiles"

    func setAllowed(clipboard: Bool? = nil, files: Bool? = nil) {
        if let clipboard { allowClipboard = clipboard }
        if let files { allowFiles = files }
        if !unattended {
            UserDefaults.standard.set(allowClipboard, forKey: Self.clipboardKey)
            UserDefaults.standard.set(allowFiles, forKey: Self.filesKey)
        }
        host.allow(clipboard: allowClipboard, files: allowFiles)
        hostLog("host: devices may share the clipboard: \(allowClipboard); send files: \(allowFiles)")
        // A device learns what is offered when it connects, so sharing starts afresh.
        guard host.isRunning else { return }
        host.stop()
        client = nil
        do { try host.start() } catch {
            sharing = false
            startError = error.localizedDescription
        }
    }
    /// The devices this Mac trusts, in the order they were paired.
    @Published private(set) var devices: [MacHost.Device] = []
    /// When the pairing started from the interface closes by itself.
    @Published private(set) var pairingEnds: Date?
    /// The core closed pairing by itself: a device was let in.
    @Published private(set) var pairedJustNow = false
    @Published private(set) var screenRecording = CGPreflightScreenCaptureAccess()
    @Published private(set) var accessibility = AXIsProcessTrusted()
    /// Ticks once a second, for countdowns and "for 42 min".
    @Published private(set) var now = Date()

    static let pausedKey = "sharingPaused"
    static let pairingSeconds: TimeInterval = 120
    private var timer: Timer?

    var name: String { options.name }

    init(options: MacHost.Options, unattended: Bool) {
        self.options = options
        self.unattended = unattended
        var options = options
        if unattended {
            allowClipboard = options.allowClipboard
            allowFiles = options.allowFiles
        } else {
            allowClipboard = UserDefaults.standard.bool(forKey: Self.clipboardKey)
            allowFiles = UserDefaults.standard.bool(forKey: Self.filesKey)
            options.allowClipboard = allowClipboard
            options.allowFiles = allowFiles
        }
        host = MacHost(options: options)
        host.onClient = { [weak self] c in
            // The same device with another display (it changed its settings) keeps its time.
            let before = self?.client
            var c = c
            if let b = before, c != nil { c?.since = b.since }
            self?.client = c
            if let self, c != nil, before == nil {
                hostLog("host: trusted devices: \(self.host.devices.map { "\($0.name) \($0.fingerprint.prefix(8))\($0.connected ? " (connected)" : "")" }.joined(separator: ", "))")
                if let t = options.testDrop { DispatchQueue.main.asyncAfter(deadline: .now() + t) { self.disconnectClient() } }
                if let t = options.testRevoke {
                    DispatchQueue.main.asyncAfter(deadline: .now() + t) {
                        if let d = self.host.devices.first(where: { $0.connected }) { self.revoke(d) }
                        hostLog("host: trusted devices now: \(self.host.devices.count)")
                    }
                }
            }
            if c == nil {
                self?.fps = 0
                self?.bitsPerSecond = 0
            }
        }
        host.onStats = { [weak self] fps, bits in
            self?.fps = fps
            self?.bitsPerSecond = bits
        }
    }

    /// Start as the launch asks: at once when unattended, otherwise unless sharing was paused.
    func launch() {
        if unattended {
            do { try host.start() } catch {
                hostLog("host: failed to start: \(error.localizedDescription)")
                exit(1)
            }
            sharing = true
            readInfo()
        } else if UserDefaults.standard.object(forKey: Self.pausedKey) != nil, !UserDefaults.standard.bool(forKey: Self.pausedKey) {
            // Sharing was on when the app was last open. On a fresh install nothing starts by
            // itself (and so macOS asks nothing at launch): the owner turns sharing on.
            setSharing(true)
        }
        let t = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func setSharing(_ on: Bool) {
        if on {
            guard !host.isRunning else { return }
            do {
                try host.start()
                sharing = true
                startError = nil
                readInfo()
            } catch {
                sharing = false
                startError = error.localizedDescription
                hostLog("host: failed to start: \(error.localizedDescription)")
            }
        } else {
            host.stop()
            sharing = false
            startError = nil
            client = nil
            pairing = false
            pairingEnds = nil
            hostLog("host: sharing paused")
        }
        if !unattended { UserDefaults.standard.set(!on, forKey: Self.pausedKey) }
    }

    /// Drop the connected device; it stays trusted and sharing goes on.
    func disconnectClient() {
        guard host.isRunning else { return }
        hostLog("host: disconnecting the device: \(host.dropClient() ? "dropped" : "none was connected")")
    }

    /// Stop trusting a device; it has to be paired again before it can connect.
    func revoke(_ d: MacHost.Device) {
        let r = host.revoke(d.fingerprint)
        hostLog("host: revoking \(d.name): \(r == 1 ? "done" : r == 0 ? "it wasn't trusted" : "the list couldn't be saved")")
        if r < 0 { startError = "The list of paired devices couldn't be saved." }
        devices = host.devices
    }

    /// Let the next unknown device in, for two minutes.
    func beginPairing() {
        guard host.isRunning else { return }
        pairedJustNow = false
        host.pairing = true
        pairing = true
        pairingEnds = Date().addingTimeInterval(Self.pairingSeconds)
        hostLog("host: pairing on")
    }

    func endPairing() {
        guard pairing else { return }
        host.pairing = false
        pairing = false
        pairingEnds = nil
        hostLog("host: pairing off")
    }

    func shutdown() {
        timer?.invalidate()
        host.stop()
    }

    private func readInfo() {
        if let info = host.info {
            fingerprint = info.fingerprint
            if let p = info.address.split(separator: ":").last, Int(p) != nil { port = String(p) }
        }
        addresses = Self.localAddresses()
    }

    private func tick() {
        now = Date()
        let sr = CGPreflightScreenCaptureAccess(), ax = AXIsProcessTrusted()
        if sr != screenRecording { screenRecording = sr }
        if ax != accessibility { accessibility = ax }
        guard host.isRunning else {
            if !devices.isEmpty { devices = [] }
            return
        }
        let list = host.devices
        if list != devices { devices = list }
        let core = host.pairing
        if pairing, !core {
            // The core turns pairing off once it has let a device in.
            pairing = false
            pairingEnds = nil
            pairedJustNow = true
        } else if !pairing, core {
            pairing = true  // started with --pair yes
        }
        if pairing, let end = pairingEnds, now >= end { endPairing() }
    }

    // MARK: Permissions (asked for and read, never changed from here)

    /// macOS lists an app under Screen Recording once it has asked; then open the pane.
    func openScreenRecordingSettings() {
        if !CGPreflightScreenCaptureAccess() { CGRequestScreenCaptureAccess() }
        open("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
    }

    /// The first time, the system's own prompt (which lists the app and offers System
    /// Settings); after that, the pane.
    func openAccessibilitySettings() {
        let asked = "askedAccessibility"
        if !UserDefaults.standard.bool(forKey: asked) {
            UserDefaults.standard.set(true, forKey: asked)
            _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        } else {
            open("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
    }

    private func open(_ url: String) {
        if let u = URL(string: url) { NSWorkspace.shared.open(u) }
    }

    /// This Mac's IPv4 addresses on its networks, for a device to be pointed at.
    static func localAddresses() -> [String] {
        var out: [String] = []
        var list: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&list) == 0 else { return [] }
        defer { freeifaddrs(list) }
        var next = list
        while let a = next {
            let flags = Int32(a.pointee.ifa_flags)
            if let sa = a.pointee.ifa_addr, sa.pointee.sa_family == UInt8(AF_INET), flags & IFF_UP != 0, flags & IFF_LOOPBACK == 0 {
                var name = [CChar](repeating: 0, count: Int(NI_MAXHOST))
                if getnameinfo(sa, socklen_t(sa.pointee.sa_len), &name, socklen_t(name.count), nil, 0, NI_NUMERICHOST) == 0 {
                    out.append(String(cString: name))
                }
            }
            next = a.pointee.ifa_next
        }
        return out
    }

    /// What to type on the other device: this Mac's network name, with the port if it isn't 7791.
    var addressToEnter: String {
        let host = ProcessInfo.processInfo.hostName
        return port == "7791" ? host : "\(host):\(port)"
    }
}

final class HostAppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let controller: HostController
    private var statusItem: NSStatusItem?
    private let popover = NSPopover()
    private var windows: [String: NSWindow] = [:]
    private var sinks: Set<AnyCancellable> = []
    static let firstRunKey = "firstRunDone"

    init(controller: HostController) {
        self.controller = controller
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        buildMenu()
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.target = self
        item.button?.action = #selector(togglePanel)
        statusItem = item

        popover.behavior = .transient
        popover.contentViewController = NSHostingController(
            rootView: HostPanel(
                controller: controller,
                pair: { [weak self] in self?.showPairing() },
                settings: { [weak self] in self?.showSettings() },
                quit: { NSApp.terminate(nil) },
                close: { [weak self] in self?.popover.performClose(nil) }
            ).tint(Tokens.tint))

        controller.launch()
        controller.$client.combineLatest(controller.$sharing).sink { [weak self] client, sharing in
            self?.updateIcon(client: client, sharing: sharing)
        }.store(in: &sinks)

        // A first start by hand, with a permission still missing: say what is needed.
        if !controller.unattended, !UserDefaults.standard.bool(forKey: Self.firstRunKey),
            !(controller.screenRecording && controller.accessibility)
        {
            showFirstRun()
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    func applicationWillTerminate(_ note: Notification) {
        hostLog("host: stopping")
        controller.shutdown()
    }

    /// ⌘, and ⌘Q work while one of the app's windows has the keyboard.
    private func buildMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        let settings = NSMenuItem(title: "Settings…", action: #selector(showSettings), keyEquivalent: ",")
        settings.target = self
        appMenu.addItem(settings)
        appMenu.addItem(.separator())
        appMenu.addItem(NSMenuItem(title: "Quit remoteman", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        appItem.submenu = appMenu
        main.addItem(appItem)
        let windowItem = NSMenuItem()
        let windowMenu = NSMenu(title: "Window")
        windowMenu.addItem(NSMenuItem(title: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w"))
        windowItem.submenu = windowMenu
        main.addItem(windowItem)
        NSApp.mainMenu = main
    }

    private func updateIcon(client: MacHost.Client?, sharing: Bool) {
        guard let button = statusItem?.button else { return }
        button.image = Self.icon(connected: client != nil)
        button.appearsDisabled = !sharing
        let state = client.map { "\($0.name) is connected" } ?? (sharing ? "ready" : "not sharing")
        button.setAccessibilityLabel("remoteman: \(state)")
        button.toolTip = "remoteman: \(state)"
    }

    /// The host icon's leaning display with its stand bar, as a template: an outline when
    /// ready, filled while a device is connected.
    static func icon(connected: Bool) -> NSImage {
        let image = NSImage(size: NSSize(width: 19, height: 16), flipped: false) { _ in
            NSColor.black.set()
            // Lean 14 degrees to the right about the middle, as the app icon does.
            var lean = AffineTransform(translationByX: 9.5, byY: 8)
            lean.append(AffineTransform(m11: 1, m12: 0, m21: CGFloat(tan(14 * Double.pi / 180)), m22: 1, tX: 0, tY: 0))
            lean.translate(x: -9.5, y: -8)
            let screen = NSBezierPath(roundedRect: NSRect(x: 2.5, y: 5, width: 14, height: 9.5), xRadius: 2.2, yRadius: 2.2)
            screen.transform(using: lean)
            screen.lineWidth = 1.5
            screen.stroke()
            if connected { screen.fill() }
            let stand = NSBezierPath()
            stand.move(to: NSPoint(x: 6.5, y: 1.6))
            stand.line(to: NSPoint(x: 12.5, y: 1.6))
            stand.transform(using: lean)
            stand.lineWidth = 1.5
            stand.lineCapStyle = .round
            stand.stroke()
            return true
        }
        image.isTemplate = true
        return image
    }

    @objc private func togglePanel() {
        guard let button = statusItem?.button else { return }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.makeKey()
        }
    }

    // MARK: Windows

    private func show<V: View>(
        _ key: String, title: String, size: NSSize, resizable: Bool = false, plain: Bool = false, _ view: () -> V
    ) {
        popover.performClose(nil)
        if windows[key] == nil {
            var style: NSWindow.StyleMask = [.titled, .closable, .miniaturizable]
            if resizable { style.insert(.resizable) }
            if plain { style.insert(.fullSizeContentView) }
            let visible = NSScreen.main?.visibleFrame.size ?? size
            let w = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: size.width, height: min(size.height, visible.height - 60)),
                styleMask: style, backing: .buffered, defer: false)
            w.title = title
            w.isReleasedWhenClosed = false
            w.identifier = NSUserInterfaceItemIdentifier(key)
            w.delegate = self
            if plain {
                w.titlebarAppearsTransparent = true
                w.titleVisibility = .hidden
            }
            w.contentView = NSHostingView(rootView: view().tint(Tokens.tint))
            if resizable {
                w.contentMinSize = NSSize(width: size.width, height: 400)
                w.contentMaxSize = NSSize(width: size.width, height: 4000)
            }
            w.center()
            windows[key] = w
        }
        NSApp.activate(ignoringOtherApps: true)
        windows[key]?.makeKeyAndOrderFront(nil)
    }

    @objc func showSettings() {
        show("settings", title: "remoteman Settings", size: NSSize(width: 760, height: 1200), resizable: true) {
            HostSettings(controller: controller, pair: { [weak self] in self?.showPairing() })
        }
    }

    func showPairing() {
        show("pair", title: "Pair a Device", size: NSSize(width: 520, height: 700)) {
            PairDevice(controller: controller, close: { [weak self] in self?.windows["pair"]?.performClose(nil) })
        }
        controller.beginPairing()
    }

    func showFirstRun() {
        show("firstRun", title: "remoteman", size: NSSize(width: 700, height: 640), plain: true) {
            FirstRun(controller: controller, close: { [weak self] in self?.windows["firstRun"]?.performClose(nil) })
        }
    }

    func windowWillClose(_ note: Notification) {
        guard let w = note.object as? NSWindow else { return }
        switch w.identifier?.rawValue {
        case "pair": controller.endPairing()
        case "firstRun": UserDefaults.standard.set(true, forKey: Self.firstRunKey)
        default: break
        }
    }
}
