// The macOS viewer app: the list of computers, connecting, and the session, in one window that
// changes with what is going on, plus the Settings window.
//
// For unattended runs the app also takes arguments (Shared/Automation.swift): with --host,
// --play, --bench or --opus-test the window shows nothing but the stream, as it always has,
// and the run is driven from the arguments.
import AppKit
import Combine
import SwiftUI

/// How the app was started.
enum Launch {
    /// Started to run by itself from its arguments: the window is the stream and nothing else.
    static let automated: Bool = {
        let args = CommandLine.arguments.dropFirst()
        return ["--host", "--play", "--bench", "--opus-test", "--nearby", "--wake", "--keychain-test"].contains { args.contains($0) }
    }()
}

@main
struct RemotemanApp: App {
    @StateObject private var model = AppModel(store: UIShots.directory != nil ? UIShots.sampleStore : ComputerStore())

    init() {
        // Unattended runs end with exit(); without this the next launch stops at "reopen windows?".
        UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true, "NSQuitAlwaysKeepsWindows": false])
        setvbuf(stdout, nil, _IOLBF, 0)
    }

    var body: some Scene {
        Window("remoteman", id: "main") {
            RootView(session: model.session)
                .environmentObject(model)
                .frame(minWidth: 480, minHeight: 320)
                .tint(Tokens.tint)
        }
        .defaultSize(width: Launch.automated ? 1376 : 1120, height: Launch.automated ? 1032 : 720)

        Settings {
            SettingsView().environmentObject(model).tint(Tokens.tint)
        }
    }
}

/// What the window shows and why: the saved computers, which one is selected, which one a
/// session is running with, and the window's own chrome while a session is on.
final class AppModel: ObservableObject {
    /// Settings > General > Window controls.
    enum WindowControls: String {
        case titleBar, slideIn
        static let key = "windowControls"
        static var current: WindowControls {
            WindowControls(rawValue: UserDefaults.standard.string(forKey: key) ?? "") ?? .titleBar
        }
    }
    static let showFiguresKey = "showFiguresAtStart"

    let session = ViewerSession()
    let store: ComputerStore

    /// The window is only the stream, driven by the launch arguments.
    @Published var automated = Launch.automated
    @Published var selection: SavedComputer.ID?
    /// The computer a connection is being made to, or a session is running with.
    @Published private(set) var active: SavedComputer.ID?
    /// The first picture of the session has arrived.
    @Published private(set) var hasPicture = false
    /// Why the last connection to a computer failed or ended.
    @Published var errors: [SavedComputer.ID: String] = [:]
    @Published var addSheet = false
    /// The computer whose first connection just worked: "… is paired" is shown for it.
    @Published var pairedSheet: SavedComputer.ID?
    @Published private(set) var fullScreen = false
    /// The controls are over the picture (full screen, or a window without a title bar).
    @Published var barShown = false { didSet { if barShown != oldValue { applyChrome() } } }
    /// What the computer was asked for.
    @Published private(set) var requested: ViewerSession.Display?

    /// The picture area's size in points, kept up to date by the session screen.
    var pictureSize: CGSize = .zero
    weak var window: NSWindow? { didSet { if window !== oldValue { watchWindow() } } }

    private var started: Date?
    private var pairedNow = false
    private var reconnecting = false
    private var savedFrame: NSRect?
    private var pictureTimer: Timer?
    private var stateSink: AnyCancellable?
    private var nearbySink: AnyCancellable?
    private var monitor: Any?
    private var observers: [NSObjectProtocol] = []

