// The Mac as a remoteman host: for the client that connects, a virtual display of its size,
// scale and refresh rate, captured with ScreenCaptureKit, encoded as HEVC Main10 and handed
// to the core, which owns the connection. Input from the client goes into the desktop, and
// the cursor is reported apart from the picture when the client asks.
// Adapted from espalier's ScreenController.
import AppKit
import CoreMedia
import Foundation
import IOKit.pwr_mgt
import RemotemanCore
import ScreenCaptureKit

/// Where the log goes besides stdout: an app started from Finder or `open` has no terminal.
let hostLogFile: FileHandle? = {
    let url = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Logs/remoteman-host.log")
    if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
    let h = try? FileHandle(forWritingTo: url)
    _ = try? h?.seekToEnd()
    return h
}()
private let hostLogLock = NSLock()
private let hostLogDate: DateFormatter = {
    let f = DateFormatter()
    f.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS"
    return f
}()

func hostLog(_ s: String) {
    hostLogLock.lock()
    defer { hostLogLock.unlock() }
    let line = "\(hostLogDate.string(from: Date())) \(s)\n"
    print(line, terminator: "")
    hostLogFile?.write(Data(line.utf8))
}

final class MacHost: NSObject, SCStreamOutput, SCStreamDelegate {
    struct Options {
        var listen = "0.0.0.0:7791"
        var name = Host.current().localizedName ?? "Mac"
        var stateDir = ""
        var pair = false
        /// Make the virtual display the main one (menu bar and new windows) while a client is on.
        var makeMain = false
        /// What ScreenCaptureKit delivers: "x420" (10-bit YCbCr, what the encoder takes as it is),
        /// "l10r" (10-bit RGB) or "420v" (8-bit YCbCr); the last two cost the encoder a conversion.
        var capture = "x420"
        /// How many frames may be inside the encoder at once.
        var inFlight = 2
        var bitrate = 0
        /// Fill the virtual display with moving test content while a client is on.
        var testWindow = false
        var testDrop: Double?
        var testRevoke: Double?
    }

    /// The device that is connected, as the interface shows it.
    struct Client {
        var name: String
        var width: Int
        var height: Int
        var refresh: Int
        /// 0 an extra display, 1 the main display, 2 the only display.
        var placement: Int
        var since = Date()
    }

    /// For the interface, on the main queue: the device that connected (nil: it left), and
    /// once a second while a picture is going out, the frames and bits sent in that second.
    var onClient: ((Client?) -> Void)?
    var onStats: ((_ fps: Int, _ bitsPerSecond: Int) -> Void)?

    private let options: Options
    private var host: OpaquePointer?

    var isRunning: Bool { host != nil }

    /// "address:port" this host listens on, and its key's fingerprint.
    var info: (address: String, fingerprint: String)? {
        guard let h = host else { return nil }
        var info = [CChar](repeating: 0, count: 256)
        rm_host_info(h, &info, info.count)
        let parts = String(cString: info).split(separator: "\t").map(String.init)
        return (parts.first ?? options.listen, parts.count > 1 ? parts[1] : "")
    }

    /// Pairing mode: the next unknown device is let in and remembered, and the core then turns
    /// the mode off by itself.
    var pairing: Bool {
        get { host.map { rm_host_pairing($0) != 0 } ?? false }
        set { if let h = host { rm_host_set_pairing(h, newValue ? 1 : 0) } }
    }
    /// A device this Mac trusts.
    struct Device: Identifiable, Equatable {
        var fingerprint: String
        var name: String
        var connected: Bool
        var id: String { fingerprint }
    }

    /// The trusted devices, in the order they were paired.
    var devices: [Device] {
        guard let h = host else { return [] }
        var buf = [CChar](repeating: 0, count: 8192)
        var n = rm_host_devices(h, &buf, buf.count)
        if n >= buf.count {
            buf = [CChar](repeating: 0, count: n + 1)
            n = rm_host_devices(h, &buf, buf.count)
        }
        guard let list = (try? JSONSerialization.jsonObject(with: Data(String(cString: buf).utf8))) as? [[String: Any]] else { return [] }
        return list.map {
            Device(fingerprint: $0["fingerprint"] as? String ?? "", name: $0["name"] as? String ?? "", connected: $0["connected"] as? Bool ?? false)
        }
    }

