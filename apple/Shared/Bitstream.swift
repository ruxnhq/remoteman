// Just enough HEVC and AV1 bitstream reading to set VideoToolbox up: finding parameter sets in
// Annex-B, turning NAL units into the length-prefixed form, and reading an AV1 sequence header
// to build the av1C record.
import CoreMedia
import Foundation

struct BitstreamError: Error, CustomStringConvertible {
    let description: String
    init(_ s: String) { description = s }
}

// MARK: HEVC

/// The byte ranges of the NAL units in an Annex-B buffer (start codes and trailing zeros left out).
func annexBNALRanges(_ d: UnsafeBufferPointer<UInt8>) -> [Range<Int>] {
    var out: [Range<Int>] = []
    let n = d.count
    var i = 0
    var start = -1
    while i + 2 < n {
        if d[i + 2] > 1 {
            i += 3  // no start code can end here or at the next two bytes
        } else if d[i] == 0, d[i + 1] == 0, d[i + 2] == 1 {
            if start >= 0 {
                var end = i
                while end > start, d[end - 1] == 0 { end -= 1 }
                out.append(start..<end)
            }
            start = i + 3
            i += 3
        } else {
            i += 1
        }
    }
    if start >= 0 {
        var end = n
        while end > start, d[end - 1] == 0 { end -= 1 }
        if end > start { out.append(start..<end) }
    }
    return out
}

struct HEVCAccessUnit {
    /// VPS, SPS and PPS found in the access unit, in order (empty on frames that carry none).
    var parameterSets: [[UInt8]] = []
    /// The remaining NAL units, each behind a 4-byte big-endian length.
    var lengthPrefixed: [UInt8] = []
}

func parseHEVCAccessUnit(_ d: UnsafeBufferPointer<UInt8>) -> HEVCAccessUnit {
    var au = HEVCAccessUnit()
    au.lengthPrefixed.reserveCapacity(d.count + 16)
    for r in annexBNALRanges(d) where !r.isEmpty {
        let type = (d[r.lowerBound] >> 1) & 0x3f
        switch type {
        case 32, 33, 34:
            au.parameterSets.append(Array(UnsafeBufferPointer(rebasing: d[r])))
        case 35:
            continue  // access unit delimiter
        default:
            let len = UInt32(r.count)
            au.lengthPrefixed.append(UInt8(len >> 24))
            au.lengthPrefixed.append(UInt8((len >> 16) & 0xff))
            au.lengthPrefixed.append(UInt8((len >> 8) & 0xff))
            au.lengthPrefixed.append(UInt8(len & 0xff))
            au.lengthPrefixed.append(contentsOf: UnsafeBufferPointer(rebasing: d[r]))
        }
    }
    return au
}

func hevcFormatDescription(parameterSets: [[UInt8]]) throws -> CMFormatDescription {
    let types = parameterSets.map { ($0[0] >> 1) & 0x3f }
    guard types.contains(32), types.contains(33), types.contains(34) else {
        throw BitstreamError("HEVC keyframe without VPS+SPS+PPS (NAL types \(types))")
    }
    let total = parameterSets.reduce(0) { $0 + $1.count }
    let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: total)
    defer { buf.deallocate() }
    var ptrs: [UnsafePointer<UInt8>] = []
    var sizes: [Int] = []
    var off = 0
    for p in parameterSets {
        p.withUnsafeBufferPointer { (buf + off).update(from: $0.baseAddress!, count: p.count) }
        ptrs.append(UnsafePointer(buf + off))
        sizes.append(p.count)
        off += p.count
    }
    var fmt: CMFormatDescription?
    let st = CMVideoFormatDescriptionCreateFromHEVCParameterSets(
        allocator: nil, parameterSetCount: parameterSets.count, parameterSetPointers: ptrs,
        parameterSetSizes: sizes, nalUnitHeaderLength: 4, extensions: nil,
        formatDescriptionOut: &fmt)
    guard st == 0, let f = fmt else {
        throw BitstreamError("CMVideoFormatDescriptionCreateFromHEVCParameterSets: \(st)")
    }
    return f
}

// MARK: AV1

private struct BitReader {
    let b: UnsafeBufferPointer<UInt8>
    var pos = 0
    init(_ b: UnsafeBufferPointer<UInt8>) { self.b = b }
    mutating func f(_ n: Int) throws -> Int {
        var v = 0
        for _ in 0..<n {
            guard pos / 8 < b.count else { throw BitstreamError("AV1 sequence header truncated") }
            v = (v << 1) | Int((b[pos / 8] >> (7 - UInt8(pos % 8))) & 1)
            pos += 1
        }
        return v
    }
}

struct AV1OBU {
    let type: UInt8
    let whole: Range<Int>  // header, size field and payload
    let payload: Range<Int>
}

func av1OBUs(_ d: UnsafeBufferPointer<UInt8>) throws -> [AV1OBU] {
    var out: [AV1OBU] = []
    var i = 0
    while i < d.count {
        let start = i
        let hdr = d[i]
        i += 1
        if hdr & 0x04 != 0 { i += 1 }  // extension byte
        guard hdr & 0x02 != 0 else { throw BitstreamError("AV1 OBU without a size field") }
        var size = 0
        var shift = 0
        while i < d.count {
            let byte = d[i]
            i += 1
            size |= Int(byte & 0x7f) << shift
            shift += 7
            if byte & 0x80 == 0 { break }
        }
        guard i + size <= d.count else { throw BitstreamError("AV1 OBU runs past the frame") }
        out.append(AV1OBU(type: (hdr >> 3) & 0xf, whole: start..<(i + size), payload: i..<(i + size)))
        i += size
    }
    return out
}

