// One viewing session: a source of frames (a host through the core, or a recording), the
// decoder, the renderer, and the numbers for the stats overlay.
import Combine
import CoreMedia
import Foundation
import VideoToolbox

final class ViewerSession: ObservableObject {
    enum State: Equatable {
        case idle
        case connecting(String)
        case streaming(String)
        case failed(String)
    }

    struct Display {
        var width: Int
        var height: Int
        var scale: Float
        var refresh: Int
    }

    /// Where this device's display sits among the host's own.
    enum Placement: String, CaseIterable, Identifiable {
        case extend, main, only
        var id: String { rawValue }
        var label: String {
            switch self {
            case .extend: return "Extra display"
            case .main: return "Main display"
            case .only: return "Only display"
            }
        }
        /// The choice remembered for a computer (extend when none was made).
        static func saved(for host: String) -> Placement {
            Placement(rawValue: UserDefaults.standard.string(forKey: "placement." + host) ?? "") ?? .extend
        }
        func save(for host: String) { UserDefaults.standard.set(rawValue, forKey: "placement." + host) }
    }

    @Published private(set) var state: State = .idle
    @Published private(set) var statsText = ""
    @Published var showStats = true
    /// A text report filling the window instead of the stream (the decode benchmark).
    @Published var report: String?

    let renderer = VideoRenderer()
    /// The display link's real rate, measured while the connect form is showing.
    let refreshMeter = RefreshMeter()
    private(set) var client: CoreClient?
    /// Look, don't touch: with this on, no pointer, key or touch event leaves the app.
    @Published var viewOnly = false
    /// Where input goes: the client, unless the session is view-only.
    var input: CoreClient? { viewOnly ? nil : client }

    /// The host's cursor, sent apart from the picture so the view can draw it at the local
    /// pointer with no delay. All on the main thread.
    struct HostCursor {
        let image: CGImage
        let hotX: Int
        let hotY: Int
    }
    /// nil until the host has sent a shape; `.some(nil)` when the host has no cursor to show.
    private(set) var hostCursor: HostCursor??
    private(set) var hostCursorAt: (x: Float, y: Float, visible: Bool)?
    var cursorShapeChanged: (() -> Void)?
    var cursorMoved: ((Float, Float, Bool) -> Void)?

    private let pipeLock = NSLock()
    private var decoder: VideoDecoder?
    private var info: StreamInfo?
    private var playing = false
    private var generation = 0

    private let statsLock = NSLock()
    private var received = 0
    private var decodeTimes: [Double] = []
    private var decodeErrors = 0
    private var lastError = ""
    private var lastTick = DispatchTime.now().uptimeNanoseconds
    private var lastPresented = 0
    private var timer: Timer?

