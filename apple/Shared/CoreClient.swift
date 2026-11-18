// The Rust client core behind a Swift face. The core owns the connection; this passes its
// callbacks on and its input calls through.
import CoreGraphics
import Foundation
import RemotemanCore

final class CoreClient {
    struct Config {
        var host: String  // "name:port"
        var stateDir: String
        var name: String
        var pair: Bool
        var width: Int
        var height: Int
        var scale: Float
        var refresh: Int
        var decoders: String  // "av1,hevc444,hevc"
        /// "key=value;..." passed to the core: so far placement=extend|main|only.
        var options: String = ""
    }

    struct Stats {
        var lost: UInt32 = 0
        var keyframeRequests: UInt32 = 0
        var roundTripUs: UInt32 = 0
    }

    // Called on the core's threads, one at a time.
    var onStream: ((StreamInfo) -> Void)?
    /// `flags`: bit 0 a keyframe, bit 1 a recovery point (an ordinary frame to the decoder, the
    /// first one after a loss).
    var onFrame: ((UnsafeRawBufferPointer, _ flags: UInt8, _ number: UInt32, _ captureUs: UInt64, _ ageUs: Int64) -> Void)?
    var onClosed: ((String) -> Void)?
    /// The host's cursor image (nil: no cursor) and its hotspot, in the host display's pixels.
    /// Setting this before connecting is what makes the host leave the cursor out of the picture.
    var onCursorShape: ((CGImage?, _ hotX: Int, _ hotY: Int) -> Void)?
    /// Where the host's cursor is, as fractions of the display, and whether it is on it.
    var onCursorAt: ((Float, Float, Bool) -> Void)?
    /// One of the host's feature messages (sound_format, status, file_result, camera_keyframe).
    var onMessage: (([String: Any]) -> Void)?
    /// One Opus packet of the desktop's sound: the packet, whether it is the first after a
    /// pause, its number, and the host's clock at capture.
    var onSound: ((UnsafeRawBufferPointer, _ start: Bool, _ seq: UInt32, _ timeUs: UInt64) -> Void)?
    /// A blob from the host. what 0: opened (kind, header). 1: a piece. 2: closed (complete).
    var onBlob: ((_ what: UInt8, _ id: UInt32, _ kind: UInt8, _ header: [String: Any], UnsafeRawBufferPointer, _ complete: Bool) -> Void)?

    /// How far connecting has got: 0 looking the host up, 1 reaching it and checking
    /// identities (pairing happens here), 2 hello, 3 the host is making the display.
    var onStage: ((Int) -> Void)?

    static let clipboardKind: UInt8 = 0x40
    static let fileKind: UInt8 = 0x41

    /// Why the host closed the connection, known once `onClosed` is called: 0 the host stopped
    /// or another device took over, 1 something failed there, 2 its owner ended this connection
    /// (connecting again is fine), 3 this device was taken off its trust list (pair again),
    /// -1 it ended some other way (the network, this side leaving).
    private(set) var closeCode = -1
    private var handle: OpaquePointer?
    private let lock = NSLock()
    private var closedSeen = false

    private init() {}