struct AV1SequenceHeader {
    var profile = 0, level = 0, tier = 0
    var highBitdepth = 0, twelveBit = 0, mono = 0, subX = 1, subY = 1, chromaPos = 0
    var width = 0, height = 0
}

func parseAV1SequenceHeader(_ p: UnsafeBufferPointer<UInt8>) throws -> AV1SequenceHeader {
    var r = BitReader(p)
    var s = AV1SequenceHeader()
    s.profile = try r.f(3)
    _ = try r.f(1)  // still_picture
    let reduced = try r.f(1)
    var decoderModel = 0
    var bufferDelayLength = 0
    if reduced == 1 {
        s.level = try r.f(5)
    } else {
        if try r.f(1) == 1 {  // timing_info_present
            _ = try r.f(32); _ = try r.f(32)
            if try r.f(1) == 1 {  // equal_picture_interval, then a uvlc
                var zeros = 0
                while try r.f(1) == 0 { zeros += 1; if zeros > 32 { break } }
                if zeros < 32 { _ = try r.f(zeros) }
            }
            decoderModel = try r.f(1)
            if decoderModel == 1 {
                bufferDelayLength = try r.f(5) + 1
                _ = try r.f(32); _ = try r.f(5); _ = try r.f(5)
            }
        }
        let displayDelayPresent = try r.f(1)
        let points = try r.f(5) + 1
        for op in 0..<points {
            _ = try r.f(12)
            let level = try r.f(5)
            let tier = level > 7 ? try r.f(1) : 0
            if op == 0 { s.level = level; s.tier = tier }
            if decoderModel == 1, try r.f(1) == 1 {
                _ = try r.f(bufferDelayLength); _ = try r.f(bufferDelayLength); _ = try r.f(1)
            }
            if displayDelayPresent == 1, try r.f(1) == 1 { _ = try r.f(4) }
        }
    }
    let wBits = try r.f(4) + 1
    let hBits = try r.f(4) + 1
    s.width = try r.f(wBits) + 1
    s.height = try r.f(hBits) + 1
    if reduced == 0, try r.f(1) == 1 { _ = try r.f(4); _ = try r.f(3) }  // frame ids
    _ = try r.f(3)  // 128x128 superblocks, filter_intra, intra_edge_filter
    if reduced == 0 {
        _ = try r.f(4)  // interintra, masked compound, warped motion, dual filter
        let orderHint = try r.f(1)
        if orderHint == 1 { _ = try r.f(2) }
        var forceScreenContent = 2
        if try r.f(1) == 0 { forceScreenContent = try r.f(1) }
        if forceScreenContent > 0, try r.f(1) == 0 { _ = try r.f(1) }
        if orderHint == 1 { _ = try r.f(3) }
    }
    _ = try r.f(3)  // superres, cdef, restoration
    s.highBitdepth = try r.f(1)
    if s.profile == 2, s.highBitdepth == 1 { s.twelveBit = try r.f(1) }
    if s.profile != 1 { s.mono = try r.f(1) }
    var cp = 2, tc = 2, mc = 2
    if try r.f(1) == 1 { cp = try r.f(8); tc = try r.f(8); mc = try r.f(8) }
    if s.mono == 1 {
        _ = try r.f(1)
    } else if cp == 1, tc == 13, mc == 0 {
        s.subX = 0; s.subY = 0
    } else {
        _ = try r.f(1)  // colour range
        if s.profile == 0 {
            s.subX = 1; s.subY = 1
        } else if s.profile == 1 {
            s.subX = 0; s.subY = 0
        } else if s.highBitdepth == 1, s.twelveBit == 1 {
            s.subX = try r.f(1)
            s.subY = s.subX == 1 ? try r.f(1) : 0
        } else {
            s.subX = 1; s.subY = 0
        }
        if s.subX == 1, s.subY == 1 { s.chromaPos = try r.f(2) }
    }
    return s
}

/// The format description for an AV1 stream, from the sequence header OBU (header included).
func av1FormatDescription(sequenceHeaderOBU obu: [UInt8], payload: Range<Int>) throws -> CMFormatDescription {
    let s = try obu.withUnsafeBufferPointer {
        try parseAV1SequenceHeader(UnsafeBufferPointer(rebasing: $0[payload]))
    }
    var av1C: [UInt8] = [
        0x81,
        UInt8(s.profile << 5 | s.level),
        UInt8(s.tier << 7 | s.highBitdepth << 6 | s.twelveBit << 5 | s.mono << 4 | s.subX << 3
            | s.subY << 2 | s.chromaPos),
        0x00,
    ]
    av1C += obu
    let ext: [CFString: Any] = [
        kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: ["av1C": Data(av1C)]
    ]
    var fmt: CMFormatDescription?
    let st = CMVideoFormatDescriptionCreate(
        allocator: nil, codecType: kCMVideoCodecType_AV1, width: Int32(s.width),
        height: Int32(s.height), extensions: ext as CFDictionary, formatDescriptionOut: &fmt)
    guard st == 0, let f = fmt else {
        throw BitstreamError("CMVideoFormatDescriptionCreate(AV1): \(st)")
    }
    return f
}
