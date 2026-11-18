// One viewing session: a source of frames (a host through the core, or a recording), the
// decoder, the renderer, and the numbers for the stats overlay.
import Combine
import CoreMedia
import Foundation
import RemotemanCore
import VideoToolbox

final class ViewerSession: ObservableObject {
    enum State: Equatable {
        case idle
        case connecting(String)
        case streaming(String)
        case failed(String)
    }

    struct Display: Equatable {
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
    /// How far connecting has got (CoreClient.onStage); when it fails, the stage that failed.
    @Published private(set) var stage = 0
    /// Why the host ended the last session (CoreClient.closeCode); -1 when it wasn't the host.
    @Published private(set) var closeCode = -1
    /// The computer took this device off its trust list: it has to be paired again.
    var mustPairAgain: Bool { closeCode == 3 }
    @Published private(set) var statsText = ""
    /// The latest half-second of figures, for an app that draws them itself.
    @Published private(set) var stats = Snapshot()
    @Published var showStats = true
    /// A text report filling the window instead of the stream (the decode benchmark).
    @Published var report: String?

    let renderer = VideoRenderer()
    /// The display link's real rate, measured while the connect form is showing.
    let refreshMeter = RefreshMeter()
    private(set) var client: CoreClient?
    /// Look, don't touch: with this on, no pointer, key or touch event leaves the app.
    @Published var viewOnly = false
    /// Set while a modifier is latched on the key strip: text typed on the on-screen keyboard
    /// then goes as keys, so that Ctrl held and "c" typed is Ctrl-C rather than the letter c.
    var modifiersLatched = false
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

    // MARK: Features (what the host lists; see docs/PROTOCOL.md, "Features")

    /// What the connected host can do. Empty when not connected.
    @Published private(set) var features: Set<String> = []
    @Published private(set) var soundOn = false
    @Published private(set) var micOn = false
    @Published private(set) var clipboardOn = false
    /// Files on their way to or from the computer, newest last.
    @Published private(set) var transfers: [Transfer] = []
    /// What the host says the picture is encoded at, in bits a second (0: not told).
    @Published private(set) var hostBitrate = 0
    /// The ceiling this device asked for (0: the host chooses).
    @Published private(set) var bitrateLimit = 0
    /// Something for the user to read about a feature that didn't work.
    @Published var notice: String?
    /// What the host was last asked for.
    private(set) var display: Display?
    private(set) var placement = Placement.extend
    private var target = ""
    private let sound = SoundPlayer()
    private var mic: MicSender?
    private var sharing: Sharing?

    /// For tests: what the sound player has taken since the host announced the format.
    var soundReport: String {
        let t = sound.totals
        return sound.running
            ? "\(sound.format): \(t.packets) packets, \(t.bytes) bytes, \(t.starts) starts after a pause, \(t.concealed) concealed, \(t.dropped) left out, ran dry \(t.underruns) times; \(sound.tone)"
            : "no sound_format received (or sound is off)"
    }

    func lookAtClipboardNow() { sharing?.lookNow() }
    var clipboardLog: [String] { (sharing?.log ?? []) + [sharing?.tickReport ?? ""] }

    func has(_ feature: String) -> Bool { features.contains(feature) }

    private func remembered(_ what: String) -> Bool { UserDefaults.standard.bool(forKey: "\(what).\(target)") }
    private func remember(_ what: String, _ on: Bool) { UserDefaults.standard.set(on, forKey: "\(what).\(target)") }

    /// Play the computer's sound here. While on, the computer's own speakers are silent.
    func setSound(_ on: Bool) {
        guard let client, has("sound"), on != soundOn else { return }
        soundOn = on
        remember("sound", on)
        client.send(["t": "sound", "on": on])
        if !on { sound.stop() }
    }

    /// This device's microphone as a microphone on the computer. Never switched on by itself.
    func setMic(_ on: Bool) {
        guard let client, has("mic"), on != micOn else { return }
        if !on {
            micOn = false
            mic?.stop()
            mic = nil
            client.send(["t": "mic", "on": false])
            return
        }
        MicSender.allowed { [weak self] ok in
            guard let self, let client = self.client, !self.micOn else { return }
            guard ok else {
                self.notice = "remoteman isn't allowed to use the microphone. Allow it in Settings, under Privacy & Security."
                return
            }
            let m = MicSender()
            m.send = { [weak client] data, start, seq, timeUs in client?.sendMic(data, start: start, seq: seq, timeUs: timeUs) }
            client.send(["t": "mic", "on": true, "codec": "opus", "rate": 48000, "channels": 1, "frame_us": 10000])
            if let why = m.start() {
                client.send(["t": "mic", "on": false])
                self.notice = why
                return
            }
            self.mic = m
            self.micOn = true
        }
    }

