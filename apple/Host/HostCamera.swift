// The connected device's camera as a camera on this Mac. The device sends H.264 (Annex-B,
// parameter sets on keyframes); this decodes it and writes the pictures into the sink stream
// of remoteman's camera extension (HostCamera/), which apps then read as "remoteman Camera".
// The extension has to be installed once, with the owner's approval in System Settings.
import CoreImage
import CoreMediaIO
import Foundation
import SystemExtensions
import VideoToolbox

final class HostCamera {
    static let deviceName = "remoteman Camera"
    static let extensionID = "com.xnixy.remoteman.host.camera"

    /// Frames taken, shown, refused by the decoder, and keyframes, since the camera went on.
    private(set) var totals = (frames: 0, shown: 0, undecodable: 0, keyframes: 0)
    private var sink: Sink?
    private var decoder: VTDecompressionSession?
    private var format: CMVideoFormatDescription?
    private var parameterSets: [Data] = []
    private let ci = CIContext()
    /// Asked when a frame can't be decoded: the device then sends a keyframe.
    var wantKeyframe: (() -> Void)?
    private var lastAsk = Date.distantPast

    /// Whether the camera extension is installed and running: the device is there.
    static var installed: Bool { findDevice() != nil }

    /// Returns why not, if the camera can't be fed.
    func start() -> String? {
        guard let device = Self.findDevice() else { return "remoteman's camera isn't installed on this Mac yet." }
        guard let s = Sink(device: device) else { return "remoteman's camera couldn't be opened." }
        sink = s
        totals = (0, 0, 0, 0)
        return nil
    }

    func stop() {
        sink?.stop()
        sink = nil
        if let d = decoder { VTDecompressionSessionInvalidate(d) }
        decoder = nil
        format = nil
        parameterSets = []
    }