    /// The decoders this device has in hardware, most preferred first, as the core wants them.
    static var decoders: String {
        #if os(macOS)
            VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1)
        #endif
        var list: [String] = []
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1) { list.append("av1") }
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) { list += ["hevc444", "hevc"] }
        return list.joined(separator: ",")
    }

    static var stateDir: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let dir = base.appendingPathComponent("remoteman", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.path
    }

    var isActive: Bool {
        switch state {
        case .connecting, .streaming: return true
        default: return false
        }
    }

    // MARK: Sources

    func connect(
        host: String, pair: Bool, name: String, display: Display, decoders: String = ViewerSession.decoders,
        placement: Placement = .extend
    ) {
        guard !isActive else { return }
        let target = host.contains(":") ? host : host + ":7791"
        state = .connecting(target)
        generation += 1
        let gen = generation
        let config = CoreClient.Config(
            host: target, stateDir: Self.stateDir, name: name, pair: pair, width: display.width,
            height: display.height, scale: display.scale, refresh: display.refresh, decoders: decoders,
            options: placement == .extend ? "" : "placement=\(placement.rawValue)")
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                let client = try CoreClient.connect(config) { c in
                    c.onStream = { [weak self] info in self?.streamStarted(info) }
                    c.onFrame = { [weak self] data, key, number, captureUs, ageUs in
                        self?.frame(data, key: key, number: number, captureUs: captureUs, ageUs: ageUs)
                    }
                    c.onCursorShape = { [weak self] image, hotX, hotY in
                        DispatchQueue.main.async {
                            guard let self, self.generation == gen else { return }
                            self.hostCursor = .some(image.map { HostCursor(image: $0, hotX: hotX, hotY: hotY) })
                            self.cursorShapeChanged?()
                        }
                    }
                    c.onCursorAt = { [weak self] x, y, visible in
                        DispatchQueue.main.async {
                            guard let self, self.generation == gen else { return }
                            self.hostCursorAt = (x, y, visible)
                            self.cursorMoved?(x, y, visible)
                        }
                    }
                    c.onClosed = { [weak self] reason in
                        DispatchQueue.main.async { self?.closed(reason, generation: gen) }
                    }
                }
                let hostName = client.host.name
                DispatchQueue.main.async {
                    guard let self, self.generation == gen, case .connecting = self.state else {
                        client.close()
                        return
                    }
                    self.client = client
                    self.state = .streaming(hostName.isEmpty ? target : hostName)
                    self.startStats()
                }
            } catch {
                DispatchQueue.main.async {
                    guard let self, self.generation == gen else { return }
                    self.state = .failed("\(error)")
                }
            }
        }
    }

    /// Play a recording at its own rate, over and over.
    func play(recording url: URL) {
        guard !isActive else { return }
        let rec: Recording
        do { rec = try Recording(url: url) } catch {
            state = .failed("\(error)")
            return
        }
        generation += 1
        let gen = generation
        playing = true
        state = .streaming(url.lastPathComponent)
        startStats()
        Thread.detachNewThread { [weak self] in
            let interval = 1.0 / Double(max(rec.info.refresh, 1))
            while let self, self.playing, self.generation == gen {
                self.streamStarted(rec.info)
                let start = DispatchTime.now().uptimeNanoseconds
                for (i, f) in rec.frames.enumerated() {
                    guard self.playing, self.generation == gen else { return }
                    let due = start + UInt64(Double(i) * interval * 1e9)
                    let now = DispatchTime.now().uptimeNanoseconds
                    if due > now { usleep(UInt32((due - now) / 1000)) }
                    rec.data.withUnsafeBytes { raw in
                        self.frame(
                            UnsafeRawBufferPointer(rebasing: raw[f.range]), key: f.key, number: f.number,
                            captureUs: f.captureUs, ageUs: -1)
                    }
                }
            }
        }
    }

    func disconnect() {
        generation += 1
        playing = false
        let c = client
        client = nil
        c?.close()
        teardown()
        state = .idle
    }

    private func closed(_ reason: String, generation gen: Int) {
        guard gen == generation else { return }
        client = nil
        teardown()
        state = .failed(reason)
    }

    private func teardown() {
        timer?.invalidate()
        timer = nil
        pipeLock.lock()
        let d = decoder
        decoder = nil
        info = nil
        pipeLock.unlock()
        d?.invalidate()
        renderer?.clear()
        hostCursor = nil
        hostCursorAt = nil
        cursorShapeChanged?()
    }

    // MARK: Pipeline (core or playback thread)

    private func streamStarted(_ info: StreamInfo) {
        let d = VideoDecoder(
            info: info,
            onFrame: { [weak self] f in
                guard let self else { return }
                self.statsLock.lock()
                self.decodeTimes.append(f.decodeSeconds)
                if self.decodeLog.count < 200_000 {
                    self.decodeLog.append((DispatchTime.now().uptimeNanoseconds, f.decodeSeconds))
                }
                self.statsLock.unlock()
                self.renderer?.submit(
                    VideoRenderer.Frame(
                        pixelBuffer: f.pixelBuffer, number: f.number, ageUs: f.ageUs, receivedNs: f.receivedNs))
            },
            onError: { [weak self] e in self?.note(error: e) })
        pipeLock.lock()
        let old = decoder
        decoder = d
        self.info = info
        pipeLock.unlock()
        old?.invalidate()
    }

    private func frame(_ data: UnsafeRawBufferPointer, key: Bool, number: UInt32, captureUs: UInt64, ageUs: Int64) {
        statsLock.lock()
        received += 1
        if receivedLog.count < 200_000 { receivedLog.append(DispatchTime.now().uptimeNanoseconds) }
        if key { keyframesReceived += 1 }
        statsLock.unlock()
        pipeLock.lock()
        defer { pipeLock.unlock() }
        guard let decoder else { return }
        do {
            try decoder.decode(data, key: key, number: number, captureUs: captureUs, ageUs: ageUs)
        } catch {
            note(error: "\(error)")
        }
    }

    // MARK: Run summary (for unattended measurements)

    private var keyframesReceived = 0
    private var receivedLog: [UInt64] = []
    private var decodeLog: [(UInt64, Double)] = []

    /// Figures over the run so far, leaving out the first `skip` seconds after the first frame.
    func summary(skip: Double) -> String {
        let shown = renderer?.takeShownLog() ?? []
        statsLock.lock()
        let received = receivedLog, decodes = decodeLog
        statsLock.unlock()
        guard let first = received.first, let last = received.last else { return "no frames received" }
        let from = first + UInt64(skip * 1e9)
        let seconds = Double(last - min(last, from)) / 1e9
        guard seconds > 0.5 else { return "too short a run to summarise (\(received.count) frames)" }
        func pct(_ v: [Double], _ p: Double) -> String {
            guard !v.isEmpty else { return "–" }
            let s = v.sorted()
            return String(format: "%.2f", s[min(s.count - 1, Int(Double(s.count) * p))])
        }
        let ages = shown.filter { $0.0 >= from && $0.1 >= 0 }.map { Double($0.1) / 1000 }
        let shownCount = shown.filter { $0.0 >= from }.count
        let recv = received.filter { $0 >= from }.count
        let dec = decodes.filter { $0.0 >= from }.map { $0.1 * 1000 }
        // Gaps between frames shown, to see stutter that an average rate hides.
        let times = shown.filter { $0.0 >= from }.map { $0.0 }
        let gaps = zip(times.dropFirst(), times).map { Double($0 - $1) / 1e6 }
        var out = "summary over \(String(format: "%.1f", seconds)) s (first \(skip) s left out): \(info?.label ?? "?")\n"
        out += "  shown \(String(format: "%.1f", Double(shownCount) / seconds)) fps, received \(String(format: "%.1f", Double(recv) / seconds)) fps (\(recv) frames)\n"
        out += "  age at presentation ms: median \(pct(ages, 0.5)) p95 \(pct(ages, 0.95)) p99 \(pct(ages, 0.99)) max \(pct(ages, 1.0)) (\(ages.count) frames with a clock)\n"
        out += "  decode ms: median \(pct(dec, 0.5)) p95 \(pct(dec, 0.95)) p99 \(pct(dec, 0.99)) max \(pct(dec, 1.0))\n"
        out += "  gap between frames shown ms: median \(pct(gaps, 0.5)) p99 \(pct(gaps, 0.99)) max \(pct(gaps, 1.0))\n"
        if let c = client {
            let cs = c.stats
            out += "  lost \(cs.lost), loss reports \(cs.keyframeRequests), keyframes received \(keyframesReceived) (whole run), round trip \(String(format: "%.2f", Double(cs.roundTripUs) / 1000)) ms, decode errors \(decodeErrors)"
        }
        return out
    }

    private func note(error: String) {
        statsLock.lock()
        decodeErrors += 1
        lastError = error
        statsLock.unlock()
    }

    // MARK: Stats

    struct Snapshot {
        var label = ""
        var shownFps = 0.0
        var receivedFps = 0.0
        var decodeMedianMs = Double.nan
        var decodeMaxMs = Double.nan
        var ageMs = Double.nan
        var lost: UInt32 = 0
        var keyframeRequests: UInt32 = 0
        var roundTripMs = Double.nan
        var replaced = 0
        var decodeErrors = 0
        var lastError = ""

        var text: String {
            func f(_ x: Double, _ digits: Int = 1) -> String { x.isNaN ? "–" : String(format: "%.\(digits)f", x) }
            var s = "\(label)\n"
            s += "shown \(f(shownFps, 0)) fps · received \(f(receivedFps, 0)) fps · replaced unseen \(replaced)\n"
            s += "decode \(f(decodeMedianMs, 2)) ms (max \(f(decodeMaxMs, 2))) · age \(f(ageMs)) ms\n"
            s += "lost \(lost) · loss reports \(keyframeRequests) · round trip \(f(roundTripMs, 2)) ms"
            if decodeErrors > 0 { s += "\ndecode errors \(decodeErrors): \(lastError)" }
            return s
        }
    }

    /// The numbers since the last call.
    func takeStats() -> Snapshot {
        var s = Snapshot()
        let now = DispatchTime.now().uptimeNanoseconds
        let counters = renderer?.counters() ?? (0, 0, -1)
        statsLock.lock()
        let dt = max(Double(now - lastTick) / 1e9, 0.001)
        lastTick = now
        s.receivedFps = Double(received) / dt
        received = 0
        let times = decodeTimes.sorted()
        decodeTimes.removeAll(keepingCapacity: true)
        s.decodeErrors = decodeErrors
        s.lastError = lastError
        s.shownFps = Double(counters.0 - lastPresented) / dt
        lastPresented = counters.0
        statsLock.unlock()
        if !times.isEmpty {
            s.decodeMedianMs = times[times.count / 2] * 1000
            s.decodeMaxMs = times[times.count - 1] * 1000
        }
        s.replaced = counters.1
        if counters.2 >= 0 { s.ageMs = Double(counters.2) / 1000 }
        pipeLock.lock()
        s.label = info?.label ?? "waiting for the stream"
        pipeLock.unlock()
        if let c = client {
            let cs = c.stats
            s.lost = cs.lost
            s.keyframeRequests = cs.keyframeRequests
            if cs.roundTripUs > 0 { s.roundTripMs = Double(cs.roundTripUs) / 1000 }
        }
        return s
    }

    private func startStats() {
        timer?.invalidate()
        _ = takeStats()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            guard let self else { return }
            self.statsText = self.takeStats().text
        }
    }
}
