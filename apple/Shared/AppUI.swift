// The app: a connect form, then the stream filling the window (macOS) or the screen (iOS),
// with a stats overlay. The same code on both.
//
// For unattended runs the app also takes arguments:
//   --host name[:port]  connect at launch        --pair yes        accept a host not seen before
//   --play file.rmrec   play a recording          --seconds N       stop after N seconds and quit
//   --snapshot out.png  save the picture at the end
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
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.black
                if session.isActive {
                    ViewerRepresentable(session: session)
                    overlay
                } else {
                    connectForm(size: geo.size)
                }
            }
            .onAppear {
                guard !started else { return }
                started = true
                Automation.run(session: session, display: display(for: geo.size))
            }
        }
        .ignoresSafeArea()
    }

    private func display(for size: CGSize) -> ViewerSession.Display {
        #if os(macOS)
            let refresh = NSScreen.main?.maximumFramesPerSecond ?? 60
        #else
            let refresh = UIScreen.main.maximumFramesPerSecond
        #endif
        // Encoders want even sizes.
        let w = Int((size.width * displayScale).rounded()) & ~1
        let h = Int((size.height * displayScale).rounded()) & ~1
        return ViewerSession.Display(width: max(w, 320), height: max(h, 240), scale: Float(displayScale), refresh: refresh)
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
        session.connect(host: h, pair: pair, name: deviceName, display: display(for: lastSize))
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

/// Unattended runs, from the launch arguments.
enum Automation {
    static func run(session: ViewerSession, display: ViewerSession.Display) {
        var args = Array(CommandLine.arguments.dropFirst())
        var host: String?
        var play: String?
        var snapshot: String?
        var seconds: Double?
        var pair = false
        var display = display
        var decoders = ViewerSession.decoders
        while !args.isEmpty {
            let a = args.removeFirst()
            func value() -> String { args.isEmpty ? "" : args.removeFirst() }
            switch a {
            case "--host": host = value()
            case "--play": play = value()
            case "--snapshot": snapshot = value()
            case "--seconds": seconds = Double(value())
            case "--pair": pair = ["yes", "1", "true"].contains(value())
            case "--size":
                let p = value().lowercased().split(separator: "x").compactMap { Int($0) }
                if p.count == 2 { display.width = p[0]; display.height = p[1] }
            case "--scale": display.scale = Float(value()) ?? display.scale
            case "--refresh": display.refresh = Int(value()) ?? display.refresh
            case "--decoders": decoders = value()
            default: break
            }
        }
        #if os(macOS)
            let name = Host.current().localizedName ?? "Mac"
        #else
            let name = UIDevice.current.name
        #endif
        if let play {
            session.play(recording: URL(fileURLWithPath: play))
        } else if let host {
            print("remoteman: hello \(display.width)x\(display.height) scale \(display.scale) @\(display.refresh), decoders \(decoders)")
            session.connect(host: host, pair: pair, name: name, display: display, decoders: decoders)
        } else {
            return
        }
        guard let seconds else { return }
        var elapsed = 0.0
        Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { t in
            elapsed += 1
            print("remoteman: [\(Int(elapsed)) s] \(session.state)\n\(session.statsText)")
            if case .failed(let reason) = session.state {
                print("remoteman: failed: \(reason)")
                t.invalidate()
                exit(1)
            }
            guard elapsed >= seconds else { return }
            t.invalidate()
            if let snapshot {
                if let image = session.renderer?.snapshot() {
                    let ok = writePNG(image, to: URL(fileURLWithPath: snapshot))
                    print("remoteman: snapshot \(image.width)x\(image.height) \(ok ? "saved to" : "NOT saved to") \(snapshot)")
                } else {
                    print("remoteman: no picture to snapshot")
                }
            }
            if let c = session.client {
                let h = c.host
                print("remoteman: host \(h.name) (\(h.os)) \(h.fingerprint)")
            }
            session.disconnect()
            // Give the goodbye a moment to leave.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(0) }
        }
    }

    static func writePNG(_ image: CGImage, to url: URL) -> Bool {
        guard let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)
        else { return false }
        CGImageDestinationAddImage(dest, image, nil)
        return CGImageDestinationFinalize(dest)
    }
}