    /// One whole frame from the device (the core's thread).
    func frame(_ data: UnsafeRawBufferPointer, key: Bool) {
        guard sink != nil else { return }
        totals.frames += 1
        if key { totals.keyframes += 1 }
        var sets: [Data] = []
        var body = Data()
        for nal in Self.nals(data) {
            guard let first = nal.first else { continue }
            let kind = first & 0x1F
            if kind == 7 || kind == 8 {
                sets.append(Data(nal))
            } else if kind != 9 {  // access unit delimiters are left out
                var length = UInt32(nal.count).bigEndian
                withUnsafeBytes(of: &length) { body.append(contentsOf: $0) }
                body.append(contentsOf: nal)
            }
        }
        if !sets.isEmpty, sets != parameterSets { newFormat(sets) }
        guard let decoder, let format, !body.isEmpty else { return undecodable() }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: nil, memoryBlock: nil, blockLength: body.count, blockAllocator: nil, customBlockSource: nil, offsetToData: 0,
            dataLength: body.count, flags: 0, blockBufferOut: &block) == noErr, let block
        else { return }
        body.withUnsafeBytes { _ = CMBlockBufferReplaceDataBytes(with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: body.count) }
        var sample: CMSampleBuffer?
        var size = body.count
        guard CMSampleBufferCreateReady(
            allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: 1, sampleTimingEntryCount: 0, sampleTimingArray: nil,
            sampleSizeEntryCount: 1, sampleSizeArray: &size, sampleBufferOut: &sample) == noErr, let sample
        else { return }
        // Decoded on this thread, so the pictures stay in order.
        let status = VTDecompressionSessionDecodeFrame(decoder, sampleBuffer: sample, flags: [], infoFlagsOut: nil) { [weak self] status, _, image, _, _ in
            guard let self else { return }
            guard status == noErr, let image else { return self.undecodable() }
            self.totals.shown += 1
            self.sink?.write(CIImage(cvPixelBuffer: image), ci: self.ci)
        }
        if status != noErr { undecodable() }
    }

    private func undecodable() {
        totals.undecodable += 1
        if Date().timeIntervalSince(lastAsk) > 0.25 {
            lastAsk = Date()
            wantKeyframe?()
        }
    }

    private func newFormat(_ sets: [Data]) {
        parameterSets = sets
        if let d = decoder { VTDecompressionSessionInvalidate(d) }
        decoder = nil
        format = nil
        let pointers = sets.map { set in set.withUnsafeBytes { UnsafePointer<UInt8>(OpaquePointer($0.baseAddress!)) } }
        // The pointers must outlive the call: keep the datas alive around it.
        var desc: CMVideoFormatDescription?
        let status = withExtendedLifetime(sets) {
            CMVideoFormatDescriptionCreateFromH264ParameterSets(
                allocator: nil, parameterSetCount: sets.count, parameterSetPointers: pointers, parameterSetSizes: sets.map { $0.count },
                nalUnitHeaderLength: 4, formatDescriptionOut: &desc)
        }
        guard status == noErr, let desc else { return }
        let attrs: [CFString: Any] = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA, kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary]
        var session: VTDecompressionSession?
        guard VTDecompressionSessionCreate(
            allocator: nil, formatDescription: desc, decoderSpecification: nil, imageBufferAttributes: attrs as CFDictionary,
            outputCallback: nil, decompressionSessionOut: &session) == noErr, let session
        else { return }
        VTSessionSetProperty(session, key: kVTDecompressionPropertyKey_RealTime, value: kCFBooleanTrue)
        format = desc
        decoder = session
        let d = CMVideoFormatDescriptionGetDimensions(desc)
        hostLog("host: the device's camera sends \(d.width)×\(d.height)")
    }

    /// The NAL units of an Annex-B frame, without their start codes.
    static func nals(_ data: UnsafeRawBufferPointer) -> [UnsafeRawBufferPointer.SubSequence] {
        var out: [UnsafeRawBufferPointer.SubSequence] = []
        let n = data.count
        var start = -1
        var i = 0
        while i + 2 < n {
            if data[i] == 0, data[i + 1] == 0, data[i + 2] == 1 {
                if start >= 0 {
                    var end = i
                    if end > start, data[end - 1] == 0 { end -= 1 }  // the fourth byte of a long start code
                    if end > start { out.append(data[start..<end]) }
                }
                start = i + 3
                i += 3
            } else {
                i += 1
            }
        }
        if start >= 0, start < n { out.append(data[start..<n]) }
        return out
    }

    // MARK: CoreMediaIO (as espalier's bridge feeds its camera)

    static func findDevice() -> CMIODeviceID? {
        let devices: [CMIODeviceID] = property(CMIOObjectID(kCMIOObjectSystemObject), kCMIOHardwarePropertyDevices) ?? []
        return devices.first { name($0) == deviceName }
    }

    static func name(_ id: CMIOObjectID) -> String? {
        var address = CMIOObjectPropertyAddress(
            mSelector: CMIOObjectPropertySelector(kCMIOObjectPropertyName), mScope: CMIOObjectPropertyScope(kCMIOObjectPropertyScopeGlobal),
            mElement: CMIOObjectPropertyElement(kCMIOObjectPropertyElementMain))
        var name: Unmanaged<CFString>?
        var used: UInt32 = 0
        let status = withUnsafeMutablePointer(to: &name) {
            CMIOObjectGetPropertyData(id, &address, 0, nil, UInt32(MemoryLayout<CFString>.size), &used, $0)
        }
        return status == noErr ? name?.takeRetainedValue() as String? : nil
    }

    static func property<T>(_ id: CMIOObjectID, _ selector: Int) -> [T]? {
        var address = CMIOObjectPropertyAddress(
            mSelector: CMIOObjectPropertySelector(selector), mScope: CMIOObjectPropertyScope(kCMIOObjectPropertyScopeGlobal),
            mElement: CMIOObjectPropertyElement(kCMIOObjectPropertyElementMain))
        var size: UInt32 = 0
        guard CMIOObjectGetPropertyDataSize(id, &address, 0, nil, &size) == noErr, size > 0 else { return nil }
        let count = Int(size) / MemoryLayout<T>.stride
        let p = UnsafeMutablePointer<T>.allocate(capacity: count)
        defer { p.deallocate() }
        var used: UInt32 = 0
        guard CMIOObjectGetPropertyData(id, &address, 0, nil, size, &used, p) == noErr else { return nil }
        return Array(UnsafeBufferPointer(start: p, count: Int(used) / MemoryLayout<T>.stride))
    }

    /// The camera's sink stream: what is enqueued here is what the camera shows.
    final class Sink {
        let device: CMIODeviceID, stream: CMIOStreamID
        private let size = CGSize(width: 1280, height: 720)
        private var queue: Unmanaged<CMSimpleQueue>?
        private var pool: CVPixelBufferPool?

        init?(device: CMIODeviceID) {
            let streams: [CMIOStreamID] = HostCamera.property(device, kCMIODevicePropertyStreams) ?? []
            // Direction 0 is into the device: the sink (1 is what apps read).
            guard
                let sink = streams.first(where: { (HostCamera.property($0, kCMIOStreamPropertyDirection) as [UInt32]?)?.first == 0 })
                    ?? (streams.count > 1 ? streams[1] : nil)
            else { return nil }
            self.device = device
            stream = sink
            var q: Unmanaged<CMSimpleQueue>?
            guard CMIOStreamCopyBufferQueue(stream, { _, _, _ in }, nil, &q) == noErr, let q else { return nil }
            queue = q
            guard CMIODeviceStartStream(device, stream) == noErr else { return nil }
            let attrs: [CFString: Any] = [
                kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey: Int(size.width),
                kCVPixelBufferHeightKey: Int(size.height), kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
            ]
            CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        }

        func stop() {
            CMIODeviceStopStream(device, stream)
            CMIOStreamCopyBufferQueue(stream, nil, nil, &queue)
            queue = nil
        }

        /// Fit the picture inside the camera's (a turned one is centred on black), then enqueue.
        func write(_ image: CIImage, ci: CIContext) {
            guard let q = queue?.takeUnretainedValue(), let pool else { return }
            guard CMSimpleQueueGetCount(q) < CMSimpleQueueGetCapacity(q) else { return }
            var pb: CVPixelBuffer?
            guard CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pb) == kCVReturnSuccess, let pb else { return }
            let e = image.extent
            let scale = min(size.width / e.width, size.height / e.height)
            let fitted = image.transformed(by: CGAffineTransform(translationX: -e.minX, y: -e.minY).scaledBy(x: scale, y: scale))
            let placed = fitted.transformed(
                by: CGAffineTransform(translationX: (size.width - fitted.extent.width) / 2, y: (size.height - fitted.extent.height) / 2))
            let black = CIImage(color: .black).cropped(to: CGRect(origin: .zero, size: size))
            ci.render(placed.composited(over: black), to: pb, bounds: CGRect(origin: .zero, size: size), colorSpace: CGColorSpace(name: CGColorSpace.sRGB))
            var desc: CMVideoFormatDescription?
            CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: pb, formatDescriptionOut: &desc)
            guard let desc else { return }
            var timing = CMSampleTimingInfo(
                duration: CMTime(value: 1, timescale: 30), presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()), decodeTimeStamp: .invalid)
            var sb: CMSampleBuffer?
            guard CMSampleBufferCreateReadyWithImageBuffer(
                allocator: nil, imageBuffer: pb, formatDescription: desc, sampleTiming: &timing, sampleBufferOut: &sb) == noErr, let sb
            else { return }
            if CMSimpleQueueEnqueue(q, element: Unmanaged.passRetained(sb).toOpaque()) != noErr {
                Unmanaged.passUnretained(sb).release()
            }
        }
    }
}