    init(store: ComputerStore) {
        self.store = store
        selection = store.computers.first?.id
        stateSink = session.$state.sink { [weak self] s in
            // The value arrives before the session has it; act once it does.
            DispatchQueue.main.async { self?.stateChanged(s) }
        }
        nearbySink = Nearby.shared.$found.sink { [weak self] found in
            DispatchQueue.main.async { self?.noteNearby(found) }
        }
        // Looking by itself only once the person has asked for it (and macOS has been answered).
        if Nearby.wanted, !Launch.automated { Nearby.shared.look() }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.mouseMoved, .leftMouseDragged, .keyDown]) { [weak self] e in
            guard let self else { return e }
            return self.intercept(e)
        }
    }

    /// What a computer seen on this network says of its network cards is kept for waking it.
    func noteNearby(_ found: [Nearby.Found]) {
        for c in store.computers {
            guard let f = Nearby.shared.seen(fingerprint: c.fingerprint), !f.macs.isEmpty, f.macs != c.macs else { continue }
            store.update(c.id) { $0.macs = f.macs }
        }
    }

    var activeComputer: SavedComputer? { store[active] }
    var selected: SavedComputer? { store[selection] }

    // MARK: Connecting and leaving

    /// Touch ID (or the Mac's password) first, for a computer that asks for it.
    func connect(_ c: SavedComputer) {
        guard !session.isActive, active == nil else { return }
        let ask = c.auth == "always" || (c.auth.isEmpty && ViewerPrefs.requireAuth)
        unlocker.cancel()
        guard ask, !automated else { return connectNow(c) }
        Gate.pass(for: c.name) { [weak self] ok in
            if ok { self?.connectNow(c) }
        }
    }

    /// For a locked computer that asks for its user's password.
    let unlocker = Unlocker()

    private func connectNow(_ c: SavedComputer, unlock: String? = nil) {
        guard !session.isActive, active == nil else { return }
        errors[c.id] = nil
        selection = c.id
        active = c.id
        hasPicture = false
        started = nil
        pairedNow = false
        reconnecting = false
        let display = sessionDisplay()
        requested = display
        session.showStats = UserDefaults.standard.bool(forKey: Self.showFiguresKey)
        // A computer that may be asleep is woken first; one that is awake takes no notice.
        if c.wake, !c.macs.isEmpty, Nearby.shared.seen(fingerprint: c.fingerprint) == nil { Wake.send(macs: c.macs, lastAddress: c.address) }
        session.connect(host: c.address, pair: c.pairOnConnect, name: DeviceDisplay.name, display: display, placement: c.place, unlock: unlock)
        watchForPicture()
    }

    /// The session's chrome with no computer behind it (UIShots plays a recording into it).
    func beginPreview(_ c: SavedComputer) {
        selection = c.id
        active = c.id
        hasPicture = false
        started = nil
        requested = sessionDisplay()
        watchForPicture()
    }

    func disconnect() {
        reconnecting = false
        keepThumbnail()
        session.disconnect()
    }

    /// Leave and join again, for what can only be said at the start: where this display sits
    /// among the computer's own, and its size.
    /// The picture now, kept as the computer's thumbnail.
    func keepThumbnail() {
        guard !automated, let id = active, hasPicture, let image = session.renderer?.snapshot() else { return }
        store.saveThumbnail(image, for: id)
    }

    /// 60 Hz for those who turned the display's full rate off.
    private func capped(_ d: ViewerSession.Display) -> ViewerSession.Display {
        var d = d
        if !ViewerPrefs.fullRefresh { d.refresh = min(d.refresh, 60) }
        return d
    }

    /// `rejoin`: for what the computer is only told as a session starts (which stream it sends).
    func reconnect(placement: ViewerSession.Placement? = nil, rejoin: Bool = false) {
        guard let id = active, case .streaming = session.state else { return }
        if let placement { store.update(id) { $0.place = placement } }
        guard let c = store[id] else { return }
        let size = pictureSize.width > 0 ? pictureSize : CGSize(width: 1280, height: 748)
        let display = capped(DeviceDisplay.current(
            for: size, displayScale: window?.backingScaleFactor ?? 2, measured: session.refreshMeter.rate))
        // A computer that can change it in place does; the picture starts afresh by itself.
        if !rejoin, session.reconfigure(display: display, placement: c.place) {
            requested = display
            return
        }
        reconnecting = true
        hasPicture = false
        barShown = false
        session.disconnect()
        requested = display
        session.connect(host: c.address, pair: false, name: DeviceDisplay.name, display: display, placement: c.place)
        watchForPicture()
        applyChrome()
    }

    private func stateChanged(_ s: ViewerSession.State) {
        guard !automated, let id = active else { return }
        switch s {
        case .connecting:
            break
        case .streaming:
            reconnecting = false
            guard started == nil else { return }
            unlocker.connected()
            started = Date()
            let host = session.client?.host
            store.update(id) { c in
                c.lastUsed = Date()
                c.lastDuration = nil
                if let host {
                    if !host.os.isEmpty { c.os = host.os }
                    if !host.fingerprint.isEmpty { c.fingerprint = host.fingerprint }
                    // No name was given when it was added: take the computer's own.
                    if c.name == c.address, !host.name.isEmpty { c.name = host.name }
                }
                if c.pairOnConnect { pairedNow = true }
                c.pairOnConnect = false
                if c.paired == nil { c.paired = Date() }
            }
        case .idle:
            if reconnecting { return }
            finish(id, reason: nil)
        case .failed(let reason):
            if session.mustPairAgain {
                store.update(id) { $0.pairOnConnect = true }
                finish(id, reason: "This Mac was taken off its list of trusted devices. Turn pairing on there, then pair again.")
                return
            }
            let locked = started == nil ? session.lockedAsk : nil
            let computer = store[id]
            finish(id, reason: started == nil ? reason : "The connection ended: \(reason)")
            // Locked, and it wants its user's password: ask (or use the kept one) and connect again.
            if let locked, let c = computer {
                unlocker.locked(words: locked, key: c.fingerprint.isEmpty ? c.address : c.fingerprint, name: c.name) { [weak self] password in
                    self?.connectNow(c, unlock: password)
                }
            }
        }
    }

    private func finish(_ id: SavedComputer.ID, reason: String?) {
        if let started {
            store.update(id) { $0.lastDuration = Date().timeIntervalSince(started) }
        }
        errors[id] = reason
        reconnecting = false
        started = nil
        active = nil
        hasPicture = false
        barShown = false
        requested = nil
        pictureTimer?.invalidate()
        pictureTimer = nil
        if let w = window, let f = savedFrame, !w.styleMask.contains(.fullScreen) { w.setFrame(f, display: true, animate: true) }
        savedFrame = nil
        applyChrome()
        if pairedNow, reason == nil { pairedSheet = id }
        pairedNow = false
    }

    private func watchForPicture() {
        pictureTimer?.invalidate()
        pictureTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] t in
            guard let self, self.active != nil else { return t.invalidate() }
            guard case .streaming = self.session.state, self.session.renderer?.videoSize != nil else { return }
            t.invalidate()
            self.hasPicture = true
            self.applyChrome()
        }
    }

    // MARK: The window

    /// The session window's size (1280 × 800, or the screen in full screen), and from it what
    /// to ask the computer for: the picture area's pixels.
    private func sessionDisplay() -> ViewerSession.Display {
        var size = CGSize(width: 1280, height: 748)
        if let w = window {
            if w.styleMask.contains(.fullScreen) {
                size = w.frame.size
            } else {
                savedFrame = w.frame
                let visible = (w.screen ?? NSScreen.main)?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
                let target = NSSize(width: min(1280, visible.width), height: min(800, visible.height))
                // With the controls in the title bar the picture sits under it.
                let bar = WindowControls.current == .titleBar ? max(0, w.frame.height - w.contentLayoutRect.height) : 0
                size = CGSize(width: target.width, height: target.height - bar)
                var f = w.frame
                f.origin.x = max(visible.minX, min(f.midX - target.width / 2, visible.maxX - target.width))
                f.origin.y = max(visible.minY, min(f.maxY - target.height, visible.maxY - target.height))
                f.size = target
                w.setFrame(f, display: true, animate: true)
            }
        }
        return capped(DeviceDisplay.current(
            for: size, displayScale: window?.backingScaleFactor ?? 2, measured: session.refreshMeter.rate))
    }

    /// The session's controls are drawn over the picture, not in a title bar.
    var bare: Bool { active != nil && hasPicture && (fullScreen || WindowControls.current == .slideIn) }

    /// Without a title bar the window is only the picture, edge to edge.
    func applyChrome() {
        guard let w = window, !automated else { return }
        let hidden = bare && !fullScreen
        w.titlebarAppearsTransparent = hidden
        w.titleVisibility = hidden ? .hidden : .visible
        if hidden { w.styleMask.insert(.fullSizeContentView) }
    }

    private func watchWindow() {
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        guard let w = window else { return }
        fullScreen = w.styleMask.contains(.fullScreen)
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: NSWindow.didEnterFullScreenNotification, object: w, queue: .main) { [weak self] _ in
                self?.fullScreenChanged(true)
            })
        observers.append(
            center.addObserver(forName: NSWindow.didExitFullScreenNotification, object: w, queue: .main) { [weak self] _ in
                self?.fullScreenChanged(false)
            })
    }

    private func fullScreenChanged(_ on: Bool) {
        fullScreen = on
        barShown = false
        applyChrome()
        // The picture area has a new size: once the layout has settled, ask for that size.
        guard active != nil, hasPicture else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
            guard let self, self.fullScreen == on, self.hasPicture else { return }
            self.reconnect()
        }
    }

    func toggleFullScreen() { window?.toggleFullScreen(nil) }

    /// Events looked at before the picture gets them: the pointer at the top edge brings the
    /// controls in, and the two shortcuts that are this app's own stay here (the picture sends
    /// every other ⌘ shortcut to the computer).
    private func intercept(_ e: NSEvent) -> NSEvent? {
        guard !automated, active != nil, hasPicture, let w = window, e.window === w else { return e }
        if e.type == .keyDown {
            let mods = e.modifierFlags.intersection([.command, .option, .control, .shift])
            if mods == [.command, .option], e.keyCode == 34 {  // ⌥⌘I
                session.showStats.toggle()
                return nil
            }
            if mods == [.command, .control], e.keyCode == 3 {  // ⌃⌘F
                toggleFullScreen()
                return nil
            }
            return e
        }
        guard bare, let content = w.contentView else { return e }
        let fromTop = content.bounds.height - e.locationInWindow.y
        if !barShown, fromTop <= 3 {
            barShown = true
        } else if barShown, fromTop > (fullScreen ? 96 : 84) {
            barShown = false
        }
        return e
    }
}

