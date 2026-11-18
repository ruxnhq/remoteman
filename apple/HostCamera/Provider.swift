// remoteman's camera for the Mac host: a CoreMediaIO camera extension with one device,
// "remoteman Camera". Rebuilt from espalier's camera extension under remoteman's own identifier.

import CoreMediaIO
import CoreVideo
import Foundation
import IOKit.audio
import os.log

let width: Int32 = 1280, height: Int32 = 720, fps: Int32 = 30
let logger = Logger(subsystem: "com.xnixy.remoteman.host.camera", category: "extension")
/// Whether an app is reading the camera ("1"/"0"): the host app reads it as the CMIO device
/// property 'rmus', so it knows when to ask the PC for pictures without touching the streams.
let inUseProperty = CMIOExtensionProperty(rawValue: "4cc_rmus_glob_0000")

// MARK: Device

final class DeviceSource: NSObject, CMIOExtensionDeviceSource {
    private(set) var device: CMIOExtensionDevice!
    private var source: StreamSource!
    private var sink: SinkSource!
    private let format: CMFormatDescription
    private var pool: CVPixelBufferPool!
    private let queue = DispatchQueue(label: "remoteman.camera.frames", qos: .userInteractive)
    private var timer: DispatchSourceTimer?
    private var sourceClients = 0
    private var sinkClient: CMIOExtensionClient?
    private var lastSinkFrame = Date.distantPast
    private var placeholder: CVPixelBuffer?

    init(localizedName: String) {
        var desc: CMFormatDescription?
        CMVideoFormatDescriptionCreate(allocator: nil, codecType: kCVPixelFormatType_32BGRA, width: width, height: height,
                                       extensions: nil, formatDescriptionOut: &desc)
        format = desc!
        super.init()
        device = CMIOExtensionDevice(localizedName: localizedName, deviceID: UUID(uuidString: "7B91C0DE-2A4F-4E6B-9C3D-000000007791")!,
                                     legacyDeviceID: nil, source: self)
        let attrs: [CFString: Any] = [kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey: width,
                                      kCVPixelBufferHeightKey: height, kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary]
        CVPixelBufferPoolCreate(nil, nil, attrs as CFDictionary, &pool)
        placeholder = makePlaceholder()

        let streamFormat = CMIOExtensionStreamFormat(formatDescription: format, maxFrameDuration: CMTime(value: 1, timescale: fps),
                                                     minFrameDuration: CMTime(value: 1, timescale: fps), validFrameDurations: nil)
        source = StreamSource(localizedName: "remoteman Camera", streamID: UUID(), format: streamFormat, device: self)
        sink = SinkSource(localizedName: "remoteman Sink", streamID: UUID(), format: streamFormat, device: self)
        do {
            try device.addStream(source.stream)
            try device.addStream(sink.stream)
        } catch {
            logger.error("adding streams: \(error.localizedDescription, privacy: .public)")
        }
    }

    var availableProperties: Set<CMIOExtensionProperty> { [.deviceTransportType, .deviceModel, inUseProperty] }

    func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionDeviceProperties {
        let p = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) { p.transportType = kIOAudioDeviceTransportTypeVirtual }
        if properties.contains(.deviceModel) { p.model = "remoteman Camera" }
        if properties.contains(inUseProperty) {
            p.setPropertyState(CMIOExtensionPropertyState<AnyObject>(value: inUse as NSString), forProperty: inUseProperty)
        }
        return p
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {}

