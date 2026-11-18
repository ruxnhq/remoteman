// This device's camera as a camera on the computer: H.264, 1280×720 at 30 a second, Annex-B
// with the parameter sets in front of every keyframe, no B-frames, a keyframe every 2 s and
// whenever the computer asks for one (docs/PROTOCOL.md, "Camera").
import AVFoundation
import VideoToolbox

final class CameraSender: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
    private let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "remoteman.camera")
    private var encoder: VTCompressionSession?
    private var encodedSize = (width: 0, height: 0)
    private var wantKeyframe = true
    private var began: UInt64 = 0
    private var rotation: NSObject?  // AVCaptureDevice.RotationCoordinator
    private var connection: AVCaptureConnection?
    private var turnTimer: Timer?
    private(set) var running = false
    /// For tests: how far the picture was last turned to stay upright, in degrees.
    private(set) var turned = 0.0
    /// Frames, bytes and keyframes sent, and the size they were last encoded at.
    private(set) var totals = (frames: 0, bytes: 0, keyframes: 0, width: 0, height: 0)
    /// For tests: keyframes this side forced (the first, a new size, the computer asking), frames
    /// that hold an IDR picture, and how often the computer asked.
    private(set) var forced = 0, idr = 0, asked = 0
    /// One whole frame, on the camera's queue.
    var send: ((UnsafeRawBufferPointer, _ key: Bool, _ timeUs: UInt64) -> Void)?

    static func allowed(_ then: @escaping (Bool) -> Void) {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: then(true)
        case .notDetermined: AVCaptureDevice.requestAccess(for: .video) { ok in DispatchQueue.main.async { then(ok) } }
        default: then(false)
        }
    }

    /// Returns why not, if the camera can't be started. On the main thread.
    func start() -> String? {
        guard !running else { return nil }
        #if os(iOS)
            let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .front) ?? AVCaptureDevice.default(for: .video)
        #else
            let device = AVCaptureDevice.default(for: .video)
        #endif
        guard let device, let input = try? AVCaptureDeviceInput(device: device) else { return "This device has no camera to use." }
        session.beginConfiguration()
        session.inputs.forEach { session.removeInput($0) }
        session.outputs.forEach { session.removeOutput($0) }
        session.sessionPreset = session.canSetSessionPreset(.hd1280x720) ? .hd1280x720 : .high
        guard session.canAddInput(input) else {
            session.commitConfiguration()
            return "The camera couldn't be opened."
        }
        session.addInput(input)
        let output = AVCaptureVideoDataOutput()
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange]
        output.alwaysDiscardsLateVideoFrames = true
        output.setSampleBufferDelegate(self, queue: queue)
        guard session.canAddOutput(output) else {
            session.commitConfiguration()
            return "The camera couldn't be opened."
        }
        session.addOutput(output)
        session.commitConfiguration()
        if (try? device.lockForConfiguration()) != nil {
            // 30 a second where the camera's format allows it.
            let thirty = CMTime(value: 1, timescale: 30)
            if device.activeFormat.videoSupportedFrameRateRanges.contains(where: { $0.minFrameRate <= 30 && $0.maxFrameRate >= 30 }) {
                device.activeVideoMinFrameDuration = thirty
                device.activeVideoMaxFrameDuration = thirty
            }
            device.unlockForConfiguration()
        }
        connection = output.connection(with: .video)
        #if os(iOS)
            // Keep the picture upright however the device is held.
            let coordinator = AVCaptureDevice.RotationCoordinator(device: device, previewLayer: nil)
            rotation = coordinator
            turn()
            turnTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.turn() }
        #endif
        began = DispatchTime.now().uptimeNanoseconds
        totals = (0, 0, 0, 0, 0)
        forced = 0
        idr = 0
        asked = 0
        wantKeyframe = true
        running = true
        queue.async { [session] in session.startRunning() }
        return nil
    }

    #if os(iOS)
        private func turn() {
            guard let c = connection, let r = rotation as? AVCaptureDevice.RotationCoordinator else { return }
            let angle = r.videoRotationAngleForHorizonLevelCapture
            if c.isVideoRotationAngleSupported(angle), c.videoRotationAngle != angle { c.videoRotationAngle = angle }
            turned = Double(c.videoRotationAngle)
        }
    #endif

    func stop() {
        guard running else { return }
        running = false
        turnTimer?.invalidate()
        turnTimer = nil
        queue.async { [self] in
            session.stopRunning()
            if let e = encoder { VTCompressionSessionInvalidate(e) }
            encoder = nil
            encodedSize = (0, 0)
        }
    }

    /// The computer lost a frame: the next one is a keyframe.
    func keyframe() {
        queue.async {
            self.wantKeyframe = true
            self.asked += 1
        }
    }

    // MARK: Camera queue

    func captureOutput(_ output: AVCaptureOutput, didOutput sample: CMSampleBuffer, from connection: AVCaptureConnection) {
        guard running, let image = CMSampleBufferGetImageBuffer(sample) else { return }
        let w = CVPixelBufferGetWidth(image), h = CVPixelBufferGetHeight(image)
        if encoder == nil || encodedSize != (w, h) {
            // A new size (the device was turned): a new encoder, which starts with a keyframe.
            if let e = encoder { VTCompressionSessionInvalidate(e) }
            encoder = makeEncoder(width: w, height: h)
            encodedSize = (w, h)
            wantKeyframe = true
        }
        guard let encoder else { return }
        let options: CFDictionary? = wantKeyframe ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        if wantKeyframe { forced += 1 }
        wantKeyframe = false
        let timeUs = (DispatchTime.now().uptimeNanoseconds - began) / 1000
        VTCompressionSessionEncodeFrame(
            encoder, imageBuffer: image, presentationTimeStamp: CMTime(value: CMTimeValue(timeUs), timescale: 1_000_000), duration: .invalid,
            frameProperties: options, infoFlagsOut: nil
        ) { [weak self] status, _, buffer in
            guard status == noErr, let buffer, let self else { return }
            self.queue.async { self.encoded(buffer, timeUs: timeUs) }
        }
    }

    private func makeEncoder(width: Int, height: Int) -> VTCompressionSession? {
        var s: VTCompressionSession?
        VTCompressionSessionCreate(
            allocator: nil, width: Int32(width), height: Int32(height), codecType: kCMVideoCodecType_H264, encoderSpecification: nil,
            imageBufferAttributes: nil, compressedDataAllocator: nil, outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard let s else { return nil }
        let set: [(CFString, Any)] = [
            (kVTCompressionPropertyKey_RealTime, true),
            (kVTCompressionPropertyKey_AllowFrameReordering, false),
            (kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel),
            (kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, 2),
            // In frames too: by the duration alone the encoder made one about every second.
            (kVTCompressionPropertyKey_MaxKeyFrameInterval, 60),
            (kVTCompressionPropertyKey_ExpectedFrameRate, 30),
            (kVTCompressionPropertyKey_AverageBitRate, 2_500_000),
        ]
        for (key, value) in set { VTSessionSetProperty(s, key: key, value: value as CFTypeRef) }
        VTCompressionSessionPrepareToEncodeFrames(s)
        return s
    }

    /// Length-prefixed NAL units become Annex-B, with the parameter sets in front of a keyframe.
    private func encoded(_ buffer: CMSampleBuffer, timeUs: UInt64) {
        guard running, let block = CMSampleBufferGetDataBuffer(buffer), let format = CMSampleBufferGetFormatDescription(buffer) else { return }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[CFString: Any]]
        let key = !((attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
        let startCode: [UInt8] = [0, 0, 0, 1]
        var out = Data()
        var lengthSize: Int32 = 4
        if key {
            var count = 0
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil, parameterSetCountOut: &count,
                nalUnitHeaderLengthOut: &lengthSize)
            for i in 0..<count {
                var p: UnsafePointer<UInt8>?
                var size = 0
                if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: i, parameterSetPointerOut: &p, parameterSetSizeOut: &size, parameterSetCountOut: nil,
                    nalUnitHeaderLengthOut: nil) == noErr, let p
                {
                    out.append(contentsOf: startCode)
                    out.append(p, count: size)
                }
            }
        } else {
            CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil, parameterSetCountOut: nil,
                nalUnitHeaderLengthOut: &lengthSize)
        }
        let total = CMBlockBufferGetDataLength(block)
        var bytes = [UInt8](repeating: 0, count: total)
        guard CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: total, destination: &bytes) == noErr else { return }
        let n = Int(lengthSize)
        var at = 0
        var sawIDR = false
        while at + n <= total {
            var length = 0
            for i in 0..<n { length = length << 8 | Int(bytes[at + i]) }
            at += n
            guard length > 0, at + length <= total else { break }
            if bytes[at] & 0x1F == 5, !sawIDR {
                sawIDR = true
                idr += 1
            }
            out.append(contentsOf: startCode)
            out.append(contentsOf: bytes[at..<at + length])
            at += length
        }
        totals.frames += 1
        totals.bytes += out.count
        if key { totals.keyframes += 1 }
        totals.width = encodedSize.width
        totals.height = encodedSize.height
        out.withUnsafeBytes { send?($0, key, timeUs) }
    }
}
