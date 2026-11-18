// The macOS viewer app: a connect form, then the stream filling the window, with a stats overlay.
//
// For unattended runs the app also takes arguments:
//   --host name[:port]  connect at launch        --pair yes        accept a host not seen before
//   --play file.rmrec   play a recording          --seconds N       stop after N seconds and quit (--verbose yes: stats each second)
//   --snapshot out.png  save the picture at the end
//   --script "move:.5,.5;wait:1;shot:a.png"   drive input and take pictures (see runScript)
//   --placement extend|main|only   where this display sits among the host's own
//   --frame-log out.csv   every frame's number, size and arrival time, and the longest waits
//   --view-only yes     send no input            --bench dir       time decode of the .rmrec files there (--exit yes to quit after)
//   --size WxH --scale S --refresh HZ --decoders av1,hevc444,hevc   override what the hello says
// Every option takes a value: AppKit reads the arguments too, as "-key value" pairs, and takes
// a word left over for a document to open, which keeps the window from appearing.
import ImageIO
import SwiftUI
import UniformTypeIdentifiers

#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

@main
struct RemotemanApp: App {
    @StateObject private var session = ViewerSession()

    init() {
        // Unattended runs end with exit(); without this the next launch stops at "reopen windows?".
        UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true, "NSQuitAlwaysKeepsWindows": false])
        setvbuf(stdout, nil, _IOLBF, 0)
    }

    var body: some Scene {
        #if os(macOS)
            WindowGroup("remoteman") {
                ContentView().environmentObject(session).frame(minWidth: 480, minHeight: 320)
            }
            .defaultSize(width: 1376, height: 1032)
        #else
            WindowGroup {
                ContentView().environmentObject(session)
                    .persistentSystemOverlays(.hidden)
                    .statusBarHidden()
            }
        #endif
    }
}

struct ContentView: View {
    @EnvironmentObject private var session: ViewerSession
    @AppStorage("host") private var host = ""
    @State private var pair = false
    @State private var started = false
    @State private var keyboardShown = false
    @State private var placement = ViewerSession.Placement.extend

    private var placementNote: String {
        switch placement {
        case .extend: return "Added beside the computer's own screens."
        case .main: return "Becomes the computer's main screen (menu bar, new windows) while connected."
        case .only: return "The computer's own screens go off while connected. Not every computer can do this."
        }
    }
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.black
                if let report = session.report {
                    ScrollView {
                        Text(report).font(.system(size: 12, design: .monospaced)).foregroundStyle(.white)
                            .textSelection(.enabled).padding(24).frame(maxWidth: .infinity, alignment: .leading)
                    }
                } else if session.isActive {
                    ViewerRepresentable(session: session, keyboardShown: keyboardShown)
                    overlay
                } else {
                    RefreshProbe(meter: session.refreshMeter).frame(width: 1, height: 1).opacity(0)
                    connectForm(size: geo.size)
                }
            }
            .onAppear {
                guard !started else { return }
                started = true
                // Give the refresh meter a moment before an unattended connect reads it.
                let size = geo.size
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) {
                    Automation.run(session: session, display: display(for: size))
                }
            }
        }
        .ignoresSafeArea()
    }

    private func display(for size: CGSize) -> ViewerSession.Display {
        #if os(macOS)
            let nominal = NSScreen.main?.maximumFramesPerSecond ?? 60
        #else
            let nominal = UIScreen.main.maximumFramesPerSecond
        #endif
        // What the display link really ticks at, when it has been measured and is no more than
        // the mode claims.
        let measured = session.refreshMeter.rate
        let refresh = measured.map { $0 >= 24 && $0 <= nominal ? $0 : nominal } ?? nominal
        // The panel's real pixels: with "More Space" an iPad lays out at a scale that isn't its
        // panel's, and SwiftUI's displayScale would ask for more pixels than exist.
        #if os(macOS)
            let scale = displayScale
        #else
            let scale = UIScreen.main.nativeScale
        #endif
        // Encoders want even sizes.
        let w = Int((size.width * scale).rounded()) & ~1
        let h = Int((size.height * scale).rounded()) & ~1
        return ViewerSession.Display(width: max(w, 320), height: max(h, 240), scale: Float(scale), refresh: refresh)
    }

    private var deviceName: String {
        #if os(macOS)
            Host.current().localizedName ?? "Mac"
        #else
            UIDevice.current.name
        #endif
    }

    private func connectForm(size: CGSize) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("remoteman").font(.largeTitle.weight(.semibold))
            Text("Connect to a PC running remoteman-agent.").foregroundStyle(.secondary)
            TextField("Host, for example blackbox or 10.0.0.76:7791", text: $host)
                .textFieldStyle(.roundedBorder)
                #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                #endif
                .onSubmit(connect)
            Toggle("Pair with this host (first connection only)", isOn: $pair)
            Picker("This device is its", selection: $placement) {
                ForEach(ViewerSession.Placement.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .onChange(of: host) { _, h in placement = .saved(for: h.trimmingCharacters(in: .whitespaces)) }
            .onAppear { placement = .saved(for: host.trimmingCharacters(in: .whitespaces)) }
            Text(placementNote).font(.footnote).foregroundStyle(.secondary)
            HStack {
                Button("Connect", action: connect)
                    .buttonStyle(.borderedProminent)
                    .disabled(host.trimmingCharacters(in: .whitespaces).isEmpty)
                if case .connecting(let h) = session.state {
                    ProgressView().controlSize(.small)
                    Text("Connecting to \(h)…").foregroundStyle(.secondary)
                }
            }
            if case .failed(let reason) = session.state {
                Text(reason).foregroundStyle(.red).textSelection(.enabled)
            }
            Text("This device decodes: \(ViewerSession.decoders)").font(.footnote).foregroundStyle(.secondary)
        }
        .padding(28)
        .frame(maxWidth: 460)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16))
        .environment(\.colorScheme, .dark)
        .padding()
        .onAppear { lastSize = size }
        .onChange(of: size) { _, s in lastSize = s }
    }

    @State private var lastSize: CGSize = .zero

    private func connect() {
        let h = host.trimmingCharacters(in: .whitespaces)
        guard !h.isEmpty else { return }
        placement.save(for: h)
        session.connect(host: h, pair: pair, name: deviceName, display: display(for: lastSize), placement: placement)
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
                    #if os(iOS)
                        Button(keyboardShown ? "Hide keyboard" : "Keyboard") { keyboardShown.toggle() }
                    #endif
                    Button("Disconnect") {
                        keyboardShown = false
                        session.disconnect()
                    }
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

