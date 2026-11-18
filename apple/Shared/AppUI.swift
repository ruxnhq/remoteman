// The app: a connect form, then the stream filling the window (macOS) or the screen (iOS),
// with a stats overlay. The same code on both.
//
// For unattended runs the app also takes arguments:
//   --host name[:port]  connect at launch        --pair yes        accept a host not seen before
//   --play file.rmrec   play a recording          --seconds N       stop after N seconds and quit (--verbose yes: stats each second)
//   --snapshot out.png  save the picture at the end
//   --script "move:.5,.5;wait:1;shot:a.png"   drive input and take pictures (see runScript)
//   --placement extend|main|only   where this display sits among the host's own
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
        var bench: String?
        var exitAfter = false
        var script: String?
        var verbose = false
        var placement = ViewerSession.Placement.extend
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
            case "--view-only": session.viewOnly = ["yes", "1", "true"].contains(value())
            case "--script": script = value()
            case "--placement": placement = ViewerSession.Placement(rawValue: value()) ?? .extend
            case "--verbose": verbose = ["yes", "1", "true"].contains(value())
            case "--bench": bench = value()
            case "--exit": exitAfter = ["yes", "1", "true"].contains(value())
            default: break
            }
        }
        #if os(macOS)
            let name = Host.current().localizedName ?? "Mac"
        #else
            let name = UIDevice.current.name
        #endif
        if let bench {
            runBenchmarks(in: resolve(bench), session: session, exitAfter: exitAfter)
            return
        }
        if let play {
            session.play(recording: resolve(play))
        } else if let host {
            print("remoteman: hello \(display.width)x\(display.height) scale \(display.scale) @\(display.refresh), decoders \(decoders), placement \(placement.rawValue)")
            session.connect(host: host, pair: pair, name: name, display: display, decoders: decoders, placement: placement)
        } else {
            return
        }
        if let script {
            runScript(script, session: session)
            return
        }
        guard let seconds else { return }
        var elapsed = 0.0
        Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { t in
            elapsed += 1
            // Off by default: printing from the main thread can hold up the display link.
            if verbose { print("remoteman: [\(Int(elapsed)) s] \(session.state)\n\(session.statsText)") }
            if case .failed(let reason) = session.state {
                print("remoteman: failed: \(reason)")
                t.invalidate()
                exit(1)
            }
            guard elapsed >= seconds else { return }
            t.invalidate()
            if let snapshot {
                if let image = session.renderer?.snapshot() {
                    let ok = writePNG(image, to: resolve(snapshot))
                    print("remoteman: snapshot \(image.width)x\(image.height) \(ok ? "saved to" : "NOT saved to") \(snapshot)")
                } else {
                    print("remoteman: no picture to snapshot")
                }
            }
            if let c = session.client {
                let h = c.host
                print("remoteman: host \(h.name) (\(h.os)) \(h.fingerprint)")
            }
            print("remoteman: \(session.summary(skip: 3))")
            session.disconnect()
            // Give the goodbye a moment to leave.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(0) }
        }
    }

    /// Drive a session from a script, for testing input against a live host, then leave and quit.
    /// Steps are separated by ";": move:x,y  down:b  up:b  click:b  scroll:dx,dy  key:usage
    /// keydown:usage  keyup:usage  text:words  touch:phase,slot,x,y  wait:seconds  shot:file.png  cursor (print the host's)
    static func runScript(_ script: String, session: ViewerSession) {
        DispatchQueue.global().async {
            // Wait for the stream and its first picture.
            for _ in 0..<150 {
                if session.renderer?.videoSize != nil { break }
                usleep(100_000)
            }
            guard let c = session.client, session.renderer?.videoSize != nil else {
                print("remoteman: script: no picture, state \(DispatchQueue.main.sync { session.state })")
                DispatchQueue.main.async {
                    session.disconnect()
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(1) }
                }
                return
            }
            for step in script.split(separator: ";") {
                let parts = step.split(separator: ":", maxSplits: 1).map(String.init)
                let op = parts[0].trimmingCharacters(in: .whitespaces)
                let arg = parts.count > 1 ? parts[1] : ""
                let n = arg.split(separator: ",").compactMap { Float($0.trimmingCharacters(in: .whitespaces)) }
                func usage() -> UInt16 { UInt16(arg.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "0x", with: ""), radix: arg.contains("0x") ? 16 : 10) ?? 0 }
                switch op {
                case "move" where n.count == 2: c.pointerTo(x: n[0], y: n[1])
                case "down" where n.count == 1: c.button(UInt8(n[0]), down: true)
                case "up" where n.count == 1: c.button(UInt8(n[0]), down: false)
                case "click" where n.count == 1:
                    c.button(UInt8(n[0]), down: true)
                    usleep(60_000)
                    c.button(UInt8(n[0]), down: false)
                case "scroll" where n.count == 2: c.scroll(dx: n[0], dy: n[1])
                case "key":
                    c.key(usage: usage(), down: true)
                    usleep(40_000)
                    c.key(usage: usage(), down: false)
                case "keydown": c.key(usage: usage(), down: true)
                case "keyup": c.key(usage: usage(), down: false)
                case "text": c.text(arg)
                case "touch" where n.count == 4: c.touch(phase: UInt8(n[0]), slot: UInt8(n[1]), x: n[2], y: n[3])
                case "wait" where n.count == 1: usleep(UInt32(n[0] * 1_000_000))
                case "cursor":
                    let (at, shape) = DispatchQueue.main.sync { (session.hostCursorAt, session.hostCursor) }
                    let s = shape.map { $0.map { "\($0.image.width)x\($0.image.height) hot \($0.hotX),\($0.hotY)" } ?? "none" } ?? "not sent"
                    print("remoteman: host cursor at \(at.map { String(format: "%.3f, %.3f visible %@", $0.x, $0.y, $0.visible ? "yes" : "no") } ?? "unknown"), shape \(s)")
                case "shot":
                    if let image = session.renderer?.snapshot() {
                        let ok = writePNG(image, to: resolve(arg))
                        print("remoteman: shot \(image.width)x\(image.height) \(ok ? "saved to" : "NOT saved to") \(arg)")
                    } else {
                        print("remoteman: shot: no picture")
                    }
                default: print("remoteman: script: bad step \(step)")
                }
            }
            DispatchQueue.main.async {
                print("remoteman: script done\n\(session.takeStats().text)")
                session.disconnect()
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(0) }
            }
        }
    }

    /// A path as given if absolute, otherwise inside the app's Documents folder (where files can
    /// be put on and taken off a device).
    static func resolve(_ path: String) -> URL {
        if path.hasPrefix("/") { return URL(fileURLWithPath: path) }
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        return path == "." || path == "Documents" ? docs : docs.appendingPathComponent(path)
    }

    /// Time hardware decode of every recording in `dir`, paced at the stream's rate, and leave
    /// the report on screen, on stdout and in bench-result.txt beside the recordings.
    static func runBenchmarks(in dir: URL, session: ViewerSession, exitAfter: Bool = false) {
        session.report = "Timing decode…"
        DispatchQueue.global(qos: .userInitiated).async {
            let files = ((try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [])
                .filter { $0.pathExtension == "rmrec" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
            var out = "remoteman decode timing · \(deviceModel()) · \(ProcessInfo.processInfo.operatingSystemVersionString)\n\n"
            if files.isEmpty { out += "no .rmrec files in \(dir.path)\n" }
            for f in files {
                out += runBench(f, paced: true, loops: 3) + "\n"
                let sofar = out
                DispatchQueue.main.async { session.report = sofar }
            }
            out += "done\n"
            try? out.write(to: dir.appendingPathComponent("bench-result.txt"), atomically: true, encoding: .utf8)
            print(out)
            let done = out
            DispatchQueue.main.async {
                session.report = done
                if exitAfter { exit(0) }
            }
        }
    }

    static func deviceModel() -> String {
        var size = 0
        let key = ProcessInfo.processInfo.isiOSAppOnMac ? "hw.model" : "hw.machine"
        sysctlbyname(key, nil, &size, nil, 0)
        var buf = [CChar](repeating: 0, count: max(size, 1))
        sysctlbyname(key, &buf, &size, nil, 0)
        return String(cString: buf)
    }

    static func writePNG(_ image: CGImage, to url: URL) -> Bool {
        guard let dest = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)
        else { return false }
        CGImageDestinationAddImage(dest, image, nil)
        return CGImageDestinationFinalize(dest)
    }
}
