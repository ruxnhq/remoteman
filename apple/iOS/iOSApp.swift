// The iPad and iPhone app: the list of computers, and from it pairing, connecting and the
// session, built to the approved design. `AppModel` is the one place that knows which computer
// is being connected to and what to do when a connection starts, fails or ends.
import SwiftUI
import UIKit

@main
struct RemotemanApp: App {
    init() {
        setvbuf(stdout, nil, _IOLBF, 0)
    }

    var body: some Scene {
        WindowGroup {
            WindowRoot(connectTo: nil)
        }
        // "Connect in New Window" (iPad): a window that opens straight into one computer.
        WindowGroup(for: UUID.self) { id in
            WindowRoot(connectTo: id.wrappedValue)
        }
    }
}

/// One window: its own session, over the list of computers every window shares.
struct WindowRoot: View {
    let connectTo: UUID?
    @StateObject private var model = AppModel()
    @State private var started = false

    var body: some View {
        RootView()
            .environmentObject(model)
            .environmentObject(model.session)
            .environmentObject(model.store)
            .tint(Theme.tint)
            .onAppear {
                guard !started, let id = connectTo, let c = model.store.computer(id) else { return }
                started = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { model.connect(c) }
            }
    }
}

final class AppModel: ObservableObject {
    let session = ViewerSession()
    let store = ComputerStore.shared
    /// The computer being connected to or shown; nil for an unattended run started from arguments.
    @Published var current: Computer?
    /// What this device asks for; set from the root view's size.
    var display = ViewerSession.Display(width: 1920, height: 1080, scale: 2, refresh: 60)
    /// A computer added from a scanned code answered with another key.
    @Published var scanMismatch: String?
    private var watching: Any?
    private var nearby: Any?

    init() {
        // What a computer seen on this network says of its network cards is kept for waking it.
        nearby = Nearby.shared.$found.sink { [weak self] _ in
            DispatchQueue.main.async {
                guard let self else { return }
                for c in self.store.computers {
                    guard let f = Nearby.shared.seen(fingerprint: c.fingerprint), !f.macs.isEmpty, f.macs != c.macs else { continue }
                    var u = c
                    u.macs = f.macs
                    self.store.update(u)
                }
            }
        }
        ViewerSession.watchNetwork()
        // Looking by itself only once the person has asked for it (and the system has been answered).
        if Nearby.wanted, !CommandLine.arguments.contains(where: { $0.hasPrefix("--") }) { Nearby.shared.look() }
        session.showStats = UserDefaults.standard.bool(forKey: "figuresInNewSessions")
        watching = session.$state.sink { [weak self] state in
            // The state arrives before it is stored; act on the next turn of the main queue.
            DispatchQueue.main.async { self?.stateChanged(state) }
        }
    }

    /// Face ID (or the passcode) first, for a computer that asks for it.
    func connect(_ c: Computer) {
        let ask = c.auth == "always" || (c.auth == nil && ViewerPrefs.requireAuth)
        unlocker.cancel()
        guard ask else { return connectNow(c) }
        Gate.pass(for: c.name) { [weak self] ok in
            if ok { self?.connectNow(c) } else { Haptics.failure() }
        }
    }

    /// For a locked computer that asks for its user's password.
    let unlocker = Unlocker()

    private func connectNow(_ c: Computer, unlock: String? = nil) {
        current = c
        // A computer that may be asleep is woken first; one that is awake takes no notice.
        if c.wake == true, let macs = c.macs, !macs.isEmpty, Nearby.shared.seen(fingerprint: c.fingerprint) == nil {
            Wake.send(macs: macs, lastAddress: c.address)
        }
        session.connect(
            host: c.address, pair: !c.paired, name: DeviceDisplay.name, display: asked(for: c), placement: c.placementChoice, unlock: unlock, attachOnly: c.wayIn == "attach")
    }

    /// "More space" for the computer in view: the same pixels drawn smaller.
    var moreSpace: Bool { current.map { UserDefaults.standard.bool(forKey: "moreSpace." + $0.address) } ?? false }

    private func asked(for c: Computer) -> ViewerSession.Display {
        var d = display
        if UserDefaults.standard.bool(forKey: "moreSpace." + c.address) { d.scale = max(1, d.scale * 0.75) }
        return d
    }

    /// Change something about the display mid-session: in place on a computer that can, by
    /// leaving and connecting again on one that can't.
    private func redisplay(_ c: Computer) {
        if session.reconfigure(display: asked(for: c), placement: c.placementChoice) { return }
        session.disconnect()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { self.connect(c) }
    }

    /// True if anything changed.
    @discardableResult
    func setPlacement(_ p: ViewerSession.Placement) -> Bool {
        guard var c = current, c.placementChoice != p else { return false }
        c.placementChoice = p
        store.update(c)
        current = c
        redisplay(c)
        return true
    }

    func setMoreSpace(_ on: Bool) {
        guard let c = current, on != moreSpace else { return }
        UserDefaults.standard.set(on, forKey: "moreSpace." + c.address)
        objectWillChange.send()
        redisplay(c)
    }

    /// The device was turned or its window resized while a session is on: the computer's
    /// display follows, where it can.
    func screenChanged() {
        guard let c = current, case .streaming = session.state else { return }
        session.reconfigure(display: asked(for: c))
    }

