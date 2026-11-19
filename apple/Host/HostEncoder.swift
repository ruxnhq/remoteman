// The Mac host's encoder: HEVC Main10 in hardware, no reordering, no frame delay, no periodic
// keyframes, and never a queue. A frame offered while two are still being encoded or sent is
// dropped, which is how a slow network (or a slow encoder) holds the picture back. Each frame comes out
// in Annex-B, with the parameter sets in front of every keyframe.
// Adapted from espalier's ScreenEncoder.
import CoreMedia
import Foundation
import QuartzCore
import VideoToolbox

final class HostEncoder {
    struct Output {
        let annexB: Data
        let keyframe: Bool
        /// The capture time, on the host time clock.
        let pts: CMTime
        let encodeMs: Double
    }

    /// For measuring only (`--encoder`): "plain" (as shipped), "realtime", or "lowlatency".
    static var testMode = "plain"

    let width: Int, height: Int
    private var session: VTCompressionSession?
    private let lock = NSLock()
    /// Frames inside the encoder or being sent. The hardware works on more than one at a time
    /// (a 5.7 MP frame takes 13.5 ms alone, but two overlapping come out 9 ms apart), so a
    /// second may go in while the first is still inside; a third is dropped.
    private var inFlight = 0
    var maxInFlight = 2
    private var forceKey = true
    /// The newest frame that found the encoder busy. It is encoded as soon as a frame comes out:
    /// ScreenCaptureKit sends nothing while the screen is still, so if the last frame of a burst
    /// were simply dropped, the device would keep an old picture until something else changed.
    private var waiting: (CVPixelBuffer, CMTime)?
    /// Where `encode` is called from (the capture's queue); the waiting frame goes in from there too.
    var queue: DispatchQueue?
    /// For measuring only: off drops a busy frame for good, as before.
    static var keepsLast = true
    /// Counted for the log: frames encoded late from `waiting`, and times a dropped frame was
    /// followed by nothing for a quarter of a second (the device was left with an old picture).
    private var late = 0, stale = 0, droppedAt = 0.0
    func takeCounts() -> (late: Int, stale: Int) {
        lock.lock()
        defer { late = 0; stale = 0; lock.unlock() }
        return (late, stale)
    }
    /// Called on VideoToolbox's thread; the encoder takes no new frame until it returns.
    var onFrame: ((Output) -> Void)?

    init(width: Int, height: Int, fps: Int, bitrate: Int, tenBit: Bool = true) throws {
        self.width = width
        self.height = height
        var s: VTCompressionSession?
        // The ordinary rate control, not the low-latency one: with reordering off it too gives
        // each frame back before the next goes in, and the hardware takes about half as long
        // over a frame (espalier measured it on an M4).
        var specKeys: [CFString: Any] = [kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: true]
        if Self.testMode == "lowlatency" { specKeys[kVTVideoEncoderSpecification_EnableLowLatencyRateControl] = true }
        let spec = specKeys as CFDictionary
        let status = VTCompressionSessionCreate(
            allocator: nil, width: Int32(width), height: Int32(height), codecType: kCMVideoCodecType_HEVC,
            encoderSpecification: spec, imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &s)
        guard status == noErr, let s else { throw NSError(domain: NSOSStatusErrorDomain, code: Int(status)) }
        let props: [(CFString, Any)] = [
            // Not "real time": that lets the encoder take its time up to the frame rate; this
            // has it finish each frame as fast as it can.
            (kVTCompressionPropertyKey_RealTime, Self.testMode != "plain"),
            (kVTCompressionPropertyKey_AllowFrameReordering, false),
                        (kVTCompressionPropertyKey_ExpectedFrameRate, fps),
            (kVTCompressionPropertyKey_AverageBitRate, bitrate),
            // Bytes per second over one second: 1.5× the average, as a ceiling for bursts.
            (kVTCompressionPropertyKey_DataRateLimits, [bitrate * 3 / 16, 1] as CFArray),
            // Keyframes only when the client asks.
            (kVTCompressionPropertyKey_MaxKeyFrameInterval, Int(Int32.max)),
            (kVTCompressionPropertyKey_ProfileLevel, tenBit ? kVTProfileLevel_HEVC_Main10_AutoLevel : kVTProfileLevel_HEVC_Main_AutoLevel),
            (kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, true),
            // What the stream says it is: BT.709, limited range.
            (kVTCompressionPropertyKey_ColorPrimaries, kCVImageBufferColorPrimaries_ITU_R_709_2),
            (kVTCompressionPropertyKey_TransferFunction, kCVImageBufferTransferFunction_ITU_R_709_2),
            (kVTCompressionPropertyKey_YCbCrMatrix, kCVImageBufferYCbCrMatrix_ITU_R_709_2),
        ]
        for (k, v) in props {
            let st = VTSessionSetProperty(s, key: k, value: v as CFTypeRef)
            if st != noErr { hostLog("encoder: property \(k) refused (\(st))") }
        }
        VTCompressionSessionPrepareToEncodeFrames(s)
        session = s
    }