    /// Blocks until the host answers: call it off the main thread. `configure` sets the
    /// callbacks before anything can arrive.
    static func connect(_ c: Config, configure: (CoreClient) -> Void) throws -> CoreClient {
        let client = CoreClient()
        configure(client)
        // The core keeps `ctx` until on_closed; hold the object for that long.
        let ctx = Unmanaged.passRetained(client).toOpaque()
        let callbacks = rm_callbacks(
            ctx: ctx,
            on_stream: { ctx, _, codec, chroma, depth, width, height, refresh, hdr in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                me.onStream?(
                    StreamInfo(
                        codec: Codec(rawValue: codec) ?? .hevc, chroma: Int(chroma), depth: Int(depth),
                        width: Int(width), height: Int(height), refresh: Int(refresh), hdr: hdr != 0))
            },
            on_frame: { ctx, data, len, key, number, captureUs, ageUs in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                me.onFrame?(UnsafeRawBufferPointer(start: data, count: len), key, number, captureUs, ageUs)
            },
            on_closed: { ctx, reason in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                me.lock.lock()
                let first = !me.closedSeen
                me.closedSeen = true
                me.lock.unlock()
                guard first else { return }
                me.lock.lock()
                if let h = me.handle { me.closeCode = Int(rm_client_close_code(h)) }
                me.lock.unlock()
                me.onClosed?(reason.map { String(cString: $0) } ?? "closed")
                Unmanaged<CoreClient>.fromOpaque(ctx!).release()
            },
            on_cursor_shape: client.onCursorShape == nil
                ? nil
                : { ctx, rgba, width, height, hotX, hotY in
                    let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                    var image: CGImage?
                    if let rgba, width > 0, height > 0 {
                        let data = Data(bytes: rgba, count: Int(width) * Int(height) * 4)
                        if let provider = CGDataProvider(data: data as CFData) {
                            image = CGImage(
                                width: Int(width), height: Int(height), bitsPerComponent: 8, bitsPerPixel: 32,
                                bytesPerRow: Int(width) * 4, space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                                provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
                        }
                    }
                    me.onCursorShape?(image, Int(hotX), Int(hotY))
                },
            on_cursor_at: client.onCursorAt == nil
                ? nil
                : { ctx, x, y, visible in
                    let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                    me.onCursorAt?(x, y, visible != 0)
                },
            on_message: { ctx, json in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                guard let json, let m = CoreClient.object(String(cString: json)) else { return }
                me.onMessage?(m)
            },
            on_sound: { ctx, data, len, start, seq, timeUs in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                me.onSound?(UnsafeRawBufferPointer(start: data, count: len), start != 0, seq, timeUs)
            },
            on_blob: { ctx, what, id, kind, header, data, len, complete in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                let h = header.flatMap { CoreClient.object(String(cString: $0)) } ?? [:]
                me.onBlob?(what, id, kind, h, UnsafeRawBufferPointer(start: data, count: data == nil ? 0 : len), complete != 0)
            },
            on_stage: { ctx, stage in
                let me = Unmanaged<CoreClient>.fromOpaque(ctx!).takeUnretainedValue()
                me.onStage?(Int(stage))
            })
        var err = [CChar](repeating: 0, count: 512)
        let h = rm_client_connect(
            c.host, c.stateDir, c.name, c.pair ? 1 : 0, UInt32(c.width), UInt32(c.height), c.scale,
            UInt32(c.refresh), c.decoders, c.options, callbacks, &err, err.count)
        guard let h else {
            Unmanaged<CoreClient>.fromOpaque(ctx).release()
            throw BitstreamError(String(cString: err))
        }
        client.lock.lock()
        client.handle = h
        client.lock.unlock()
        return client
    }

    private func with(_ body: (OpaquePointer) -> Void) {
        lock.lock()
        if let h = handle { body(h) }
        lock.unlock()
    }

    /// "name", "os", "fingerprint" of the host.
    var host: (name: String, os: String, fingerprint: String) {
        var out = [CChar](repeating: 0, count: 512)
        with { rm_client_host($0, &out, out.count) }
        let parts = String(cString: out).split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        return (parts.first ?? "", parts.count > 1 ? parts[1] : "", parts.count > 2 ? parts[2] : "")
    }

    /// What the host can do ("video", "input", "cursor", "placement", "configure", "sound", ...).
    var features: Set<String> {
        var out = [CChar](repeating: 0, count: 512)
        with { rm_client_features($0, &out, out.count) }
        return Set(String(cString: out).split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) })
    }

    static func object(_ json: String) -> [String: Any]? {
        (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any]
    }

    /// One feature message. False if the core doesn't know it or the connection is gone.
    @discardableResult
    func send(_ message: [String: Any]) -> Bool {
        guard let data = try? JSONSerialization.data(withJSONObject: message), let json = String(data: data, encoding: .utf8)
        else { return false }
        var ok = false
        with { ok = rm_client_send_message($0, json) != 0 }
        return ok
    }

    func sendMic(_ packet: UnsafeRawBufferPointer, start: Bool, seq: UInt32, timeUs: UInt64) {
        with { rm_client_send_mic($0, packet.bindMemory(to: UInt8.self).baseAddress, packet.count, start ? 1 : 0, seq, timeUs) }
    }

    func sendCamera(_ frame: UnsafeRawBufferPointer, key: Bool, timeUs: UInt64) {
        with { rm_client_send_camera($0, frame.bindMemory(to: UInt8.self).baseAddress, frame.count, key ? 1 : 0, timeUs) }
    }

    /// Send a blob, a piece at a time from `next` (nil: the end). Waits while the network is
    /// behind: never call it on the main thread or from a callback. False if it didn't all go.
    func sendBlob(kind: UInt8, header: [String: Any], next: () -> Data?) -> Bool {
        guard let h = try? JSONSerialization.data(withJSONObject: header), let json = String(data: h, encoding: .utf8)
        else { return false }
        var blob: OpaquePointer?
        with { blob = rm_client_blob_open($0, kind, json) }
        guard let blob else { return false }
        while let piece = next() {
            if piece.isEmpty { continue }
            let ok = piece.withUnsafeBytes { rm_blob_write(blob, $0.bindMemory(to: UInt8.self).baseAddress, piece.count) }
            if ok == 0 {
                rm_blob_cancel(blob)
                return false
            }
        }
        return rm_blob_finish(blob) != 0
    }

    var stats: Stats {
        var s = Stats()
        with { rm_client_stats($0, &s.lost, &s.keyframeRequests, &s.roundTripUs) }
        return s
    }

    func pointerTo(x: Float, y: Float) { with { rm_client_pointer_to($0, x, y) } }
    func pointerBy(dx: Float, dy: Float) { with { rm_client_pointer_by($0, dx, dy) } }
    func button(_ b: UInt8, down: Bool) { with { rm_client_button($0, b, down ? 1 : 0) } }
    func scroll(dx: Float, dy: Float) { with { rm_client_scroll($0, dx, dy) } }
    func key(usage: UInt16, down: Bool) { with { rm_client_key($0, usage, down ? 1 : 0) } }
    func touch(phase: UInt8, slot: UInt8, x: Float, y: Float) { with { rm_client_touch($0, phase, slot, x, y) } }
    func text(_ s: String) { with { rm_client_text($0, s) } }

    /// Leave. Safe to call more than once.
    func close() {
        lock.lock()
        let h = handle
        handle = nil
        lock.unlock()
        if let h { rm_client_close(h) }
    }
}