    /// Share the clipboard both ways.
    func setClipboard(_ on: Bool) {
        guard let sharing, has("clipboard"), on != clipboardOn else { return }
        clipboardOn = on
        remember("clipboard", on)
        // On a Mac what is on the clipboard now goes along. On iPadOS and iOS reading it makes
        // the system ask each time, so only what is copied from here on is sent.
        #if os(macOS)
            sharing.setClipboard(on, sendCurrent: true)
        #else
            sharing.setClipboard(on, sendCurrent: false)
        #endif
    }

    func send(file url: URL) {
        guard has("files") else { return }
        sharing?.send(file: url)
    }

    func clearFinishedTransfers() { transfers.removeAll { $0.state != .moving } }

    /// The most the picture may use, in bits a second; 0 gives the choice back to the host.
    func setBitrateLimit(_ bits: Int) {
        guard let client, has("configure") else { return }
        bitrateLimit = bits
        client.send(["t": "configure", "bitrate": bits])
    }

    /// Ask for another size, scale, refresh rate or placement without leaving. False if this
    /// host can't (the caller then leaves and connects again).
    @discardableResult
    func reconfigure(display new: Display? = nil, placement newPlacement: Placement? = nil) -> Bool {
        guard let client, has("configure"), case .streaming = state else { return false }
        let d = new ?? display
        let p = newPlacement ?? placement
        guard let d else { return false }
        if d == display, p == placement { return true }
        display = d
        placement = p
        return client.send([
            "t": "configure",
            "display": ["width": d.width, "height": d.height, "scale": Double(d.scale), "refresh": d.refresh, "placement": p.rawValue],
        ])
    }

    /// One of the host's feature messages (core thread).
    private func message(_ m: [String: Any], sharing: Sharing, generation gen: Int) {
        switch m["t"] as? String {
        case "sound_format":
            let channels = (m["channels"] as? NSNumber)?.intValue ?? 2
            let frameUs = (m["frame_us"] as? NSNumber)?.intValue ?? 10000
            let why = (m["codec"] as? String ?? "opus") == "opus" ? sound.start(channels: channels, frameUs: frameUs) : "The computer's sound is in a form this device can't play."
            if let why { DispatchQueue.main.async { if self.generation == gen { self.notice = why } } }
        case "status":
            if let b = (m["bitrate"] as? NSNumber)?.intValue {
                DispatchQueue.main.async { if self.generation == gen { self.hostBitrate = b } }
            }
        case "file_result":
            sharing.fileResult(m)
        default:
            break
        }
    }

    private func transferChanged(_ t: Transfer) {
        if let i = transfers.firstIndex(where: { $0.id == t.id }) { transfers[i] = t } else { transfers.append(t) }
        if case .failed(let why) = t.state { notice = "\(t.name): \(why)" }
    }

    private let pipeLock = NSLock()
    private var decoder: VideoDecoder?
    private var info: StreamInfo?
    private var playing = false
    private var generation = 0

    private let statsLock = NSLock()
    private var received = 0
    private var receivedBytes = 0
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

