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
        /// What ScreenCaptureKit delivers: "l10r" (10-bit RGB) or "420v" (8-bit YCbCr).
        var capture = "l10r"
        var bitrate = 0
        /// Fill the virtual display with moving test content while a client is on.
        var testWindow = false
    }

    private let options: Options
    private var host: OpaquePointer?
    private let frames = DispatchQueue(label: "remoteman.host.frames", qos: .userInteractive)
    private let input = HostInput()

    // Main queue.
    private var display: RMVirtualDisplay?
    private var stream: SCStream?
    private var previousMain: CGDirectDisplayID?
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
            on_client_left: { ctx in Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().clientLeft() })
        var err = [CChar](repeating: 0, count: 512)
        guard
            let h = rm_host_start(
                options.listen, options.stateDir, options.name, "macos", "video,input,cursor", options.pair ? 1 : 0,
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
        let width = min(7680, max(640, Int(c.width) & ~1))
        let height = min(4320, max(360, Int(c.height) & ~1))
        let fps = min(240, max(24, Int(c.refresh)))
        // macOS draws at 1× or 2×: anything from 1.5 up gets the 2× desktop.
        let hiDPI = c.scale >= 1.5
        let cursorApart = c.cursor_apart != 0
        let done = DispatchSemaphore(value: 0)
        var failure: String? = "starting the display timed out"
        DispatchQueue.main.async { [self] in
            teardown()
            generation += 1
            let gen = generation
            Task { @MainActor in
                failure = await self.setUp(width: width, height: height, fps: fps, hiDPI: hiDPI, cursorApart: cursorApart, gen: gen)
                done.signal()
            }
        }
        if done.wait(timeout: .now() + 15) == .timedOut {
            DispatchQueue.main.async { self.teardown() }
            return refuse("starting the display timed out")
        }
        if let failure {
            DispatchQueue.main.async { self.teardown() }
            return refuse(failure)
        }
        accept.pointee.codec = 0
        accept.pointee.depth = 10
        accept.pointee.chroma = 420
        accept.pointee.width = UInt32(width)
        accept.pointee.height = UInt32(height)
        accept.pointee.refresh = UInt32(fps)
        accept.pointee.hdr = 0
        return 1
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
        DispatchQueue.main.async { self.teardown() }
    }

    // MARK: Display and capture (main queue)

    /// Returns nil when the picture is running, or why it isn't.
    @MainActor
    private func setUp(width: Int, height: Int, fps: Int, hiDPI: Bool, cursorApart: Bool, gen: Int) async -> String? {
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
        if options.makeMain, CGMainDisplayID() != id {
            previousMain = CGMainDisplayID()
            MacHost.makeMain(id)
        }
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

        let bitrate = options.bitrate > 0 ? options.bitrate : (width * height >= 3840 * 2160 ? 40_000_000 : 25_000_000)
        let enc: HostEncoder
        do { enc = try HostEncoder(width: width, height: height, fps: fps, bitrate: bitrate) } catch {
            return "no HEVC Main10 hardware encoder for \(width)×\(height) (\(error.localizedDescription))"
        }
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
            options.capture == "420v"
            ? kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange : kCVPixelFormatType_ARGB2101010LEPacked
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
        lastBuffer = pb
        lock.unlock()
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
        sent = 0; bytes = 0; encodeMs = 0; dropped = 0
        lock.unlock()
        if let line { hostLog(line) }
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
