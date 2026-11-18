// Unattended runs: the apps take launch arguments so they can be driven from a shell and
// checked without anyone looking at them.
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

/// What this device asks the host for: its panel's pixels, scale and the refresh rate its
/// display link really delivers.
enum DeviceDisplay {
    static func current(for size: CGSize, displayScale: CGFloat, measured: Int?) -> ViewerSession.Display {
        #if os(macOS)
            let nominal = NSScreen.main?.maximumFramesPerSecond ?? 60
            let scale = displayScale
        #else
            let nominal = UIScreen.main.maximumFramesPerSecond
            // What iPadOS composes at: with "More Space" that is more than the panel has, and
            // the stream then maps one to one onto it.
            let scale = UIScreen.main.nativeScale
        #endif
        let refresh = measured.map { $0 >= 24 && $0 <= nominal ? $0 : nominal } ?? nominal
        // Encoders want even sizes.
        let w = Int((size.width * scale).rounded()) & ~1
        let h = Int((size.height * scale).rounded()) & ~1
        return ViewerSession.Display(width: max(w, 320), height: max(h, 240), scale: Float(scale), refresh: refresh)
    }

    static var name: String {
        #if os(macOS)
            Host.current().localizedName ?? "Mac"
        #else
            UIDevice.current.name
        #endif
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
        var frameLog: String?
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
            case "--opus-test":
                if ["yes", "1", "true"].contains(value()) {
                    let text = opusProbe()
                    print(text)
                    session.report = text
                    try? text.write(to: resolve("opus-test.txt"), atomically: true, encoding: .utf8)
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1) { exit(0) }
                    return
                }
            case "--frame-log": frameLog = value()
            case "--placement": placement = ViewerSession.Placement(rawValue: value()) ?? .extend
            case "--mute": SoundPlayer.silent = ["yes", "1", "true"].contains(value())
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
            let join = { session.connect(host: host, pair: pair, name: name, display: display, decoders: decoders, placement: placement) }
            if script?.contains("mem") == true { print("remoteman: memory before any session \(memory())") }
            join()
            if let script {
                runScript(script, session: session, join: join)
                return
            }
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
            if let frameLog {
                let r = session.frameReport()
                try? r.csv.write(to: resolve(frameLog), atomically: true, encoding: .utf8)
                print("remoteman: frame log \(frameLog)\n\(r.text)")
            }
            session.disconnect()
            // Give the goodbye a moment to leave.
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { exit(0) }
        }
    }

    /// Drive a session from a script, for testing input against a live host, then leave and quit.
    /// Steps are separated by ";": move:x,y  down:b  up:b  click:b  scroll:dx,dy  key:usage
    /// keydown:usage  keyup:usage  text:words  touch:phase,slot,x,y  wait:seconds  shot:file.png  cursor (print the host's)
    /// features  stats  bitrate:bits  resize:w,h[,scale[,refresh]]  placement:extend|main|only
    /// sound:on|off  mic:on|off  clipboard:on|off  file:path  transfers
    /// leave (end the session, stay open)  join (connect again as at launch)  mem (this app's memory)
    static func runScript(_ script: String, session: ViewerSession, join: (() -> Void)? = nil) {
        DispatchQueue.global().async {
            // Wait for the stream and its first picture.
            for _ in 0..<150 {
                if session.renderer?.videoSize != nil { break }
                usleep(100_000)
            }
            guard var c = session.client, session.renderer?.videoSize != nil else {
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
                // Features. stats: the figures since the last one (prints the picture's size and rate).
                case "features": print("remoteman: host features \(DispatchQueue.main.sync { session.features }.sorted().joined(separator: ","))")
                case "stats":
                    let t = DispatchQueue.main.sync { () -> String in
                        let st = session.stats  // the last half second, as the app shows it
                        return "\(st.text)\n\(String(format: "%.1f", st.megabitsPerSecond)) Mb/s received · host says \(session.hostBitrate) bit/s · limit \(session.bitrateLimit)"
                    }
                    print("remoteman: stats\n\(t)")
                case "bitrate" where n.count == 1: DispatchQueue.main.sync { session.setBitrateLimit(Int(n[0])) }
                case "resize" where n.count >= 2:
                    DispatchQueue.main.sync {
                        var d = session.display ?? ViewerSession.Display(width: 1920, height: 1080, scale: 1, refresh: 60)
                        d.width = Int(n[0])
                        d.height = Int(n[1])
                        if n.count > 2 { d.scale = n[2] }
                        if n.count > 3 { d.refresh = Int(n[3]) }
                        print("remoteman: resize to \(d.width)x\(d.height) scale \(d.scale) @\(d.refresh): \(session.reconfigure(display: d) ? "asked" : "host can't")")
                    }
                case "placement":
                    DispatchQueue.main.sync {
                        let p = ViewerSession.Placement(rawValue: arg.trimmingCharacters(in: .whitespaces)) ?? .extend
                        print("remoteman: placement \(p.rawValue): \(session.reconfigure(placement: p) ? "asked" : "host can't")")
                    }
                case "sound": DispatchQueue.main.sync { session.setSound(arg.contains("on")) }
                case "mic": DispatchQueue.main.sync { session.setMic(arg.contains("on")) }
                case "clipboard": DispatchQueue.main.sync { session.setClipboard(arg.contains("on")) }
                case "file": DispatchQueue.main.sync { session.send(file: resolve(arg.trimmingCharacters(in: .whitespaces))) }
                case "camera": DispatchQueue.main.sync { session.setCamera(arg.contains("on")) }
                case "micreport": print("remoteman: microphone: \(DispatchQueue.main.sync { session.micReport })")
                case "camerareport": print("remoteman: camera: \(DispatchQueue.main.sync { session.cameraReport })")
                case "tone" where n.count == 1: print("remoteman: tone: \(DispatchQueue.main.sync { TestTone.play(seconds: Double(n[0])) })")
                case "soundreport": print("remoteman: sound: \(DispatchQueue.main.sync { session.soundReport })")
                case "cliplog": print("remoteman: clipboard log\n  " + DispatchQueue.main.sync { session.clipboardLog }.joined(separator: "\n  "))
                case "copy":
                    DispatchQueue.main.sync {
                        Sharing.copyForTest(arg)
                        session.lookAtClipboardNow()
                    }
                case "clip": print("remoteman: clipboard here: \(DispatchQueue.main.sync { Sharing.clipboardSummary(expecting: arg.isEmpty ? nil : arg) })")
                case "makefile":
                    let p = arg.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
                    if p.count == 2, let n = Int(p[1]) {
                        let bytes = Data((0..<n).map { UInt8(truncatingIfNeeded: $0 &* 31 &+ 7) })
                        try? bytes.write(to: resolve(p[0]))
                        print("remoteman: made \(p[0]), \(n) bytes")
                    }
                case "leave": DispatchQueue.main.sync { session.disconnect() }
                case "join":
                    DispatchQueue.main.sync { join?() }
                    for _ in 0..<150 {
                        if let again = DispatchQueue.main.sync(execute: { session.client }), session.renderer?.videoSize != nil {
                            c = again
                            break
                        }
                        usleep(100_000)
                    }
                    print("remoteman: joined: \(DispatchQueue.main.sync { "\(session.state)" })")
                case "mem": print("remoteman: memory \(memory()), \(DispatchQueue.main.sync { session.client == nil ? "not connected" : "connected" })")
                case "transfers":
                    print("remoteman: transfers \(DispatchQueue.main.sync { session.transfers })  notice \(DispatchQueue.main.sync { session.notice ?? "none" })")
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

    /// What this app holds now (the figure Activity Monitor shows), and how many threads.
    static func memory() -> String {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let ok = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        guard ok == KERN_SUCCESS else { return "unreadable" }
        var threads: thread_act_array_t?
        var n: mach_msg_type_number_t = 0
        if task_threads(mach_task_self_, &threads, &n) == KERN_SUCCESS, let threads {
            vm_deallocate(mach_task_self_, vm_address_t(UInt(bitPattern: threads)), vm_size_t(Int(n) * MemoryLayout<thread_act_t>.size))
        }
        return String(format: "%.1f MB, %d threads", Double(info.phys_footprint) / 1_048_576, Int(n))
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