    /// Forget a computer's key, so that it has to be paired again. `host` as given to connect.
    @discardableResult
    static func forget(host: String) -> Bool {
        rm_forget_host(stateDir, host.contains(":") ? host : host + ":7791") == 1
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
        self.target = target
        stage = 0
        closeCode = -1
        self.display = display
        self.placement = placement
        let config = CoreClient.Config(
            host: target, stateDir: Self.stateDir, name: name, pair: pair, width: display.width,
            height: display.height, scale: display.scale, refresh: display.refresh, decoders: decoders,
            options: placement == .extend ? "" : "placement=\(placement.rawValue)")
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                var shared: Sharing?
                let client = try CoreClient.connect(config) { c in
                    let sharing = Sharing(client: c)
                    shared = sharing
                    c.onStage = { [weak self] stage in
                        DispatchQueue.main.async { if let self, self.generation == gen { self.stage = stage } }
                    }
                    c.onMessage = { [weak self] m in self?.message(m, sharing: sharing, generation: gen) }
                    c.onSound = { [weak self] data, start, seq, _ in self?.sound.packet(data, start: start, seq: seq) }
                    c.onBlob = { what, id, kind, header, data, complete in
                        sharing.blob(what: what, id: id, kind: kind, header: header, data: data, complete: complete)
                    }
                    c.onStream = { [weak self] info in self?.streamStarted(info) }
                    c.onFrame = { [weak self] data, flags, number, captureUs, ageUs in
                        self?.frame(
                            data, key: flags & 1 != 0, recovery: flags & 2 != 0, number: number, captureUs: captureUs,
                            ageUs: ageUs)
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
                let features = client.features
                DispatchQueue.main.async {
                    guard let self, self.generation == gen, case .connecting = self.state else {
                        client.close()
                        return
                    }
                    self.client = client
                    self.features = features
                    self.sharing = shared
                    shared?.transferChanged = { [weak self] t in
                        guard let self, self.generation == gen else { return }
                        self.transferChanged(t)
                    }
                    self.state = .streaming(hostName.isEmpty ? target : hostName)
                    self.startStats()
                    // What was on for this computer last time comes back on; the microphone never does.
                    if self.remembered("sound") { self.setSound(true) }
                    if self.remembered("clipboard") { self.setClipboard(true) }
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
        closeCode = client?.closeCode ?? -1
        // On 3 the computer's key is kept: pairing again still checks it is the same computer.
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
        sound.stop()
        mic?.stop()
        mic = nil
        sharing?.stop()
        sharing = nil
        features = []
        soundOn = false
        micOn = false
        clipboardOn = false
        transfers = []
        hostBitrate = 0
        bitrateLimit = 0
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

    private func frame(
        _ data: UnsafeRawBufferPointer, key: Bool, recovery: Bool = false, number: UInt32, captureUs: UInt64, ageUs: Int64
    ) {
        statsLock.lock()
        received += 1
        receivedBytes += data.count
        let arrived = DispatchTime.now().uptimeNanoseconds
        if receivedLog.count < 200_000 { receivedLog.append(arrived) }
        // A wait of 55 ms or more before this frame: remember when, to spot a rhythm.
        if let last = lastArrival, arrived - last > 55_000_000 {
            pauses.append(arrived)
            if pauses.count > 40 { pauses.removeFirst(pauses.count - 40) }
        }
        lastArrival = arrived
        if key { keyframesReceived += 1 }
        if frameLog.count < 200_000 {
            frameLog.append((number, data.count, key ? 1 : recovery ? 2 : 0, DispatchTime.now().uptimeNanoseconds))
        }
        if recovery { recoveryPoints += 1
        }
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

    private var lastArrival: UInt64?
    private var pauses: [UInt64] = []

    /// Whether frames have been arriving with a pause about every half second (512 Wi-Fi time
    /// units) over the last several seconds: the mark of the device's radio leaving its channel
    /// at regular intervals, which Apple's peer-to-peer Wi-Fi (AirDrop, AirPlay, Continuity) and
    /// background scans do. Nothing is wrong with the stream when this shows; the link is pausing.
    /// Call with `statsLock` held.
    private func rhythmicPauses(now: UInt64) -> Int {
        let recent = pauses.filter { now - $0 < 8_000_000_000 }
        guard recent.count >= 6 else { return 0 }
        let steps = zip(recent.dropFirst(), recent).map { Double($0 - $1) / 1e6 }
        let onBeat = steps.filter { $0 > 470 && $0 < 580 }.count
        return Double(onBeat) >= Double(steps.count) * 0.7 ? recent.count : 0
    }
    private var rhythmSeen = 0
    private var keyframesReceived = 0
    /// Every frame handed over by the core: its number, size, whether a keyframe, and when.
    private var frameLog: [(UInt32, Int, Int, UInt64)] = []
    private var recoveryPoints = 0

    /// The frame log as CSV, and the frames around the longest waits between two frames.
    func frameReport(stalls: Int = 4) -> (csv: String, text: String) {
        statsLock.lock()
        let log = frameLog
        statsLock.unlock()
        guard let t0 = log.first?.3 else { return ("", "no frames") }
        var csv = "number,bytes,flags,ms\n"
        for f in log { csv += "\(f.0),\(f.1),\(f.2),\(String(format: "%.2f", Double(f.3 - t0) / 1e6))\n" }
        let sizes = log.map { $0.1 }.sorted()
        var text = "frames \(log.count), bytes median \(sizes[sizes.count / 2]) p95 \(sizes[sizes.count * 95 / 100]) max \(sizes.last ?? 0)\n"
        // How many numbers each loss skipped, and whether a recovery point followed.
        var skips: [Int: Int] = [:]
        var unflagged = 0
        for i in 1..<max(1, log.count) {
            let skipped = Int(log[i].0) - Int(log[i - 1].0) - 1
            if skipped > 0 {
                skips[min(skipped, 12), default: 0] += 1
                if log[i].2 == 0 { unflagged += 1 }
            }
        }
        text += "recovery points \(recoveryPoints); numbers skipped per loss (12 = 12 or more): "
            + skips.keys.sorted().map { "\($0): \(skips[$0]!)" }.joined(separator: ", ")
            + "; resumed on an unflagged frame \(unflagged) times\n"
        // The longest waits before a frame, after the first three seconds.
        let waits = (1..<log.count).filter { log[$0].3 - t0 > 3_000_000_000 }
            .map { ($0, Double(log[$0].3 - log[$0 - 1].3) / 1e6) }.sorted { $0.1 > $1.1 }.prefix(stalls)
        for (i, ms) in waits.sorted(by: { $0.0 < $1.0 }) {
            text += String(format: "wait of %.1f ms before frame %d (numbers skipped: %d):\n", ms, log[i].0, Int(log[i].0) - Int(log[i - 1].0) - 1)
            for j in max(0, i - 4)...min(log.count - 1, i + 6) {
                let dt = j > 0 ? Double(log[j].3 - log[j - 1].3) / 1e6 : 0
                text += String(format: "  %@ #%d  %6d B  +%.1f ms%@\n", j == i ? ">" : " ", log[j].0, log[j].1, dt, log[j].2 == 1 ? "  KEY" : log[j].2 == 2 ? "  RECOVERY" : "")
            }
        }
        return (csv, text)
    }
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
        statsLock.lock()
        let beats = rhythmSeen
        statsLock.unlock()
        out += "  half-second Wi-Fi pause rhythm seen in \(beats) of the half-second checks\n"
        if let c = client {
            let cs = c.stats
            out += "  lost \(cs.lost), loss reports \(cs.keyframeRequests), keyframes received \(keyframesReceived), recovery points \(recoveryPoints) (whole run), round trip \(String(format: "%.2f", Double(cs.roundTripUs) / 1000)) ms, decode errors \(decodeErrors)"
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
        var wifiPausing = false
        var megabitsPerSecond = Double.nan
        /// "AV1 10-bit" and "4:2:0", once the stream is known.
        var codec = ""
        var chroma = ""
        /// The picture's size, "2752 × 2064", and what the host encodes it at (0: not told).
        var size = ""
        var hostBitrate = 0
        /// Sound, while it plays: what is waiting to be played, and what was patched over.
        var soundOn = false
        var soundBufferMs = 0.0
        var soundConcealed = 0
        var soundDropped = 0

        var text: String {
            func f(_ x: Double, _ digits: Int = 1) -> String { x.isNaN ? "–" : String(format: "%.\(digits)f", x) }
            var s = "\(label)\n"
            s += "shown \(f(shownFps, 0)) fps · received \(f(receivedFps, 0)) fps · replaced unseen \(replaced)\n"
            s += "decode \(f(decodeMedianMs, 2)) ms (max \(f(decodeMaxMs, 2))) · age \(f(ageMs)) ms\n"
            s += "lost \(lost) · loss reports \(keyframeRequests) · round trip \(f(roundTripMs, 2)) ms"
            if soundOn { s += "\nsound buffer \(f(soundBufferMs, 0)) ms · concealed \(soundConcealed) · left out \(soundDropped)" }
            if decodeErrors > 0 { s += "\ndecode errors \(decodeErrors): \(lastError)" }
            if wifiPausing { s += "\nWi-Fi is pausing about twice a second (not the stream)" }
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
        s.megabitsPerSecond = Double(receivedBytes) * 8 / dt / 1_000_000
        receivedBytes = 0
        let times = decodeTimes.sorted()
        decodeTimes.removeAll(keepingCapacity: true)
        s.decodeErrors = decodeErrors
        s.lastError = lastError
        s.wifiPausing = rhythmicPauses(now: now) > 0
        if s.wifiPausing { rhythmSeen += 1 }
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
        if let info {
            s.codec = "\(info.codec == .av1 ? "AV1" : "HEVC") \(info.depth)-bit"
            s.chroma = info.chroma == 444 ? "4:4:4" : "4:2:0"
            s.size = "\(info.width) × \(info.height)"
        }
        pipeLock.unlock()
        if let c = client {
            let cs = c.stats
            s.lost = cs.lost
            s.keyframeRequests = cs.keyframeRequests
            if cs.roundTripUs > 0 { s.roundTripMs = Double(cs.roundTripUs) / 1000 }
        }
        s.hostBitrate = hostBitrate
        s.soundOn = soundOn
        if soundOn {
            // A link that pauses at intervals gets a deeper buffer, so the pauses aren't heard.
            sound.deep = s.wifiPausing
            let c = sound.takeCounters()
            s.soundBufferMs = c.queuedMs
            s.soundConcealed = c.concealed
            s.soundDropped = c.dropped
        }
        return s
    }

    private func startStats() {
        timer?.invalidate()
        _ = takeStats()
        timer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            guard let self else { return }
            let snapshot = self.takeStats()
            self.stats = snapshot
            self.statsText = snapshot.text
        }
    }
}