/// The window's content: the stream alone on an unattended run; otherwise the computers, or
/// the session with one of them.
/// The sheet asking for a locked computer's password, over whatever the window shows.
private struct UnlockPrompt: ViewModifier {
    @ObservedObject var unlocker: Unlocker
    func body(content: Content) -> some View {
        content.sheet(item: $unlocker.asking) { UnlockSheet(ask: $0, unlocker: unlocker) }
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession
    @Environment(\.displayScale) private var displayScale
    @State private var started = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                // Measures the display's real refresh rate before any connection asks for it.
                RefreshProbe(meter: session.refreshMeter).frame(width: 1, height: 1).opacity(0)
                if model.automated {
                    AutomatedView(session: session)
                } else if let computer = model.activeComputer {
                    SessionScreen(session: session, computer: computer)
                } else {
                    ComputersView()
                }
            }
            .modifier(UnlockPrompt(unlocker: model.unlocker))
            .onAppear {
                guard !started else { return }
                started = true
                // Give the refresh meter a moment before an unattended connect reads it.
                let size = geo.size
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                    if UIShots.directory != nil { return UIShots.run(model: model) }
                    Automation.run(
                        session: session,
                        display: DeviceDisplay.current(for: size, displayScale: displayScale, measured: session.refreshMeter.rate))
                }
            }
        }
        .background(WindowReader { model.window = $0 })
    }
}