    /// Stop trusting a device (it is dropped if connected). 1 done, 0 it wasn't trusted, -1 the list couldn't be saved.
    func revoke(_ fingerprint: String) -> Int {
        guard let h = host else { return 0 }
        return Int(rm_host_revoke(h, fingerprint))
    }

    /// Drop the connected device and keep listening; it stays trusted.
    @discardableResult
    func dropClient() -> Bool { host.map { rm_host_drop_client($0) != 0 } ?? false }

    private let frames = DispatchQueue(label: "remoteman.host.frames", qos: .userInteractive)
    private let input = HostInput()

    // Main queue.
    private var display: RMVirtualDisplay?
    private var stream: SCStream?
    private var previousMain: CGDirectDisplayID?
    private var mirrored: [CGDirectDisplayID] = []
    private var awake: IOPMAssertionID = 0
    private var statsTimer: Timer?
    private var cursorTimer: Timer?
    private var generation = 0

    // Under `lock`.
    private let lock = NSLock()
    private var encoder: HostEncoder?
    private var lastBuffer: CVPixelBuffer?
    private var geometry: (bounds: CGRect, scale: CGFloat)?
    private var sent = 0, bytes = 0, encodeMs = 0.0, dropped = 0, keyframes = 0

    init(options: Options) {
        self.options = options
        super.init()
        input.display = { [weak self] in
            guard let self else { return nil }
            self.lock.lock()
            defer { self.lock.unlock() }
            return self.geometry
        }
    }

    // MARK: Start and stop

    func start() throws {
        let callbacks = rm_host_callbacks(
            ctx: Unmanaged.passUnretained(self).toOpaque(),
            on_client: { ctx, client, accept in
                let me = Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue()
                return me.clientArrived(client!.pointee, accept!)
            },
            on_keyframe: { ctx in Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().keyframeWanted() },
            on_input: { ctx, input in
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().inputArrived(input!.pointee)
            },
            on_client_left: { ctx in Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().clientLeft() },
            on_message: { ctx, json in
                guard let json, let m = (try? JSONSerialization.jsonObject(with: Data(String(cString: json).utf8))) as? [String: Any]
                else { return }
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().message(m)
            },
            // Microphone, camera and blobs: not acted on yet, so not announced.
            on_mic: nil, on_camera: nil, on_blob: nil)
        var err = [CChar](repeating: 0, count: 512)
        guard
            let h = rm_host_start(
                options.listen, options.stateDir, options.name, "macos", "video,input,cursor,placement,configure", options.pair ? 1 : 0,
                callbacks, &err, err.count)
        else { throw NSError(domain: "remoteman", code: 1, userInfo: [NSLocalizedDescriptionKey: String(cString: err)]) }
        host = h
        var info = [CChar](repeating: 0, count: 256)
        rm_host_info(h, &info, info.count)
        let parts = String(cString: info).split(separator: "\t").map(String.init)
        hostLog("host: listening on \(parts.first ?? options.listen), fingerprint \(parts.count > 1 ? parts[1] : "?"), pairing \(rm_host_pairing(h) != 0 ? "on" : "off")")
    }

    func stop() {
        teardown()
        if let h = host {
            host = nil
            rm_host_stop(h)
        }
    }

    // MARK: Core callbacks (the core's thread; they may block)

