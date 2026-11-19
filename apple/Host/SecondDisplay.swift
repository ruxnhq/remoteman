// A second display for the connected device (docs/PROTOCOL.md, "A second display"): another
// virtual display beside the first, captured and encoded by itself and sent as the session's
// second picture. The pointer is drawn into this picture. Its bitrate is this Mac's own choice.
import AppKit
import CoreMedia
import ScreenCaptureKit

final class HostSecondDisplay: NSObject, SCStreamOutput, SCStreamDelegate {
    private var display: RMVirtualDisplay?
    private var stream: SCStream?
    private var encoder: HostEncoder?
    private var last: CVPixelBuffer?
    private let lock = NSLock()
    private let frames = DispatchQueue(label: "remoteman.host.second", qos: .userInteractive)
    private var generation = 0
    /// One encoded frame, on the encoder's thread: Annex-B, whether it is a keyframe, and when it was captured.
    var send: ((HostEncoder.Output) -> Void)?
    /// Where the display is among this Mac's, in points, and its pixels per point (for input).
    private(set) var geometry: (bounds: CGRect, scale: CGFloat)?
    private(set) var size = (width: 0, height: 0, fps: 0)

    var running: Bool { stream != nil }

    /// Make the display and start its picture. Returns why not, if it couldn't. Main actor.
    @MainActor
    func start(width: Int, height: Int, fps: Int, hiDPI: Bool, tenBit: Bool, side: String = "") async -> String? {
        await stop()
        generation += 1
        let gen = generation
        guard RMVirtualDisplay.isSupported() else { return "this macOS has no virtual displays" }
        var made: RMVirtualDisplay?
        for serial in [UInt32(7792), UInt32.random(in: 10_000...60_000)] {
            made = RMVirtualDisplay(name: "remoteman 2", maxWidth: UInt32(max(width, 3840)), maxHeight: UInt32(max(height, 2880)), serial: serial)
            if let d = made, d.displayID != 0 { break }
            made = nil
        }
        guard let made else { return "couldn't create a second virtual display" }
        guard made.applyWidth(UInt32(width), height: UInt32(height), refreshRate: Double(fps), hiDPI: hiDPI) else {
            return "the second display refused \(width)×\(height) at \(fps) Hz"
        }
        display = made
        let id = made.displayID
        // A new display takes a moment to have its mode and to show up for capture.
        var found: SCDisplay?
        let points = hiDPI ? width / 2 : width
        for attempt in 0..<40 {
            guard gen == generation else { return "replaced" }
            // The mode asked for isn't always the one the display comes up in: set it then.
            if let m = CGDisplayCopyDisplayMode(id), !(m.pixelWidth == width && m.pixelHeight == height && m.width == points), attempt % 3 == 2 {
                let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
                let modes = (CGDisplayCopyAllDisplayModes(id, opts) as? [CGDisplayMode]) ?? []
                if let want = modes.first(where: { $0.pixelWidth == width && $0.pixelHeight == height && $0.width == points }) {
                    CGDisplaySetDisplayMode(id, want, nil)
                }
            }
            if let m = CGDisplayCopyDisplayMode(id), m.pixelWidth == width, m.pixelHeight == height,
                let d = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false).displays.first(where: { $0.displayID == id }),
                d.width > 0
            {
                found = d
                break
            }
            try? await Task.sleep(nanoseconds: 150_000_000)
        }
        guard let d = found else { return "the second display didn't appear for capture" }
        Self.place(id, side: side)
        let enc: HostEncoder
        let bitrate = width * height >= 3840 * 2160 ? 40_000_000 : 25_000_000
        do { enc = try HostEncoder(width: width, height: height, fps: fps, bitrate: bitrate, tenBit: tenBit) } catch {
            return "no hardware encoder for a second picture of \(width)×\(height) (\(error.localizedDescription))"
        }
        enc.onFrame = { [weak self] o in self?.send?(o) }
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.pixelFormat = tenBit ? kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange : kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        config.colorSpaceName = CGColorSpace.sRGB
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        config.showsCursor = true
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.queueDepth = 3
        let s = SCStream(filter: SCContentFilter(display: d, excludingWindows: []), configuration: config, delegate: self)
        do {
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: frames)
            try await s.startCapture()
        } catch {
            return "capturing the second display failed: \(error.localizedDescription)"
        }
        guard gen == generation else {
            try? await s.stopCapture()
            return "replaced"
        }
        let b = CGDisplayBounds(id)
        lock.lock()
        encoder = enc
        last = nil
        lock.unlock()
        geometry = (b, b.width > 0 ? CGFloat(width) / b.width : 1)
        size = (width, height, fps)
        stream = s
        hostLog("host: second display \(id): \(width)×\(height) at \(fps) Hz, \(bitrate / 1000) kbit/s, at \(Int(b.minX)),\(Int(b.minY)) (\(side.isEmpty ? "right" : side) of the others)")
        // A still screen sends nothing: the first picture is made from a screenshot if none came.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [weak self] in
            guard let self, gen == self.generation else { return }
            self.lock.lock()
            let have = self.last != nil
            self.lock.unlock()
            guard !have else { return }
            SCScreenshotManager.captureSampleBuffer(contentFilter: SCContentFilter(display: d, excludingWindows: []), configuration: config) { sb, _ in
                guard let pb = sb?.imageBuffer else { return }
                self.frames.async { self.take(pb, pts: CMClockGetTime(CMClockGetHostTimeClock())) }
            }
        }
        return nil
    }

    /// On the asked side of every other display ("left", "above", "below"; anything else: the
    /// right), level with its neighbour there. The others keep their places.
    private static func place(_ id: CGDirectDisplayID, side: String) {
        var count: UInt32 = 0
        CGGetActiveDisplayList(0, nil, &count)
        var ids = [CGDirectDisplayID](repeating: 0, count: Int(count))
        CGGetActiveDisplayList(count, &ids, &count)
        let others = ids.prefix(Int(count)).filter { $0 != id && CGDisplayMirrorsDisplay($0) != id }.map { CGDisplayBounds($0) }
        guard !others.isEmpty else { return }
        let own = CGDisplayBounds(id)
        let origin: CGPoint
        switch side {
        case "left":
            let n = others.min { $0.minX < $1.minX }!
            origin = CGPoint(x: n.minX - own.width, y: n.minY)
        case "above":
            let n = others.min { $0.minY < $1.minY }!
            origin = CGPoint(x: n.minX, y: n.minY - own.height)
        case "below":
            let n = others.max { $0.maxY < $1.maxY }!
            origin = CGPoint(x: n.minX, y: n.maxY)
        default:
            let n = others.max { $0.maxX < $1.maxX }!
            origin = CGPoint(x: n.maxX, y: n.minY)
        }
        guard Int(own.minX) != Int(origin.x) || Int(own.minY) != Int(origin.y) else { return }
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return }
        CGConfigureDisplayOrigin(config, id, Int32(origin.x), Int32(origin.y))
        if CGCompleteDisplayConfiguration(config, .forSession) != .success { CGCancelDisplayConfiguration(config) }
    }

    @MainActor
    func stop() async {
        generation += 1
        let s = stream
        stream = nil
        lock.lock()
        let enc = encoder
        encoder = nil
        last = nil
        lock.unlock()
        try? await s?.stopCapture()
        enc?.invalidate()
        if display != nil { hostLog("host: second display removed") }
        display = nil
        geometry = nil
    }

    /// The device lost a frame of this picture: the next one is whole (the last one again if the screen is still).
    func keyframe() {
        lock.lock()
        let enc = encoder, pb = last
        lock.unlock()
        enc?.requestKeyframe()
        if let enc, let pb { frames.async { _ = enc.encode(pb, pts: CMClockGetTime(CMClockGetHostTimeClock())) } }
    }

    private func take(_ pb: CVPixelBuffer, pts: CMTime) {
        lock.lock()
        let enc = encoder
        last = pb
        lock.unlock()
        _ = enc?.encode(pb, pts: pts)
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
            let raw = (attachments.first?[.status] as? NSNumber)?.intValue, SCFrameStatus(rawValue: raw) == .complete, let pb = sb.imageBuffer
        else { return }
        take(pb, pts: sb.presentationTimeStamp)
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        hostLog("host: the second display's capture stopped: \(error.localizedDescription)")
    }
}
