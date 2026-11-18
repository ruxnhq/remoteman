// The iPad and iPhone app: the list of computers, and from it pairing, connecting and the
// session, built to the approved design. `AppModel` is the one place that knows which computer
// is being connected to and what to do when a connection starts, fails or ends.
import SwiftUI
import UIKit

@main
struct RemotemanApp: App {
    @StateObject private var model = AppModel()

    init() {
        setvbuf(stdout, nil, _IOLBF, 0)
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(model.session)
                .environmentObject(model.store)
                .tint(Theme.tint)
        }
    }
}

final class AppModel: ObservableObject {
    let session = ViewerSession()
    let store = ComputerStore()
    /// The computer being connected to or shown; nil for an unattended run started from arguments.
    @Published var current: Computer?
    /// What this device asks for; set from the root view's size.
    var display = ViewerSession.Display(width: 1920, height: 1080, scale: 2, refresh: 60)
    private var watching: Any?

    init() {
        session.showStats = UserDefaults.standard.bool(forKey: "figuresInNewSessions")
        watching = session.$state.sink { [weak self] state in
            // The state arrives before it is stored; act on the next turn of the main queue.
            DispatchQueue.main.async { self?.stateChanged(state) }
        }
    }

    func connect(_ c: Computer) {
        current = c
        session.connect(
            host: c.address, pair: !c.paired, name: DeviceDisplay.name, display: asked(for: c), placement: c.placementChoice)
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
        if case .failed = state, session.mustPairAgain, var c = current {
            c.paired = false
            store.update(c)
            current = c
        }
        guard case .streaming = state, var c = current, let host = session.client?.host else { return }
        c.paired = true
        c.lastUsed = Date()
        if !host.os.isEmpty { c.os = host.os }
        if !host.fingerprint.isEmpty { c.fingerprint = host.fingerprint }
        if c.name == c.address, !host.name.isEmpty { c.name = host.name }
        store.update(c)
        current = c
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