    private func clientArrived(_ c: rm_client_info, _ accept: UnsafeMutablePointer<rm_accept>) -> Int32 {
        let name = c.name.map { String(cString: $0) } ?? "?"
        let decoders = (c.decoders.map { String(cString: $0) } ?? "").split(separator: ",").map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        hostLog("host: client \"\(name)\" \(c.width)×\(c.height) scale \(c.scale) at \(c.refresh) Hz, decoders \(decoders.joined(separator: ",")), cursor apart \(c.cursor_apart != 0)")
        func refuse(_ why: String) -> Int32 {
            hostLog("host: refused: \(why)")
            withUnsafeMutableBytes(of: &accept.pointee.refuse) { buf in
                let bytes = Array(why.utf8.prefix(buf.count - 1))
                buf.copyBytes(from: bytes)
                buf[bytes.count] = 0
            }
            return 0
        }
        guard decoders.contains("hevc") else { return refuse("this host sends HEVC 4:2:0 and the client doesn't decode it") }
        guard RMVirtualDisplay.isSupported() else { return refuse("this macOS has no virtual displays") }
        guard CGPreflightScreenCaptureAccess() else {
            return refuse("the Mac host needs Screen Recording permission (System Settings, Privacy & Security)")
        }
        // Where the client wants its display: 0 extra, 1 main, 2 only. --main yes still forces main.
        let p = Picture(
            width: Int(c.width), height: Int(c.height), fps: Int(c.refresh), hiDPI: c.scale >= 1.5, cursorApart: c.cursor_apart != 0,
            placement: max(Int(c.placement), options.makeMain ? 1 : 0))
        limit = 0
        if let failure = begin(p) { return refuse(failure) }
        clientName = name
        picture = p
        fill(accept, p)
        let summary = Client(name: name, width: p.width, height: p.height, refresh: p.fps, placement: min(p.placement, 2))
        DispatchQueue.main.async { self.onClient?(summary) }
        return 1
    }

    /// What a client's display is: its pixels, rate, whether the desktop is drawn at 2×, and where it sits.
    private struct Picture: Equatable {
        var width: Int, height: Int, fps: Int
        /// macOS draws at 1× or 2×: anything from 1.5 up gets the 2× desktop.
        var hiDPI: Bool
        var cursorApart: Bool
        var placement: Int
        init(width: Int, height: Int, fps: Int, hiDPI: Bool, cursorApart: Bool, placement: Int) {
            self.width = min(7680, max(640, width & ~1))
            self.height = min(4320, max(360, height & ~1))
            self.fps = min(240, max(24, fps))
            self.hiDPI = hiDPI
            self.cursorApart = cursorApart
            self.placement = min(max(placement, 0), 2)
        }
    }
    // The core's thread.
    private var picture: Picture?
    private var clientName = ""
    /// The ceiling the client asked for, in bits a second (0: this host chooses).
    private var limit = 0

    private func bitrate(for p: Picture) -> Int {
        let own = options.bitrate > 0 ? options.bitrate : (p.width * p.height >= 3840 * 2160 ? 40_000_000 : 25_000_000)
        return limit > 0 ? max(500_000, min(own, limit)) : own
    }

    private func fill(_ accept: UnsafeMutablePointer<rm_accept>, _ p: Picture) {
        accept.pointee.codec = 0
        accept.pointee.depth = 10
        accept.pointee.chroma = 420
        accept.pointee.width = UInt32(p.width)
        accept.pointee.height = UInt32(p.height)
        accept.pointee.refresh = UInt32(p.fps)
        accept.pointee.hdr = 0
    }

    /// Take down whatever picture there is and start this one. Blocks the core's thread until
    /// it runs; returns why not, if it doesn't.
    private func begin(_ p: Picture) -> String? {
        let done = DispatchSemaphore(value: 0)
        var failure: String? = "starting the display timed out"
        let rate = bitrate(for: p)
        DispatchQueue.main.async { [self] in
            teardown()
            generation += 1
            let gen = generation
            Task { @MainActor in
                failure = await self.setUp(
                    width: p.width, height: p.height, fps: p.fps, hiDPI: p.hiDPI, cursorApart: p.cursorApart, placement: p.placement,
                    bitrate: rate, gen: gen)
                done.signal()
            }
        }
        if done.wait(timeout: .now() + 15) == .timedOut { failure = "starting the display timed out" }
        if failure != nil { DispatchQueue.main.async { self.teardown() } }
        return failure
    }

