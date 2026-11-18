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
            host: c.address, pair: !c.paired, name: DeviceDisplay.name, display: display, placement: c.placementChoice)
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
        model.display = DeviceDisplay.current(for: full, displayScale: UIScreen.main.scale, measured: session.refreshMeter.rate)
        guard !started else { return }
        started = true
        // Give the refresh meter a moment before an unattended connect reads it.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
            let d = DeviceDisplay.current(for: full, displayScale: UIScreen.main.scale, measured: session.refreshMeter.rate)
            model.display = d
            Automation.run(session: session, display: d)
        }
    }
}