    /// Another average bitrate, from the next frame on, with no keyframe.
    func setBitrate(_ bitrate: Int) {
        lock.lock()
        let s = session
        lock.unlock()
        guard let s else { return }
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: bitrate as CFTypeRef)
        VTSessionSetProperty(s, key: kVTCompressionPropertyKey_DataRateLimits, value: [bitrate * 3 / 16, 1] as CFArray)
    }

    func invalidate() {
        lock.lock()
        let s = session
        session = nil
        waiting = nil
        lock.unlock()
        if let s { VTCompressionSessionInvalidate(s) }
    }

    func requestKeyframe() {
        lock.lock()
        forceKey = true
        lock.unlock()
    }

    /// Encode now, or drop the frame (returns false) if the previous one isn't out and sent yet.
    @discardableResult
    func encode(_ pb: CVPixelBuffer, pts: CMTime) -> Bool {
        lock.lock()
        guard let session else {
            lock.unlock()
            return false
        }
        let now = CACurrentMediaTime()
        guard inFlight < maxInFlight else {
            if Self.keepsLast { waiting = (pb, pts) }
            if droppedAt == 0 { droppedAt = now }
            lock.unlock()
            return false
        }
        // A newer frame goes in: the one that waited is not needed.
        waiting = nil
        if droppedAt > 0 {
            if now - droppedAt > 0.25 { stale += 1 }
            droppedAt = 0
        }
        inFlight += 1
        let force = forceKey
        forceKey = false
        lock.unlock()
        let started = CACurrentMediaTime()
        let props = force ? [kVTEncodeFrameOptionKey_ForceKeyFrame: true] as CFDictionary : nil
        let status = VTCompressionSessionEncodeFrame(
            session, imageBuffer: pb, presentationTimeStamp: pts, duration: .invalid, frameProperties: props,
            infoFlagsOut: nil
        ) { [weak self] status, _, sb in
            guard let self else { return }
            defer {
                self.lock.lock()
                self.inFlight -= 1
                let w = self.waiting
                self.waiting = nil
                if w != nil { self.late += 1 }
                self.lock.unlock()
                if let w {
                    let again = { [weak self] in _ = self?.encode(w.0, pts: w.1) }
                    if let q = self.queue { q.async(execute: again) } else { again() }
                }
            }
            guard status == noErr, let sb else {
                if status != noErr { hostLog("encoder: frame failed (\(status))") }
                // The frame that was to be a keyframe is gone: ask again.
                if force { self.requestKeyframe() }
                return
            }
            self.output(sb, encodeMs: (CACurrentMediaTime() - started) * 1000)
        }
        if status != noErr {
            lock.lock()
            inFlight -= 1
            if force { forceKey = true }
            lock.unlock()
            return false
        }
        return true
    }

    private func output(_ sb: CMSampleBuffer, encodeMs: Double) {
        guard let desc = sb.formatDescription, let block = sb.dataBuffer else { return }
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[CFString: Any]]
        let keyframe = !((attachments?.first?[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
        var nalLength: Int32 = 4
        let params = HostEncoder.parameterSets(desc, nalLength: &nalLength)
        var length = 0
        var pointer: UnsafeMutablePointer<CChar>?
        guard
            CMBlockBufferGetDataPointer(
                block, atOffset: 0, lengthAtOffsetOut: nil, totalLengthOut: &length, dataPointerOut: &pointer) == noErr,
            let pointer
        else { return }
        var annexB = Data(capacity: length + 256)
        if keyframe, let params { annexB.append(params) }
        let bytes = UnsafeRawPointer(pointer).assumingMemoryBound(to: UInt8.self)
        var offset = 0
        let n = Int(nalLength)
        while offset + n <= length {
            var size = 0
            for i in 0..<n { size = size << 8 | Int(bytes[offset + i]) }
            offset += n
            guard size > 0, offset + size <= length else { break }
            annexB.append(contentsOf: [0, 0, 0, 1])
            annexB.append(bytes + offset, count: size)
            offset += size
        }
        onFrame?(Output(annexB: annexB, keyframe: keyframe, pts: sb.presentationTimeStamp, encodeMs: encodeMs))
    }

    /// VPS, SPS and PPS, each with a start code.
    static func parameterSets(_ desc: CMFormatDescription, nalLength: inout Int32) -> Data? {
        var count = 0
        guard
            CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                desc, parameterSetIndex: 0, parameterSetPointerOut: nil, parameterSetSizeOut: nil,
                parameterSetCountOut: &count, nalUnitHeaderLengthOut: &nalLength) == noErr
        else { return nil }
        var out = Data()
        for i in 0..<count {
            var p: UnsafePointer<UInt8>?
            var size = 0
            guard
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    desc, parameterSetIndex: i, parameterSetPointerOut: &p, parameterSetSizeOut: &size,
                    parameterSetCountOut: nil, nalUnitHeaderLengthOut: nil) == noErr, let p
            else { return nil }
            out.append(contentsOf: [0, 0, 0, 1])
            out.append(p, count: size)
        }
        return out
    }
}