    /// One of the client's feature messages. So far: configure.
    private func message(_ m: [String: Any]) {
        guard m["t"] as? String == "configure", let host, let old = picture else { return }
        if let b = (m["bitrate"] as? NSNumber)?.intValue {
            limit = max(0, b)
            let rate = bitrate(for: old)
            lock.lock()
            let enc = encoder
            lock.unlock()
            enc?.setBitrate(rate)
            hostLog("host: configure: bitrate ceiling \(limit), encoding at \(rate / 1000) kbit/s")
            rm_host_send_message(host, "{\"t\":\"status\",\"bitrate\":\(rate)}")
        }
        guard let d = m["display"] as? [String: Any] else { return }
        func int(_ k: String, _ now: Int) -> Int { (d[k] as? NSNumber)?.intValue ?? now }
        var placement = old.placement
        if let s = d["placement"] as? String { placement = ["extend": 0, "main": 1, "only": 2][s] ?? placement }
        let new = Picture(
            width: int("width", old.width), height: int("height", old.height), fps: int("refresh", old.fps),
            hiDPI: (d["scale"] as? NSNumber).map { $0.doubleValue >= 1.5 } ?? old.hiDPI, cursorApart: old.cursorApart,
            placement: max(placement, options.makeMain ? 1 : 0))
        guard new != old else { return }
        hostLog("host: configure: display \(new.width)×\(new.height) at \(new.fps) Hz, 2× \(new.hiDPI), placement \(new.placement)")
        var now = new
        if let failure = begin(new) {
            // Back to what worked; if that fails too, the client is dropped.
            hostLog("host: configure failed (\(failure)); going back to \(old.width)×\(old.height)")
            now = old
            if let again = begin(old) {
                hostLog("host: and the old display didn't come back (\(again)); dropping the client")
                rm_host_drop_client(host)
                return
            }
        }
        picture = now
        var accept = rm_accept()
        fill(&accept, now)
        if rm_host_restart(host, &accept) == 0 { hostLog("host: configure: the core didn't restart the stream") }
        rm_host_send_message(host, "{\"t\":\"status\",\"bitrate\":\(bitrate(for: now))}")
        let summary = Client(name: clientName, width: now.width, height: now.height, refresh: now.fps, placement: now.placement)
        DispatchQueue.main.async { self.onClient?(summary) }
    }

    private func keyframeWanted() {
        lock.lock()
        let enc = encoder, last = lastBuffer
        keyframes += 1
        lock.unlock()
        enc?.requestKeyframe()
        // A still screen sends no frames; encode the last one so the keyframe goes out.
        if let enc, let last { frames.async { enc.encode(last, pts: CMClockGetTime(CMClockGetHostTimeClock())) } }
    }

    private func inputArrived(_ i: rm_input) {
        switch i.kind {
        case 1: input.handle(.pointerTo(i.x, i.y))
        case 2: input.handle(.pointerBy(i.x, i.y))
        case 3: input.handle(.button(i.button, i.down != 0))
        case 4: input.handle(.scroll(i.x, i.y))
        case 5: input.handle(.key(i.usage, i.down != 0))
        case 6: input.handle(.touch(phase: i.phase, slot: i.slot, i.x, i.y))
        case 7: if let t = i.text { input.handle(.text(String(cString: t))) }
        default: break
        }
    }

    private func clientLeft() {
        hostLog("host: client left")
        picture = nil
        DispatchQueue.main.async {
            self.teardown()
            self.onClient?(nil)
        }
    }

    // MARK: Display and capture (main queue)

