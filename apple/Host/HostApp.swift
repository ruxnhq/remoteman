// The host as an app: what the interface knows about the host (HostController), and the
// menu-bar item with its panel and windows (HostAppDelegate). MacHost does the work.
import ServiceManagement
import UserNotifications
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
    /// Shown to devices looking on this network. Off until the owner turns it on.
    @Published private(set) var advertise = false
    static let advertiseKey = "advertise"

    func setAdvertise(_ on: Bool) {
        advertise = on
        if !unattended { UserDefaults.standard.set(on, forKey: Self.advertiseKey) }
        host.advertise(on)
    }
    /// Silence this Mac's speakers while a device plays its sound. On unless the owner turns it off.
    @Published private(set) var silenceSpeakers = true
    static let speakersKey = "keepSpeakersOn"

    func setSilenceSpeakers(_ on: Bool) {
        silenceSpeakers = on
        if !unattended { UserDefaults.standard.set(!on, forKey: Self.speakersKey) }
        host.setSilenceSpeakers(on)
    }
    /// remoteman's camera on this Mac: whether it is installed, and what the last request came to.
    @Published private(set) var cameraInstalled = HostCamera.installed
    @Published private(set) var cameraNote = ""

    /// remoteman's microphone on this Mac.
    @Published private(set) var microphoneInstalled = HostMicrophone.installed
    @Published private(set) var microphoneNote = ""

    func installMicrophone() {
        microphoneNote = HostMicrophone.setInstalled(!microphoneInstalled)
        // The audio server takes a moment to come back with (or without) the device.
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            self?.microphoneInstalled = HostMicrophone.installed
            self?.host.refreshFeatures()
        }
    }

    // MARK: What the owner decides about devices (kept in the app's defaults)

    @Published private(set) var lockOnLeave = UserDefaults.standard.bool(forKey: "lockOnLeave")
    @Published private(set) var notifyOnConnect = UserDefaults.standard.bool(forKey: "notifyOnConnect")
    /// Extra, main, only: which a device may ask for, and where it goes otherwise.
    @Published private(set) var allowedPlacements: Set<Int> = Set((UserDefaults.standard.array(forKey: "allowedPlacements") as? [Int]) ?? [0, 1, 2])
    @Published private(set) var defaultPlacement = UserDefaults.standard.integer(forKey: "defaultPlacement")
    /// The most this Mac sends, in megabits a second; 0 lets it choose.
    @Published private(set) var ownMegabits = UserDefaults.standard.integer(forKey: "ownMegabits")
    @Published private(set) var notifyNote = ""
    @Published private(set) var tenBit = UserDefaults.standard.object(forKey: "tenBit") as? Bool ?? true

    func setTenBit(_ on: Bool) {
        tenBit = on
        UserDefaults.standard.set(on, forKey: "tenBit")
        host.setTenBit(on)
    }

    func setLockOnLeave(_ on: Bool) {
        lockOnLeave = on
        UserDefaults.standard.set(on, forKey: "lockOnLeave")
        host.setLockOnLeave(on)
    }

    /// Notifications are asked for only here, when the owner turns this on.
    func setNotifyOnConnect(_ on: Bool) {
        guard on else {
            notifyOnConnect = false
            UserDefaults.standard.set(false, forKey: "notifyOnConnect")
            return
        }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { [weak self] ok, _ in
            DispatchQueue.main.async {
                self?.notifyOnConnect = ok
                self?.notifyNote = ok ? "" : "macOS isn't letting remoteman host show notifications. Allow it under System Settings, Notifications."
                UserDefaults.standard.set(ok, forKey: "notifyOnConnect")
            }
        }
    }

    func setPlacement(_ place: Int, allowed: Bool) {
        var set = allowedPlacements
        if allowed { set.insert(place) } else { set.remove(place) }
        guard !set.isEmpty else { return }  // one place always stays
        allowedPlacements = set
        if !set.contains(defaultPlacement) { defaultPlacement = set.min() ?? 0 }
        savePolicy()
    }

    func setDefaultPlacement(_ place: Int) {
        defaultPlacement = place
        allowedPlacements.insert(place)
        savePolicy()
    }

    private func savePolicy() {
        UserDefaults.standard.set(Array(allowedPlacements).sorted(), forKey: "allowedPlacements")
        UserDefaults.standard.set(defaultPlacement, forKey: "defaultPlacement")
        host.setPolicy(allowed: allowedPlacements, fallback: defaultPlacement)
    }

    func setOwnMegabits(_ m: Int) {
        ownMegabits = max(0, m)
        UserDefaults.standard.set(ownMegabits, forKey: "ownMegabits")
        host.setOwnBitrate(ownMegabits * 1_000_000)
    }

    private func announce(_ name: String) {
        guard notifyOnConnect else { return }
        let content = UNMutableNotificationContent()
        content.title = "\(name) connected"
        content.body = "It can see and use this Mac until it disconnects."
        UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    /// Started by macOS when the owner logs in (a login item, listed in System Settings).
    @Published private(set) var openAtLogin = SMAppService.mainApp.status == .enabled
    @Published private(set) var openAtLoginNote = ""

    func setOpenAtLogin(_ on: Bool) {
        do {
            if on { try SMAppService.mainApp.register() } else { try SMAppService.mainApp.unregister() }
            openAtLoginNote = ""
        } catch {
            openAtLoginNote = "It didn't work: \(error.localizedDescription)"
        }
        openAtLogin = SMAppService.mainApp.status == .enabled
        hostLog("host: opens at login: \(openAtLogin)")
    }

    /// Reaching this Mac at its login window (LoginWindow.swift).
    @Published private(set) var loginInstalled = LoginWindow.installed
    @Published private(set) var loginNote = ""
    /// Sharing stopped because this session isn't the one on screen; it comes back with it.
    private var yielded = false

    /// Logging in and unlocking for paired devices (Unattended.swift).
    @Published private(set) var unattendedOn = Unattended.enabled
    @Published private(set) var unattendedNote = ""

    /// `password` nil turns it off and removes the password from this Mac.
    func setUnattended(password: String?) {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("remoteman-host", isDirectory: true).path
        unattendedNote = Unattended.set(
            password, stateDir: unattended ? options.stateDir : support, listen: "0.0.0.0:\(port.isEmpty ? "7791" : port)", name: options.name)
        unattendedOn = Unattended.enabled
        loginWindowOn = Unattended.coversLoginWindow
        loginInstalled = LoginWindow.installed
        // After a log in someone has to be listening: the host opens with the session.
        if unattendedOn, !openAtLogin { setOpenAtLogin(true) }
    }

    @Published private(set) var loginWindowOn = Unattended.coversLoginWindow

    func setUnattendedLoginWindow(_ on: Bool) {
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("remoteman-host", isDirectory: true).path
        unattendedNote = Unattended.setLoginWindow(
            on, stateDir: unattended ? options.stateDir : support, listen: "0.0.0.0:\(port.isEmpty ? "7791" : port)", name: options.name)
        loginWindowOn = Unattended.coversLoginWindow
        loginInstalled = LoginWindow.installed
        if loginWindowOn, !openAtLogin { setOpenAtLogin(true) }
    }

    func installLoginWindow(_ install: Bool? = nil) {
        let want = install ?? !loginInstalled
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("remoteman-host", isDirectory: true).path
        loginNote = LoginWindow.setInstalled(
            want, stateDir: unattended ? options.stateDir : support, listen: "0.0.0.0:\(port.isEmpty ? "7791" : port)", name: options.name)
        loginInstalled = LoginWindow.installed
    }

    /// One port, two hosts: this one has it while its session is on screen; the login window's
    /// has it while the login window is.
    /// When this program's file was written, as it was when this process started.
    private let builtAt = (try? FileManager.default.attributesOfItem(atPath: Bundle.main.executablePath ?? ""))?[.modificationDate] as? Date

    private func shareThePort() {
        if options.loginWindow {
            // A newer build put in place: this agent ends, and macOS starts the new one.
            let now = (try? FileManager.default.attributesOfItem(atPath: Bundle.main.executablePath ?? ""))?[.modificationDate] as? Date
            if let builtAt, let now, now != builtAt {
                hostLog("host: a newer build is installed: this agent makes way for it")
                host.stop()
                exit(0)
            }
            let want = LoginWindow.showing
            if want, !host.isRunning {
                do {
                    try host.start()
                    sharing = true
                    LoginWindow.giveBack(options.stateDir)
                    hostLog("host: the login window is showing: listening")
                } catch {
                    if startError != error.localizedDescription { hostLog("host: can't listen yet: \(error.localizedDescription)") }
                    startError = error.localizedDescription
                }
            } else if !want, host.isRunning {
                hostLog("host: someone is logged in: leaving the port to their own host")
                host.stop()
                sharing = false
                LoginWindow.giveBack(options.stateDir)
            }
            return
        }
        let onScreen = LoginWindow.sessionOnConsole
        if !onScreen, host.isRunning {
            hostLog("host: this session is no longer on screen: sharing waits")
            host.stop()
            sharing = false
            client = nil
            yielded = true
        } else if onScreen, yielded, !host.isRunning {
            // The login window's host may need a moment to let go: tried again each second.
            do {
                try host.start()
                sharing = true
                yielded = false
                startError = nil
                readInfo()
                hostLog("host: this session is on screen again: sharing")
            } catch {
                startError = error.localizedDescription
            }
        }
    }

    func installCamera() {
        CameraInstaller.shared.changed = { [weak self] note in
            self?.cameraNote = note
            self?.cameraInstalled = HostCamera.installed
        }
        if cameraInstalled { CameraInstaller.shared.remove() } else { CameraInstaller.shared.install() }
    }

    func setAllowed(clipboard: Bool? = nil, files: Bool? = nil) {
        if let clipboard { allowClipboard = clipboard }
        if let files { allowFiles = files }
        if !unattended {
            UserDefaults.standard.set(allowClipboard, forKey: Self.clipboardKey)
            UserDefaults.standard.set(allowFiles, forKey: Self.filesKey)
        }
        host.allow(clipboard: allowClipboard, files: allowFiles)
        hostLog("host: devices may share the clipboard: \(allowClipboard); send files: \(allowFiles)")
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
        if !unattended {
            options.allowClipboard = UserDefaults.standard.bool(forKey: Self.clipboardKey)
            options.allowFiles = UserDefaults.standard.bool(forKey: Self.filesKey)
            options.advertise = UserDefaults.standard.bool(forKey: Self.advertiseKey)
        }
        if !unattended { options.silenceSpeakers = !UserDefaults.standard.bool(forKey: Self.speakersKey) }
        if !unattended {
            options.lockOnLeave = UserDefaults.standard.bool(forKey: "lockOnLeave")
            options.tenBit = UserDefaults.standard.object(forKey: "tenBit") as? Bool ?? true
            options.allowedPlacements = Set((UserDefaults.standard.array(forKey: "allowedPlacements") as? [Int]) ?? [0, 1, 2])
            options.defaultPlacement = UserDefaults.standard.integer(forKey: "defaultPlacement")
            if options.bitrate == 0 { options.bitrate = UserDefaults.standard.integer(forKey: "ownMegabits") * 1_000_000 }
        }
        host = MacHost(options: options)
        silenceSpeakers = options.silenceSpeakers
        allowClipboard = options.allowClipboard
        allowFiles = options.allowFiles
        advertise = options.advertise
        host.onClient = { [weak self] c in
            // The same device with another display (it changed its settings) keeps its time.
            let before = self?.client
            var c = c
            if let b = before, c != nil { c?.since = b.since }
            self?.client = c
            if let self, let c, before == nil { self.announce(c.name) }
            if let self, c != nil, before == nil {
                hostLog("host: trusted devices: \(self.host.devices.map { "\($0.name) \($0.fingerprint.prefix(8))\($0.connected ? " (connected)" : "")" }.joined(separator: ", "))")
                if let t = options.testAllow {
                    DispatchQueue.main.asyncAfter(deadline: .now() + t) { self.setAllowed(clipboard: true, files: true) }
                    DispatchQueue.main.asyncAfter(deadline: .now() + t + 4) { self.setAllowed(clipboard: false, files: false) }
                }
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
        if options.loginWindow {
            // Starts and stops with the login window being shown (shareThePort, each second).
        } else if unattended {
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
            // The login window's host may still hold the port just after logging in: tried again each second.
            if !host.isRunning { yielded = true }
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
    /// A new device waiting for a yes here, with the code to compare.
    @Published private(set) var pendingDevice: MacHost.Pending?
    private var testDeciding = false

    func decide(_ accept: Bool) {
        let who = pendingDevice?.name ?? "a device"
        if host.decide(accept) { hostLog("host: \(accept ? "paired" : "turned away") \(who)") }
        pendingDevice = nil
    }

    func beginPairing() {
        guard host.isRunning else { return }
        pairedJustNow = false
        // Opened by hand here: the new device waits until its code has been compared.
        host.setConfirm(true)
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
        shareThePort()
        if options.loginWindow { return }
        let sr = CGPreflightScreenCaptureAccess(), ax = AXIsProcessTrusted()
        if sr != screenRecording { screenRecording = sr }
        if ax != accessibility { accessibility = ax }
        guard host.isRunning else {
            if !devices.isEmpty { devices = [] }
            return
        }
        let list = host.devices
        if list != devices { devices = list }
        let cam = HostCamera.installed
        if cam != cameraInstalled { cameraInstalled = cam }
        let mic = HostMicrophone.installed
        if mic != microphoneInstalled { microphoneInstalled = mic }
        host.refreshFeatures()  // the camera and the microphone are offered while they are installed
        let waiting = host.pending
        if waiting != pendingDevice {
            if let w = waiting, pendingDevice?.fingerprint != w.fingerprint {
                hostLog("host: \(w.name) asks to pair, code \(w.code)")
                if let t = options.testDecide, !testDeciding {
                    testDeciding = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + abs(t)) {
                        self.testDeciding = false
                        self.decide(t >= 0)
                    }
                }
            }
            pendingDevice = waiting
        }
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
    /// What the pairing code says (PairLink): this Mac's name, its addresses and its key.
    var pairLink: String {
        guard !fingerprint.isEmpty else { return "" }
        let withPort = { (h: String) in self.port == "7791" || self.port.isEmpty ? h : "\(h):\(self.port)" }
        // Addresses first (they work without name lookup), then the Mac's own name.
        let list = addresses.map(withPort) + [withPort(ProcessInfo.processInfo.hostName)]
        return PairLink(name: options.name, addresses: list, fingerprint: fingerprint).url
    }

    var addressToEnter: String {
        let host = ProcessInfo.processInfo.hostName
        return port == "7791" ? host : "\(host):\(port)"
    }
}

final class HostAppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private let controller: HostController
    private var statusItem: NSStatusItem?
    private var menuPanel: MenuPanel?

    private func closePanel() { menuPanel?.close(statusItem?.button) }
    private var windows: [String: NSWindow] = [:]
    private var sinks: Set<AnyCancellable> = []
    static let firstRunKey = "firstRunDone"

    init(controller: HostController) {
        self.controller = controller
    }

    func applicationDidFinishLaunching(_ note: Notification) {
        if controller.options.loginWindow {
            // At the login window there is no menu bar to sit in: only the host.
            controller.launch()
            return
        }
        buildMenu()
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.target = self
        item.button?.action = #selector(togglePanel)
        statusItem = item

        menuPanel = MenuPanel(
            content: HostPanel(
                controller: controller,
                pair: { [weak self] in self?.closePanel(); self?.showPairing() },
                settings: { [weak self] in self?.closePanel(); self?.showSettings() },
                quit: { NSApp.terminate(nil) },
                close: { [weak self] in self?.closePanel() }
            ).tint(Tokens.tint).frame(width: 340))

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

    /// For tests: press the menu-bar item as a click does, and log where the item and the panel are.
    func testPanel() {
        guard let button = statusItem?.button else { return hostLog("panel: no menu-bar item") }
        button.performClick(nil)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [self] in
            let item = button.window?.frame ?? .zero
            let panel = menuPanel?.frame ?? .zero
            let screens = NSScreen.screens.map { "\($0.localizedName) \($0.frame) visible \($0.visibleFrame)" }.joined(separator: " | ")
            hostLog("panel: shown \(menuPanel?.isShown ?? false); item window \(item) on \(button.window?.screen?.localizedName ?? "no screen"); panel window \(panel) on \(menuPanel?.screenName ?? "?"); gap under the menu bar \(item.minY - panel.maxY) pt; screens: \(screens)")
            if let cg = CGWindowListCreateImage(.infinite, .optionOnScreenOnly, kCGNullWindowID, [.bestResolution]) {
                let rep = NSBitmapImageRep(cgImage: cg)
                let url = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent("Library/Logs/remoteman-host-panel.png")
                try? rep.representation(using: .png, properties: [:])?.write(to: url)
                hostLog("panel: picture of the screen saved to \(url.path) (\(cg.width)x\(cg.height))")
            }
        }
    }

    @objc private func togglePanel() {
        guard let button = statusItem?.button, let menuPanel else { return }
        if menuPanel.isShown {
            menuPanel.close(button)
        } else {
            // An accessory app has to come forward for its panel to take the keyboard.
            NSApp.activate(ignoringOtherApps: true)
            menuPanel.show(under: button)
        }
    }

    // MARK: Windows

    private func show<V: View>(
        _ key: String, title: String, size: NSSize, resizable: Bool = false, plain: Bool = false, _ view: () -> V
    ) {
        closePanel()
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
