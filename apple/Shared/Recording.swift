// A recorded stream (remoteman-testclient --record): the frames a client is handed, on disk.
// "RMREC1\n", one line of JSON describing the stream, then per frame, little-endian:
// u32 length, u8 keyframe, u32 frame number, u64 capture time in µs, and the data.
import Foundation

struct Recording {
    struct Frame {
        let key: Bool
        let number: UInt32
        let captureUs: UInt64
        let range: Range<Int>
    }

    let info: StreamInfo
    let data: Data
    let frames: [Frame]

    init(url: URL) throws {
        let data = try Data(contentsOf: url, options: .mappedIfSafe)
        guard data.starts(with: Array("RMREC1\n".utf8)) else { throw BitstreamError("not an RMREC1 file") }
        guard let nl = data[7...].firstIndex(of: 0x0a) else { throw BitstreamError("no stream description") }
        guard
            let j = try JSONSerialization.jsonObject(with: data[7..<nl]) as? [String: Any],
            let codec = j["codec"] as? String, let width = j["width"] as? Int, let height = j["height"] as? Int
        else { throw BitstreamError("bad stream description") }
        info = StreamInfo(
            codec: codec == "av1" ? .av1 : .hevc, chroma: Int(j["chroma"] as? String ?? "420") ?? 420,
            depth: j["depth"] as? Int ?? 10, width: width, height: height, refresh: j["refresh"] as? Int ?? 60,
            hdr: (j["colour"] as? String) == "bt2020-pq")
        var frames: [Frame] = []
        var i = nl + 1
        func le<T: FixedWidthInteger>(_ at: Int, _: T.Type) -> T {
            var v: T = 0
            for k in 0..<MemoryLayout<T>.size { v |= T(data[at + k]) << (8 * k) }
            return v
        }
        while i + 17 <= data.count {
            let len = Int(le(i, UInt32.self))
            let start = i + 17
            guard start + len <= data.count else { break }
            frames.append(
                Frame(
                    key: data[i + 4] != 0, number: le(i + 5, UInt32.self), captureUs: le(i + 9, UInt64.self),
                    range: start..<(start + len)))
            i = start + len
        }
        self.data = data
        self.frames = frames
    }
}