/// An unattended run: the stream filling the window, the figures as text, and a way out.
struct AutomatedView: View {
    @EnvironmentObject private var model: AppModel
    @ObservedObject var session: ViewerSession

    var body: some View {
        ZStack {
            Color.black
            if let report = session.report {
                ScrollView {
                    Text(report).font(.system(size: 12, design: .monospaced)).foregroundStyle(.white)
                        .textSelection(.enabled).padding(24).frame(maxWidth: .infinity, alignment: .leading)
                }
            } else if session.isActive {
                ViewerRepresentable(session: session)
                overlay
            } else {
                VStack(spacing: 12) {
                    if case .failed(let reason) = session.state {
                        Text(reason).foregroundStyle(.red).textSelection(.enabled).multilineTextAlignment(.center)
                    }
                    Button("Show Computers") { model.automated = false }
                }
                .padding(24)
            }
        }
        .ignoresSafeArea()
    }

    private var overlay: some View {
        VStack {
            HStack(alignment: .top) {
                if session.showStats, !session.statsText.isEmpty {
                    Text(session.statsText)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(.white)
                        .padding(8)
                        .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 8))
                        .allowsHitTesting(false)
                }
                Spacer()
                HStack(spacing: 6) {
                    Button(session.showStats ? "Hide stats" : "Stats") { session.showStats.toggle() }
                    Button("Disconnect") { session.disconnect() }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .focusable(false)
                .opacity(0.8)
            }
            Spacer()
        }
        .padding(10)
    }
}
