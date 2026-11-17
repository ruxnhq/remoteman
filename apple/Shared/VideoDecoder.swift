// Hardware decode of the frames the core hands over: HEVC access units in Annex-B, or AV1
// temporal units. Output is GPU-backed 10-bit biplanar pixel buffers for Metal.
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

enum Codec: UInt8 {
    case hevc = 0
    case av1 = 1
}

struct StreamInfo: Equatable {
    var codec: Codec
    var chroma: Int  // 420 or 444
    var depth: Int
    var width: Int
    var height: Int
    var refresh: Int
    var hdr: Bool

    var label: String {
        "\(codec == .av1 ? "AV1" : "HEVC") \(chroma == 444 ? "4:4:4" : "4:2:0") \(depth)-bit \(width)×\(height) @\(refresh)"
    }
}

struct DecodedFrame {
    let pixelBuffer: CVPixelBuffer
    let number: UInt32
    let captureUs: UInt64
    /// From handing the frame to VideoToolbox to getting the picture back.
    let decodeSeconds: Double
}

final class VideoDecoder {
    let info: StreamInfo
    private let onFrame: (DecodedFrame) -> Void
    private let onError: (String) -> Void
    private var session: VTDecompressionSession?
    private var format: CMFormatDescription?
    private var formatKey: [UInt8] = []
    private(set) var usingHardware = false

    /// `onFrame` and `onError` are called on VideoToolbox's threads.
    init(info: StreamInfo, onFrame: @escaping (DecodedFrame) -> Void, onError: @escaping (String) -> Void) {
        self.info = info
        self.onFrame = onFrame
        self.onError = onError
        #if os(macOS)
            if info.codec == .av1 { VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1) }
        #endif
    }

    deinit { invalidate() }

    func invalidate() {
        if let s = session {
            VTDecompressionSessionWaitForAsynchronousFrames(s)
            VTDecompressionSessionInvalidate(s)
        }
        session = nil
        format = nil
        formatKey = []
    }

    func finish() {
        if let s = session { VTDecompressionSessionWaitForAsynchronousFrames(s) }
    }

    /// Decode one frame. Call from one thread at a time, in stream order.
    func decode(_ data: UnsafeRawBufferPointer, key: Bool, number: UInt32, captureUs: UInt64) throws {
        let bytes = data.bindMemory(to: UInt8.self)
        var payload: [UInt8]
        switch info.codec {
        case .hevc:
            let au = parseHEVCAccessUnit(bytes)
            if !au.parameterSets.isEmpty {
                var k: [UInt8] = []
                for p in au.parameterSets {
                    k += p
                    k.append(0xff)
                }
                if k != formatKey {
                    try makeSession(hevcFormatDescription(parameterSets: au.parameterSets))
                    formatKey = k
                }
            }
            payload = au.lengthPrefixed
        case .av1:
            let obus = try av1OBUs(bytes)
            payload = []
            payload.reserveCapacity(bytes.count)
            for o in obus {
                switch o.type {
                case 2, 8, 15: continue  // temporal delimiter, tile list, padding: not in a sample
                case 1:
                    let whole = Array(UnsafeBufferPointer(rebasing: bytes[o.whole]))
                    if whole != formatKey {
                        let p = (o.payload.lowerBound - o.whole.lowerBound)..<(o.payload.upperBound - o.whole.lowerBound)
                        try makeSession(av1FormatDescription(sequenceHeaderOBU: whole, payload: p))
                        formatKey = whole
                    }
                    payload.append(contentsOf: whole)
                default:
                    payload.append(contentsOf: UnsafeBufferPointer(rebasing: bytes[o.whole]))
                }
            }
        }
        guard let session, let format else {
            throw BitstreamError("frame \(number) arrived before any keyframe with parameters")
        }
        guard !payload.isEmpty else { return }

        var block: CMBlockBuffer?
        var st = CMBlockBufferCreateWithMemoryBlock(
            allocator: nil, memoryBlock: nil, blockLength: payload.count, blockAllocator: nil,
            customBlockSource: nil, offsetToData: 0, dataLength: payload.count,
            flags: kCMBlockBufferAssureMemoryNowFlag, blockBufferOut: &block)
        guard st == 0, let block else { throw BitstreamError("CMBlockBufferCreate: \(st)") }
        st = payload.withUnsafeBytes {
            CMBlockBufferReplaceDataBytes(
                with: $0.baseAddress!, blockBuffer: block, offsetIntoDestination: 0, dataLength: payload.count)
        }
        guard st == 0 else { throw BitstreamError("CMBlockBufferReplaceDataBytes: \(st)") }

        var timing = CMSampleTimingInfo(
            duration: .invalid, presentationTimeStamp: CMTime(value: CMTimeValue(captureUs), timescale: 1_000_000),
            decodeTimeStamp: .invalid)
        var size = payload.count
        var sample: CMSampleBuffer?
        st = CMSampleBufferCreateReady(
            allocator: nil, dataBuffer: block, formatDescription: format, sampleCount: 1,
            sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleSizeEntryCount: 1,
            sampleSizeArray: &size, sampleBufferOut: &sample)
        guard st == 0, let sample else { throw BitstreamError("CMSampleBufferCreateReady: \(st)") }

        let started = DispatchTime.now().uptimeNanoseconds
        let onFrame = self.onFrame
        let onError = self.onError
        st = VTDecompressionSessionDecodeFrame(
            session, sampleBuffer: sample, flags: [._EnableAsynchronousDecompression], infoFlagsOut: nil
        ) { status, _, image, _, _ in
            guard status == 0, let image else {
                onError("decode of frame \(number) failed: \(status)")
                return
            }
            let took = Double(DispatchTime.now().uptimeNanoseconds - started) / 1e9
            onFrame(DecodedFrame(pixelBuffer: image, number: number, captureUs: captureUs, decodeSeconds: took))
        }
        guard st == 0 else { throw BitstreamError("VTDecompressionSessionDecodeFrame(\(number)): \(st)") }
    }

    private func makeSession(_ fmt: CMFormatDescription) throws {
        if let s = session {
            if VTDecompressionSessionCanAcceptFormatDescription(s, formatDescription: fmt) {
                format = fmt
                return
            }
            VTDecompressionSessionWaitForAsynchronousFrames(s)
            VTDecompressionSessionInvalidate(s)
            session = nil
        }
        let spec: [CFString: Any] = [
            kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder: true
        ]
        let pixelFormat =
            info.chroma == 444
            ? kCVPixelFormatType_444YpCbCr10BiPlanarVideoRange
            : kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
        let attrs: [CFString: Any] = [
            kCVPixelBufferPixelFormatTypeKey: pixelFormat,
            kCVPixelBufferMetalCompatibilityKey: true,
            kCVPixelBufferIOSurfacePropertiesKey: [:] as CFDictionary,
        ]
        var s: VTDecompressionSession?
        let st = VTDecompressionSessionCreate(
            allocator: nil, formatDescription: fmt, decoderSpecification: spec as CFDictionary,
            imageBufferAttributes: attrs as CFDictionary, outputCallback: nil, decompressionSessionOut: &s)
        guard st == 0, let s else {
            throw BitstreamError("no hardware decoder for \(info.label): VTDecompressionSessionCreate \(st)")
        }
        var v: CFTypeRef?
        let pst = withUnsafeMutablePointer(to: &v) {
            VTSessionCopyProperty(
                s, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder, allocator: nil,
                valueOut: $0)
        }
        usingHardware = pst == 0 && ((v as? Bool) ?? false)
        session = s
        format = fmt
    }
}