    /// Leave the session, keeping its last picture as the computer's thumbnail.
    func disconnect() {
        if let c = current, let image = session.renderer?.snapshot() { store.saveThumbnail(image, for: c) }
        session.disconnect()
        current = nil
    }

    func cancelConnecting() {
        session.disconnect()
        current = nil
    }

    private func stateChanged(_ state: ViewerSession.State) {
        // A mark for what the user was waiting on: connected, or it didn't work.
        if current != nil {
            if case .streaming = state { Haptics.success() }
            if case .failed = state { Haptics.failure() }
        }
        if case .streaming = state { unlocker.connected() }
        // Locked, and it wants its user's password: ask (or use the kept one) and connect again.
        if case .failed = state, let words = session.lockedAsk, let c = current {
            unlocker.locked(words: words, key: c.fingerprint ?? c.address, name: c.name) { [weak self] password in
                self?.connectNow(c, unlock: password)
            }
        }
        if case .failed = state, session.mustPairAgain, var c = current {
            c.paired = false
            store.update(c)
            current = c
        }
        guard case .streaming = state, var c = current, let host = session.client?.host else { return }
        // Added from a scanned code: any other computer answering at that address is turned away.
        if let expect = c.expect, !expect.isEmpty {
            guard host.fingerprint.lowercased() == expect else {
                session.disconnect()
                ViewerSession.forget(host: c.address)
                scanMismatch = "\(c.name) at \(c.address) is not the computer whose code you scanned. Nothing was paired."
                Haptics.failure()
                return
            }
            c.expect = nil
        }
        c.paired = true
        c.lastUsed = Date()
        if !host.os.isEmpty { c.os = host.os }
        if !host.fingerprint.isEmpty { c.fingerprint = host.fingerprint }
        if c.name == c.address, !host.name.isEmpty { c.name = host.name }
        store.update(c)
        current = c
    }
}

/// The sheet asking for a locked computer's password, over whatever the screen shows.
private struct UnlockPrompt: ViewModifier {
    @ObservedObject var unlocker: Unlocker
    func body(content: Content) -> some View {
        content.sheet(item: $unlocker.asking) { ask in
            ScrollView { UnlockSheet(ask: ask, unlocker: unlocker) }.presentationDetents([.large]).interactiveDismissDisabled()
        }
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var session: ViewerSession
    @State private var started = false

    var body: some View {
        GeometryReader { geo in
            ZStack {
                if let report = session.report {
                    Color.black.ignoresSafeArea()
                    ScrollView {
                        Text(report).font(.system(size: 12, design: .monospaced)).foregroundStyle(.white)
                            .textSelection(.enabled).padding(24).frame(maxWidth: .infinity, alignment: .leading)
                    }
                } else {
                    switch session.state {
                    case .streaming:
                        SessionScreen()
                    case .connecting:
                        GettingInScreen(failure: nil)
                    case .failed(let reason) where model.current != nil:
                        GettingInScreen(failure: reason)
                    default:
                        ComputersScreen()
                        RefreshProbe(meter: session.refreshMeter).frame(width: 1, height: 1).opacity(0)
                    }
                }
            }
            .onAppear { sized(geo) }
            .onChange(of: geo.size) { _, _ in sized(geo) }
        }
        .ignoresSafeArea(edges: session.isActive ? .all : [])
        .modifier(UnlockPrompt(unlocker: model.unlocker))
        .alert("Not the computer you scanned", isPresented: Binding(get: { model.scanMismatch != nil }, set: { if !$0 { model.scanMismatch = nil } })) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(model.scanMismatch ?? "")
        }
        .statusBarHidden(session.isActive)
        .persistentSystemOverlays(session.isActive ? .hidden : .automatic)
    }

    /// The whole screen is what the host is asked for, whatever part of it the list uses.
    private func sized(_ geo: GeometryProxy) {
        let insets = geo.safeAreaInsets
        let full = CGSize(
            width: geo.size.width + insets.leading + insets.trailing, height: geo.size.height + insets.top + insets.bottom)
        let now = DeviceDisplay.current(for: full, displayScale: UIScreen.main.scale, measured: session.refreshMeter.rate)
        if now.width != model.display.width || now.height != model.display.height {
            model.display = now
            // Once the turn has settled.
            let size = (now.width, now.height)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.7) {
                if (model.display.width, model.display.height) == size { model.screenChanged() }
            }
        } else if !session.isActive {
            model.display = now
        }
        guard !started else { return }
        started = true
        // Give the refresh meter a moment before an unattended connect reads it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            let d = DeviceDisplay.current(for: full, displayScale: UIScreen.main.scale, measured: session.refreshMeter.rate)
            model.display = d
            if CommandLine.arguments.contains("--demo-session") {
                // The session screen with its controls and no computer behind it.
                model.current = Computer(name: "Demo computer", address: "demo")
                session.demo(name: "Demo computer")
                // --demo-expanded: with the controls open, for a picture where nothing can tap.
                if CommandLine.arguments.contains("--demo-expanded") {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { session.toggleControls?() }
                }
                return
            }
            Automation.run(session: session, display: d)
        }
    }
}