/// Installing or removing the camera extension. macOS asks the owner to allow it in System
/// Settings the first time; nothing is asked until the owner chooses to install.
final class CameraInstaller: NSObject, OSSystemExtensionRequestDelegate {
    static let shared = CameraInstaller()
    /// What the last request came to, for Settings to show.
    var changed: ((String) -> Void)?

    func install() {
        let r = OSSystemExtensionRequest.activationRequest(forExtensionWithIdentifier: HostCamera.extensionID, queue: .main)
        r.delegate = self
        OSSystemExtensionManager.shared.submitRequest(r)
        changed?("Asking macOS to install the camera…")
    }

    func remove() {
        let r = OSSystemExtensionRequest.deactivationRequest(forExtensionWithIdentifier: HostCamera.extensionID, queue: .main)
        r.delegate = self
        OSSystemExtensionManager.shared.submitRequest(r)
        changed?("Asking macOS to remove the camera…")
    }

    func request(
        _ request: OSSystemExtensionRequest, actionForReplacingExtension existing: OSSystemExtensionProperties,
        withExtension ext: OSSystemExtensionProperties
    ) -> OSSystemExtensionRequest.ReplacementAction { .replace }

    func requestNeedsUserApproval(_ request: OSSystemExtensionRequest) {
        hostLog("camera: waiting for approval in System Settings")
        changed?("Allow \"remoteman Camera\" in System Settings > General > Login Items & Extensions > Camera Extensions.")
    }

    func request(_ request: OSSystemExtensionRequest, didFinishWithResult result: OSSystemExtensionRequest.Result) {
        hostLog("camera: request finished (\(result.rawValue))")
        changed?(result == .completed ? "Done." : "macOS will finish this after a restart.")
    }

    func request(_ request: OSSystemExtensionRequest, didFailWithError error: Error) {
        hostLog("camera: request failed: \(error.localizedDescription)")
        changed?("It didn't work: \(error.localizedDescription)")
    }
}