    /// Returns nil when the picture is running, or why it isn't.
    @MainActor
    private func setUp(
        width: Int, height: Int, fps: Int, hiDPI: Bool, cursorApart: Bool, placement: Int, bitrate: Int, gen: Int
    ) async -> String? {
        holdAwake()
        // A fixed serial first; one left behind by a crashed run can't be shared, so then a new one.
        for serial in [UInt32(7791), UInt32.random(in: 10_000...60_000)] {
            display = RMVirtualDisplay(
                name: "remoteman", maxWidth: UInt32(max(width, 3840)), maxHeight: UInt32(max(height, 2880)), serial: serial)
            if let d = display, d.displayID != 0 { break }
            display = nil
        }
        guard let display else { return "couldn't create a virtual display" }
        guard display.applyWidth(UInt32(width), height: UInt32(height), refreshRate: Double(fps), hiDPI: hiDPI) else {
            return "the virtual display refused \(width)×\(height) at \(fps) Hz"
        }
        let id = display.displayID
        await settle(id, width: width, height: height, hiDPI: hiDPI)
        if let m = CGDisplayCopyDisplayMode(id) {
            hostLog("host: display \(id): \(m.pixelWidth)×\(m.pixelHeight) px, \(m.width)×\(m.height) pt, \(m.refreshRate) Hz")
        }
        if placement >= 1, CGMainDisplayID() != id {
            previousMain = CGMainDisplayID()
            MacHost.makeMain(id)
        }
        if placement == 2 { mirrored = MacHost.mirrorOthers(onto: id) }
        hostLog("host: placement \(["an extra display", "the main display", "the only display"][min(placement, 2)])")
        // A new display takes a moment to show up in ScreenCaptureKit.
        var found: SCDisplay?
        for _ in 0..<30 {
            guard gen == generation else { return "replaced by a newer client" }
            HostInput.wake()
            if let d = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
                .displays.first(where: { $0.displayID == id }), d.width > 0
            {
                found = d
                break
            }
            try? await Task.sleep(nanoseconds: 150_000_000)
        }
        guard let d = found else { return "the virtual display didn't appear for capture" }
        guard gen == generation else { return "replaced by a newer client" }

        let enc: HostEncoder
        do { enc = try HostEncoder(width: width, height: height, fps: fps, bitrate: bitrate) } catch {
            return "no HEVC Main10 hardware encoder for \(width)×\(height) (\(error.localizedDescription))"
        }
        enc.maxInFlight = max(1, options.inFlight)
        enc.onFrame = { [weak self] o in self?.send(o) }

        let config = captureConfiguration(width: width, height: height, cursorApart: cursorApart)
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.queueDepth = 3
        let s = SCStream(filter: SCContentFilter(display: d, excludingWindows: []), configuration: config, delegate: self)
        do {
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: frames)
            try await s.startCapture()
        } catch {
            return "screen capture failed: \(error.localizedDescription)"
        }
        guard gen == generation else {
            try? await s.stopCapture()
            return "replaced by a newer client"
        }
        let b = CGDisplayBounds(id)
        lock.lock()
        encoder = enc
        lastBuffer = nil
        geometry = (b, b.width > 0 ? CGFloat(width) / b.width : 1)
        sent = 0; bytes = 0; encodeMs = 0; dropped = 0
        lock.unlock()
        stream = s
        statsTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.stats() }
        if cursorApart { startCursor() }
        if options.testWindow { testWindow.show(on: id) }
        firstFrame(d, width: width, height: height, cursorApart: cursorApart, gen: gen)
        hostLog("host: sending \(width)×\(height) at \(fps) Hz, HEVC Main10 4:2:0, \(bitrate / 1000) kbit/s, capture \(options.capture), cursor \(cursorApart ? "apart" : "in the picture")")
        return nil
    }

    private func captureConfiguration(width: Int, height: Int, cursorApart: Bool) -> SCStreamConfiguration {
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.pixelFormat =
            switch options.capture {
            case "420v": kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            case "l10r": kCVPixelFormatType_ARGB2101010LEPacked
            default: kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
            }
        config.colorSpaceName = CGColorSpace.sRGB
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        config.showsCursor = !cursorApart
        return config
    }

    /// ScreenCaptureKit sends nothing while the screen doesn't change, so without a frame soon
    /// after the start, a screenshot is encoded instead: the client always gets a picture.
    private func firstFrame(_ d: SCDisplay, width: Int, height: Int, cursorApart: Bool, gen: Int) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [self] in
            lock.lock()
            let enc = encoder, have = lastBuffer != nil
            lock.unlock()
            guard gen == generation, let enc, !have, stream != nil else { return }
            let config = captureConfiguration(width: width, height: height, cursorApart: cursorApart)
            SCScreenshotManager.captureSampleBuffer(
                contentFilter: SCContentFilter(display: d, excludingWindows: []), configuration: config
            ) { [weak self] sb, error in
                guard let self, let pb = sb?.imageBuffer else {
                    return hostLog("host: no frame yet, and no screenshot either: \(error?.localizedDescription ?? "empty")")
                }
                self.lock.lock()
                let current = self.encoder === enc && self.lastBuffer == nil
                if current { self.lastBuffer = pb }
                self.lock.unlock()
                guard current else { return }
                hostLog("host: no frame yet; sent a screenshot")
                enc.requestKeyframe()
                self.frames.async { _ = enc.encode(pb, pts: CMClockGetTime(CMClockGetHostTimeClock())) }
            }
        }
    }

    private let testWindow = TestWindow()

    private func teardown() {
        generation += 1
        testWindow.close()
        statsTimer?.invalidate()
        statsTimer = nil
        cursorTimer?.invalidate()
        cursorTimer = nil
        input.release()
        lock.lock()
        let enc = encoder
        encoder = nil
        lastBuffer = nil
        geometry = nil
        lock.unlock()
        enc?.invalidate()
        if let s = stream {
            stream = nil
            Task { try? await s.stopCapture() }
        }
        if !mirrored.isEmpty {
            MacHost.unmirror(mirrored)
            mirrored = []
        }
        if let prev = previousMain, prev != CGMainDisplayID() {
            var ids = [CGDirectDisplayID](repeating: 0, count: 16)
            var n: UInt32 = 0
            CGGetOnlineDisplayList(16, &ids, &n)
            if ids.prefix(Int(n)).contains(prev) { MacHost.makeMain(prev) }
        }
        previousMain = nil
        if display != nil {
            display = nil  // the display goes with the object
            hostLog("host: display removed")
        }
        if awake != 0 {
            IOPMAssertionRelease(awake)
            awake = 0
        }
    }

    private func holdAwake() {
        if awake == 0 {
            IOPMAssertionCreateWithName(
                kIOPMAssertionTypePreventUserIdleDisplaySleep as CFString, IOPMAssertionLevel(kIOPMAssertionLevelOn),
                "remoteman host session" as CFString, &awake)
        }
        HostInput.wake()
    }

    /// Wait until the display has the mode last applied. Given a mode, macOS often settles on
    /// its 1× twin even when HiDPI was asked for, so the 2× mode is then picked by hand.
    @MainActor
    private func settle(_ id: CGDirectDisplayID, width: Int, height: Int, hiDPI: Bool) async {
        let points = hiDPI ? width / 2 : width
        func right() -> Bool {
            guard let m = CGDisplayCopyDisplayMode(id) else { return false }
            return m.pixelWidth == width && m.pixelHeight == height && m.width == points
        }
        for attempt in 0..<30 {
            if right() { return }
            if attempt >= 3, attempt % 3 == 0 {
                let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
                let modes = (CGDisplayCopyAllDisplayModes(id, opts) as? [CGDisplayMode]) ?? []
                if let m = modes.first(where: { $0.pixelWidth == width && $0.pixelHeight == height && $0.width == points }) {
                    CGDisplaySetDisplayMode(id, m, nil)
                }
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        hostLog("host: display \(id): asked for \(width)×\(height)\(hiDPI ? " HiDPI" : ""), still in another mode")
    }

    /// "Only display": macOS can't switch a screen off for a session, so every other display is
    /// made a mirror of the client's instead; there is then one desktop, and it is the client's.
    /// Returns the displays that were changed, to be put back when the client leaves.
    static func mirrorOthers(onto id: CGDirectDisplayID) -> [CGDirectDisplayID] {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        let others = ids.prefix(Int(n)).filter { $0 != id && CGDisplayMirrorsDisplay($0) == kCGNullDirectDisplay }
        guard !others.isEmpty else { return [] }
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return [] }
        for o in others { CGConfigureDisplayMirrorOfDisplay(config, o, id) }
        guard CGCompleteDisplayConfiguration(config, .forSession) == .success else { return [] }
        hostLog("host: displays \(others) now mirror display \(id)")
        return Array(others)
    }

    static func unmirror(_ displays: [CGDirectDisplayID]) {
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return }
        for o in displays { CGConfigureDisplayMirrorOfDisplay(config, o, kCGNullDirectDisplay) }
        CGCompleteDisplayConfiguration(config, .forSession)
        hostLog("host: displays \(displays) have their own picture again")
    }

    /// Main display = the one at the origin; the others keep their places relative to it.
    static func makeMain(_ id: CGDirectDisplayID) {
        guard CGMainDisplayID() != id else { return }
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return }
        let b = CGDisplayBounds(id)
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        for other in ids.prefix(Int(n)) {
            let o = CGDisplayBounds(other)
            CGConfigureDisplayOrigin(config, other, Int32(o.minX - b.minX), Int32(o.minY - b.minY))
        }
        CGCompleteDisplayConfiguration(config, .forSession)
        hostLog("host: display \(id) is now the main display")
    }

    // MARK: Frames (the frames queue, then VideoToolbox's thread)

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
            let raw = (attachments.first?[.status] as? NSNumber)?.intValue, SCFrameStatus(rawValue: raw) == .complete,
            let pb = sb.imageBuffer
        else { return }
        lock.lock()
        let enc = encoder
        let first = lastBuffer == nil
        lastBuffer = pb
        lock.unlock()
        if first {
            let f = CVPixelBufferGetPixelFormatType(pb)
            let cc = String(bytes: [24, 16, 8, 0].map { UInt8((f >> $0) & 0xff) }, encoding: .ascii) ?? "?"
            hostLog("host: capture delivers \(CVPixelBufferGetWidth(pb))×\(CVPixelBufferGetHeight(pb)) '\(cc)'")
        }
        // The timestamp is the capture time on the host clock; a busy encoder drops the frame.
        if let enc, !enc.encode(pb, pts: sb.presentationTimeStamp) {
            lock.lock()
            dropped += 1
            lock.unlock()
        }
    }

    private func send(_ o: HostEncoder.Output) {
        guard let host else { return }
        // The frame's age on this Mac's clock, taken off the session clock the client compares with.
        let age = CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()) - o.pts)
        let now = rm_host_now_us(host)
        guard now != 0 else { return }
        let capture = now &- UInt64(max(0, age) * 1_000_000)
        let taken = o.annexB.withUnsafeBytes {
            rm_host_send_frame(host, $0.bindMemory(to: UInt8.self).baseAddress, o.annexB.count, o.keyframe ? 1 : 0, capture)
        }
        guard taken != 0 else { return }
        lock.lock()
        sent += 1
        bytes += o.annexB.count
        encodeMs += o.encodeMs
        lock.unlock()
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        hostLog("host: capture stopped: \(error.localizedDescription)")
    }

    private func stats() {
        lock.lock()
        let line = sent > 0 || dropped > 0
            ? String(format: "host: %d fps, %d kbit/s, encode %.2f ms, %d dropped (busy), %d keyframe requests", sent, bytes * 8 / 1000, encodeMs / Double(max(sent, 1)), dropped, keyframes)
            : nil
        let fps = sent, bits = bytes * 8
        sent = 0; bytes = 0; encodeMs = 0; dropped = 0
        lock.unlock()
        if let line { hostLog(line) }
        onStats?(fps, bits)
    }

    // MARK: Cursor (main queue)

    private var cursorAt: (Float, Float, Bool)?
    private var cursorPixels: [UInt8]?
    private var cursorTick = 0

    private func startCursor() {
        cursorAt = nil
        cursorPixels = nil
        cursorTick = 0
        let t = Timer(timeInterval: 1.0 / 120, repeats: true) { [weak self] _ in self?.cursorPoll() }
        RunLoop.main.add(t, forMode: .common)
        cursorTimer = t
    }

    private func cursorPoll() {
        guard let host else { return }
        lock.lock()
        let g = geometry
        lock.unlock()
        guard let (b, scale) = g, let p = CGEvent(source: nil)?.location else { return }
        let visible = b.contains(p)
        let at = (Float((p.x - b.minX) / b.width), Float((p.y - b.minY) / b.height), visible)
        if cursorAt == nil || cursorAt! != at {
            cursorAt = at
            rm_host_cursor_at(host, at.0, at.1, visible ? 1 : 0)
        }
        // The shape changes far less often than the position: look at it 20 times a second.
        cursorTick += 1
        guard cursorTick % 6 == 1, let c = NSCursor.currentSystem,
            let cg = c.image.cgImage(forProposedRect: nil, context: nil, hints: nil)
        else { return }
        let w = max(1, Int((c.image.size.width * scale).rounded())), h = max(1, Int((c.image.size.height * scale).rounded()))
        var pixels = [UInt8](repeating: 0, count: w * h * 4)
        let drawn = pixels.withUnsafeMutableBytes { buf -> Bool in
            guard
                let ctx = CGContext(
                    data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.interpolationQuality = .high
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drawn, cursorPixels == nil || cursorPixels! != pixels else { return }
        cursorPixels = pixels
        rm_host_cursor_shape(
            host, pixels, UInt32(w), UInt32(h), Int32((c.hotSpot.x * scale).rounded()), Int32((c.hotSpot.y * scale).rounded()))
    }
}
