// Timing hardware decode of a recorded stream, for the rmbench tool and the app's own benchmark.
// Frames are fed at the stream's own rate (or as fast as they decode with --flat-out), and the
// time from handing a frame to VideoToolbox to getting the picture back is measured per frame.
import CoreVideo
import Foundation

func runBench(_ url: URL, paced: Bool, loops: Int) -> String {
    let rec: Recording
    do { rec = try Recording(url: url) } catch { return "\(url.lastPathComponent): \(error)" }
    let lock = NSLock()
    var times: [Double] = []
    var errors: [String] = []
    var pixelFormat: OSType = 0
    var lastOut = 0.0
    var outGaps: [Double] = []
    let dec = VideoDecoder(
        info: rec.info,
        onFrame: { f in
            let now = Double(DispatchTime.now().uptimeNanoseconds) / 1e9
            lock.lock()
            times.append(f.decodeSeconds)
            if lastOut > 0 { outGaps.append(now - lastOut) }
            lastOut = now
            pixelFormat = CVPixelBufferGetPixelFormatType(f.pixelBuffer)
            lock.unlock()
        },
        onError: { e in
            lock.lock()
            errors.append(e)
            lock.unlock()
        })
    let interval = 1.0 / Double(max(rec.info.refresh, 1))
    let t0 = DispatchTime.now().uptimeNanoseconds
    var fed = 0
    var wall = 0.0
    // A recording has one keyframe, so it can only be played once per session.
    for _ in 0..<loops {
        dec.invalidate()
        let start = DispatchTime.now().uptimeNanoseconds
        for (i, f) in rec.frames.enumerated() {
            if paced {
                let due = start + UInt64(Double(i) * interval * 1e9)
                let now = DispatchTime.now().uptimeNanoseconds
                if due > now { usleep(UInt32((due - now) / 1000)) }
            }
            do {
                try rec.data.withUnsafeBytes { raw in
                    try dec.decode(
                        UnsafeRawBufferPointer(rebasing: raw[f.range]), key: f.key, number: f.number,
                        captureUs: f.captureUs)
                }
                fed += 1
            } catch {
                lock.lock()
                errors.append("\(error)")
                lock.unlock()
            }
            if !paced { dec.finish() }
        }
        dec.finish()
    }
    wall = Double(DispatchTime.now().uptimeNanoseconds - t0) / 1e9
    let hw = dec.usingHardware
    dec.invalidate()

    func pct(_ v: [Double], _ p: Double) -> Double {
        guard !v.isEmpty else { return .nan }
        let s = v.sorted()
        return s[min(s.count - 1, Int((Double(s.count) * p).rounded(.down)))]
    }
    func ms(_ x: Double) -> String { String(format: "%.2f", x * 1000) }
    let fourcc = String(bytes: [24, 16, 8, 0].map { UInt8((pixelFormat >> $0) & 0xff) }, encoding: .ascii) ?? "?"
    let keyBytes = rec.frames.first.map { $0.range.count } ?? 0
    let avgBytes = rec.frames.isEmpty ? 0 : rec.frames.reduce(0) { $0 + $1.range.count } / rec.frames.count
    var out = "\(rec.info.label)  [\(url.lastPathComponent)]\n"
    out += "  frames: \(rec.frames.count) in file (keyframe \(keyBytes) B, mean \(avgBytes) B), fed \(fed), decoded \(times.count), errors \(errors.count)\n"
    out += "  hardware decoder: \(hw), output pixel format: \(fourcc)\n"
    out += "  decode ms: median \(ms(pct(times, 0.5)))  p95 \(ms(pct(times, 0.95)))  p99 \(ms(pct(times, 0.99)))  max \(ms(times.max() ?? .nan))  first \(ms(times.first ?? .nan))\n"
    if paced {
        out += "  paced at \(rec.info.refresh) fps; gap between pictures ms: median \(ms(pct(outGaps, 0.5)))  p99 \(ms(pct(outGaps, 0.99)))  max \(ms(outGaps.max() ?? .nan))\n"
    } else {
        out += "  flat out: \(String(format: "%.0f", Double(times.count) / wall)) fps over \(String(format: "%.2f", wall)) s\n"
    }
    for e in errors.prefix(3) { out += "  error: \(e)\n" }
    return out
}