    private func makePlaceholder() -> CVPixelBuffer? {
        var pb: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pb)
        guard let pb else { return nil }
        CVPixelBufferLockBaseAddress(pb, [])
        if let base = CVPixelBufferGetBaseAddress(pb) {
            let bytes = CVPixelBufferGetBytesPerRow(pb) * Int(height)
            let p = base.assumingMemoryBound(to: UInt8.self)
            // BGRA dark grey.
            for i in stride(from: 0, to: bytes, by: 4) { p[i] = 34; p[i + 1] = 31; p[i + 2] = 31; p[i + 3] = 255 }
        }
        CVPixelBufferUnlockBaseAddress(pb, [])
        return pb
    }

    private var inUse: String { queue.sync { sourceClients > 0 ? "1" : "0" } }

    private func announceInUse() {
        let state = CMIOExtensionPropertyState<AnyObject>(value: (sourceClients > 0 ? "1" : "0") as NSString)
        device.notifyPropertiesChanged([inUseProperty: state])
    }

    // An app started or stopped reading the camera.
    func sourceStarted() {
        queue.async { [self] in
            sourceClients += 1
            announceInUse()
            guard timer == nil else { return }
            // The placeholder when the host app sends nothing, at the stream's rate.
            let t = DispatchSource.makeTimerSource(queue: queue)
            t.schedule(deadline: .now(), repeating: 1.0 / Double(fps))
            t.setEventHandler { [weak self] in
                guard let self, Date().timeIntervalSince(self.lastSinkFrame) > 1, let pb = self.placeholder else { return }
                self.emit(pb)
            }
            t.resume()
            timer = t
        }
    }

    func sourceStopped() {
        queue.async { [self] in
            sourceClients = max(0, sourceClients - 1)
            announceInUse()
            if sourceClients == 0 { timer?.cancel(); timer = nil }
        }
    }

    private func emit(_ pb: CVPixelBuffer) {
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: fps),
                                        presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()), decodeTimeStamp: .invalid)
        var desc: CMFormatDescription?
        CMVideoFormatDescriptionCreateForImageBuffer(allocator: nil, imageBuffer: pb, formatDescriptionOut: &desc)
        var sb: CMSampleBuffer?
        guard let desc, CMSampleBufferCreateReadyWithImageBuffer(allocator: nil, imageBuffer: pb, formatDescription: desc,
                                                                 sampleTiming: &timing, sampleBufferOut: &sb) == noErr, let sb else { return }
        forward(sb)
    }

    private func forward(_ sb: CMSampleBuffer) {
        guard sourceClients > 0 else { return }
        let ns = UInt64(max(0, CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock())) * 1_000_000_000))
        source.stream.send(sb, discontinuity: [], hostTimeInNanoseconds: ns)
    }

    // The helper started or stopped writing.
    func sinkStarted(_ client: CMIOExtensionClient) {
        queue.async { [self] in
            sinkClient = client
            consume(client)
        }
    }

    func sinkStopped() {
        queue.async { [self] in sinkClient = nil }
    }

    /// Take the host app's frames one by one while it's connected.
    private func consume(_ client: CMIOExtensionClient) {
        guard sinkClient === client else { return }
        sink.stream.consumeSampleBuffer(from: client) { [weak self] sb, sequence, _, hasMore, error in
            guard let self else { return }
            self.queue.async {
                if let sb {
                    self.lastSinkFrame = Date()
                    self.forward(sb)
                    let output = CMIOExtensionScheduledOutput(sequenceNumber: sequence,
                                                              hostTimeInNanoseconds: UInt64(max(0, CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock())) * 1_000_000_000)))
                    self.sink.stream.notifyScheduledOutputChanged(output)
                }
                if error != nil || sb == nil {
                    // Nothing queued yet: ask again shortly.
                    self.queue.asyncAfter(deadline: .now() + 0.005) { self.consume(client) }
                } else {
                    self.consume(client)
                }
            }
        }
    }
}

// MARK: Streams

final class StreamSource: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private let format: CMIOExtensionStreamFormat
    private unowned let device: DeviceSource

    init(localizedName: String, streamID: UUID, format: CMIOExtensionStreamFormat, device: DeviceSource) {
        self.format = format
        self.device = device
        super.init()
        stream = CMIOExtensionStream(localizedName: localizedName, streamID: streamID, direction: .source, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [format] }
    var availableProperties: Set<CMIOExtensionProperty> { [.streamActiveFormatIndex, .streamFrameDuration] }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let p = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) { p.activeFormatIndex = 0 }
        if properties.contains(.streamFrameDuration) { p.frameDuration = CMTime(value: 1, timescale: fps) }
        return p
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}
    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool { true }
    func startStream() throws { device.sourceStarted() }
    func stopStream() throws { device.sourceStopped() }
}

final class SinkSource: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private let format: CMIOExtensionStreamFormat
    private unowned let device: DeviceSource
    private var client: CMIOExtensionClient?

    init(localizedName: String, streamID: UUID, format: CMIOExtensionStreamFormat, device: DeviceSource) {
        self.format = format
        self.device = device
        super.init()
        stream = CMIOExtensionStream(localizedName: localizedName, streamID: streamID, direction: .sink, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [format] }
    var availableProperties: Set<CMIOExtensionProperty> {
        [.streamActiveFormatIndex, .streamFrameDuration, .streamSinkBufferQueueSize, .streamSinkBuffersRequiredForStartup,
         .streamSinkBufferUnderrunCount, .streamSinkEndOfData]
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionStreamProperties {
        let p = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) { p.activeFormatIndex = 0 }
        if properties.contains(.streamFrameDuration) { p.frameDuration = CMTime(value: 1, timescale: fps) }
        if properties.contains(.streamSinkBufferQueueSize) { p.sinkBufferQueueSize = 1 }
        if properties.contains(.streamSinkBuffersRequiredForStartup) { p.sinkBuffersRequiredForStartup = 1 }
        return p
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        self.client = client
        return true
    }

    func startStream() throws {
        guard let client else { return }
        device.sinkStarted(client)
    }

    func stopStream() throws { device.sinkStopped() }
}

// MARK: Provider

final class ProviderSource: NSObject, CMIOExtensionProviderSource {
    private(set) var provider: CMIOExtensionProvider!
    private var deviceSource: DeviceSource!

    init(clientQueue: DispatchQueue?) {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: clientQueue)
        deviceSource = DeviceSource(localizedName: "remoteman Camera")
        do { try provider.addDevice(deviceSource.device) } catch {
            logger.error("adding the device: \(error.localizedDescription, privacy: .public)")
        }
    }

    func connect(to client: CMIOExtensionClient) throws {}
    func disconnect(from client: CMIOExtensionClient) {}
    var availableProperties: Set<CMIOExtensionProperty> { [.providerManufacturer] }

    func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws -> CMIOExtensionProviderProperties {
        let p = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) { p.manufacturer = "remoteman" }
        return p
    }

    func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}
}
