// One viewing session: a source of frames (a host through the core, or a recording), the
// decoder, the renderer, and the numbers for the stats overlay.
#if os(macOS)
import AppKit
#else
import UIKit
#endif
import AVFoundation
import Combine
import CoreMedia
import Foundation
import Network
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

    /// Which side of the computer's other displays a second display sits on, chosen per computer.
    enum SecondSide: UInt8, CaseIterable, Identifiable {
        case right = 0, left, above, below
        var id: UInt8 { rawValue }
        var label: String { ["To the right", "To the left", "Above", "Below"][Int(rawValue)] }
        static func saved(for host: String) -> SecondSide {
            SecondSide(rawValue: UInt8(UserDefaults.standard.integer(forKey: "secondSide." + host))) ?? .right
        }
        func save(for host: String) { UserDefaults.standard.set(Int(rawValue), forKey: "secondSide." + host) }
    }

    /// How many pixels to ask a computer for, chosen per computer: this screen's own, or the same
    /// shape with its shorter side held to 1440 or 1080. The scale shrinks by the same factor
    /// (it may fall below 1), so things keep their size on this screen. Fewer pixels are quicker
    /// to encode: a Mac's encoder takes about 16 ms over 3840×2160, twice a 120 Hz frame.
    enum PictureSize: Int, CaseIterable, Identifiable {
        case own = 0, lines1440 = 1440, lines1080 = 1080
        /// The Mac app only: the window's own pixels, asked for again when the window has been
        /// resized and still for a second. (On iPhone and iPad the app's area is always followed.)
        case window = -1
        var id: Int { rawValue }
        /// What this device offers.
        static var choices: [PictureSize] {
            #if os(macOS)
            [.own, .window, .lines1440, .lines1080]
            #else
            [.own, .lines1440, .lines1080]
            #endif
        }
        var label: String {
            #if os(macOS)
            if self == .own { return "The window's size when connecting" }
            #endif
            switch self {
            case .window: return "The window's size, as it changes"
            case .own: return "This screen's own"
            case .lines1440: return "Up to 1440 lines"
            case .lines1080: return "Up to 1080 lines"
            }
        }
        static func saved(for host: String) -> PictureSize {
            PictureSize(rawValue: UserDefaults.standard.integer(forKey: "pictureSize." + host)) ?? .own
        }
        func save(for host: String) { UserDefaults.standard.set(rawValue, forKey: "pictureSize." + host) }
        func fit(_ d: Display) -> Display {
            let short = min(d.width, d.height)
            guard rawValue > 0, short > rawValue else { return d }
            let f = Double(rawValue) / Double(short)
            func even(_ v: Int) -> Int { max(2, Int((Double(v) * f / 2).rounded()) * 2) }
            return Display(width: even(d.width), height: even(d.height), scale: d.scale * Float(f), refresh: d.refresh)
        }
    }

    /// How many times brighter than SDR white this device's screen can go (1: no HDR), and how
    /// many it shows right now (it changes with the brightness setting and the room).
    static var forcedHeadroom: Float?
    static var screenHeadroom: Float {
        if let f = forcedHeadroom { return f }
        #if os(macOS)
            return Float(NSScreen.main?.maximumPotentialExtendedDynamicRangeColorComponentValue ?? 1)
        #else
            if #available(iOS 16.0, *) { return Float(UIScreen.main.potentialEDRHeadroom) }
            return 1
        #endif
    }
    static var headroomNow: Float {
        if let f = forcedHeadroom { return f }
        #if os(macOS)
            return Float(NSScreen.main?.maximumExtendedDynamicRangeColorComponentValue ?? 1)
        #else
            if #available(iOS 16.0, *) { return Float(UIScreen.main.currentEDRHeadroom) }
            return 1
        #endif
    }
    /// What the hello and configure say about HDR: asked for when the screen can show it and
    /// the person hasn't switched it off.
    static var hdrAsk: Float? {
        if let f = forcedHeadroom { return f > 1.05 ? f : nil }  // a test asking as if it had this headroom
        let h = screenHeadroom
        return ViewerPrefs.hdr && h > 1.05 ? h : nil
    }

    /// The most a computer's picture may use, chosen per computer, in Mbit/s (0: the computer decides).
    enum BitrateCap {
        static let choices = [0, 20, 50, 100, 150, 300, 500]
        static func label(_ m: Int) -> String { m == 0 ? "Automatic" : "\(m) Mbit/s" }
        static func saved(for host: String) -> Int { UserDefaults.standard.integer(forKey: "bitrateCap." + host) }
        static func save(_ m: Int, for host: String) { UserDefaults.standard.set(m, forKey: "bitrateCap." + host) }
    }

    /// A computer's address as sessions know it (with the port).
    static func key(for address: String) -> String { address.contains(":") ? address : address + ":7791" }

    /// Where this device's display sits among the host's own.
    enum Placement: String, CaseIterable, Identifiable {
        case extend, main, only
        /// Not a display of this device's at all: the computer's main monitor as it is, at its
        /// own size (a computer that lists "mirror").
        case mirror
        /// The three ways this device's display joins the computer's own.
        static let own: [Placement] = [.extend, .main, .only]
        var id: String { rawValue }
        var label: String {
            switch self {
            case .extend: return "Extra display"
            case .main: return "Main display"
            case .only: return "Only display"
            case .mirror: return "Mirror its main monitor"
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
    /// While the computer waits for its owner to confirm this device: the six digits to compare.
    @Published private(set) var pairingCode: String?
    /// The computer is logging its owner in for this device; connecting is tried again meanwhile.
    @Published private(set) var loggingIn = false
    private var again: (() -> Void)?
    private var loginRetries = 0
    private var retrying = false
    /// The computer is locked and wants its user's password: its words for people. Set as the
    /// session fails; cleared by the next connect.
    @Published private(set) var lockedAsk: String?
    /// The computer has let this device in: it has said what it will send. Its hello comes
    /// before it looks at its lock, so `.streaming` alone doesn't say a password was right.
    var letIn: (() -> Void)?
    /// The computer wants a code from an authenticator app: `left` is -1 the first time, after a
    /// wrong code the tries left. Answer with `answerSecondFactor`, or `disconnect` to give up.
    struct SecondFactorAsk: Identifiable {
        let id = UUID()
        var left: Int
    }
    @Published var secondFactor: SecondFactorAsk?
    /// A code to answer with without asking (--second-factor, for tests).
    static var automaticCode: String?
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
    /// The second display's picture, on a computer that lists "displays" (a second window on
    /// the Mac, an external monitor on iPad).
    let renderer2 = VideoRenderer()
    @Published private(set) var secondOn = false
    /// What the second display was last asked to be.
    private(set) var secondDisplay: Display?
    private var decoder2: VideoDecoder?
    private var aimed: UInt8 = 0
    /// Which display this device's own screen sends its pointer and touches to: 0 the picture
    /// it shows, 1 the second display (an iPad as the touch surface for its external monitor).
    @Published var touchDisplay: UInt8 = 0

    /// Ask the computer for a second display of this size (again: another size), or give it back.
    func setSecondDisplay(_ d: Display?) {
        guard let client, has("displays") else { return }
        secondDisplay = d
        if let d {
            client.secondDisplay(on: true, width: d.width, height: d.height, scale: d.scale, refresh: d.refresh, side: SecondSide.saved(for: target).rawValue)
            secondOn = true
        } else {
            client.secondDisplay(on: false)
            endSecond()
        }
    }

    private func endSecond() {
        secondOn = false
        touchDisplay = 0
        pipeLock.lock()
        let d = decoder2
        decoder2 = nil
        pipeLock.unlock()
        d?.invalidate()
        renderer2?.clear()
        if aimed != 0 {
            aimed = 0
            client?.pointerOn(0)
        }
    }

    /// Pointer and touch events after this are on that display (0 the first, 1 the second).
    /// Said to the computer only when it changes.
    func aim(_ index: UInt8) {
        guard index != aimed, let c = input else { return }
        aimed = index
        c.pointerOn(index)
    }

    private func stream2Started(_ info: StreamInfo) {
        DispatchQueue.main.async { [weak self] in
            self?.renderer2?.headroom = { ViewerSession.headroomNow }
            self?.renderer2?.setHDR(info.hdr)
        }
        let d = VideoDecoder(
            info: info,
            onFrame: { [weak self] f in
                self?.renderer2?.submit(
                    VideoRenderer.Frame(pixelBuffer: f.pixelBuffer, number: f.number, ageUs: f.ageUs, receivedNs: f.receivedNs))
            },
            // A picture that can't be decoded: ask for a whole one.
            onError: { [weak self] _ in self?.client?.send(["t": "display_keyframe", "index": 1]) })
        pipeLock.lock()
        let old = decoder2
        decoder2 = d
        pipeLock.unlock()
        old?.invalidate()
    }

    private func frame2(_ data: UnsafeRawBufferPointer, key: Bool, number: UInt32, captureUs: UInt64, ageUs: Int64) {
        pipeLock.lock()
        defer { pipeLock.unlock() }
        // A frame or two can still arrive just after the display was given back.
        guard let decoder2 else { return }
        do { try decoder2.decode(data, key: key, number: number, captureUs: captureUs, ageUs: ageUs) } catch {
            client?.send(["t": "display_keyframe", "index": 1])
        }
    }
    /// The display link's real rate, measured while the connect form is showing.
    let refreshMeter = RefreshMeter()
    private(set) var client: CoreClient?
    /// Asked from the picture itself (a three-finger tap, a keyboard shortcut): show or hide the
    /// session's controls, and leave the session. Set by the screen that has the controls.
    var toggleControls: (() -> Void)?
    var leaveRequested: (() -> Void)?

    /// Look, don't touch: with this on, no pointer, key or touch event leaves the app.
    @Published var viewOnly = false
    /// Set while a modifier is latched on the key strip: text typed on the on-screen keyboard
    /// then goes as keys, so that Ctrl held and "c" typed is Ctrl-C rather than the letter c.
    var modifiersLatched = false
    /// Where input goes: the client, unless the session is view-only.
    var input: CoreClient? { viewOnly ? nil : client }

    /// A refusal as the computer worded it ("the host refused: not_paired: pairing was declined
    /// on the computer" is shown as "Pairing was declined on the computer").
    /// The words of a "locked" refusal, if this is one.
    static func lockedWords(_ reason: String) -> String? {
        for lead in ["the host refused: locked: ", "locked: "] {
            if let r = reason.range(of: lead), lead != "locked: " || r.lowerBound == reason.startIndex || reason[..<r.lowerBound].hasSuffix(": ") {
                var text = String(reason[r.upperBound...])
                // A closing reason carries its code after the words: "... (code 1)".
                if text.hasSuffix(")"), let code = text.range(of: " (code ", options: .backwards) { text = String(text[..<code.lowerBound]) }
                return text.prefix(1).uppercased() + text.dropFirst()
            }
        }
        return nil
    }

    static func plain(_ reason: String) -> String {
        guard let r = reason.range(of: "the host refused: ") else { return reason }
        var text = String(reason[r.upperBound...])
        // The code in front of the words is for programs.
        if let colon = text.range(of: ": "), !text[..<colon.lowerBound].contains(" ") { text = String(text[colon.upperBound...]) }
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    /// A key of this device's own keyboard, sent as the settings say (InputSettings).
    func hardKey(_ usage: UInt16, down: Bool) {
        guard let c = input else { return }
        c.key(usage: InputSettings.map(usage, hostOS: c.host.os), down: down)
    }

    /// Keys pressed together and let go: a shortcut this device's own keyboard can't send.
    func chord(_ usages: [UInt16]) {
        guard let c = input else { return }
        for u in usages { c.key(usage: u, down: true) }
        for u in usages.reversed() { c.key(usage: u, down: false) }
    }

    /// Scrolling by this many display pixels as this device gave it, sent in the direction and at
    /// the speed chosen.
    func wheel(dx: Float, dy: Float) {
        let k = InputSettings.scrollFactor * (InputSettings.naturalScroll ? 1 : -1)
        input?.scroll(dx: dx * k, dy: dy * k)
    }

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
    /// The view saying it has taken or let go of the pointer (a captured pointer on the Mac).
    var pointerCaptured: ((Bool) -> Void)?

    // MARK: Features (what the host lists; see docs/PROTOCOL.md, "Features")

    /// What the connected host can do. Empty when not connected.
    @Published private(set) var features: Set<String> = []
    @Published private(set) var soundOn = false
    @Published private(set) var micOn = false
    @Published private(set) var clipboardOn = false
    @Published private(set) var cameraOn = false
    private var camera: CameraSender?
    /// Files on their way to or from the computer, newest last.
    @Published private(set) var transfers: [Transfer] = []
    /// What the host says the picture is encoded at, in bits a second (0: not told).
    @Published private(set) var hostBitrate = 0
    /// The picture coming in is HDR (the computer said BT.2020 PQ): the session's controls say so.
    @Published private(set) var streamHDR = false
    /// The ceiling this device asked for (0: the host chooses).
    @Published private(set) var bitrateLimit = 0
    /// Something for the user to read about a feature that didn't work.
    @Published var notice: String?
    /// What the host was last asked for.
    private(set) var display: Display?
    private(set) var placement = Placement.extend
    private var target = ""
    /// The connected computer's address as settings are kept under it.
    var hostKey: String { target }
    private let sound = SoundPlayer()
    private var hostOffsetUs: Int64?
    private var soundAges: [Double] = []
    /// Median age of the sound packets when they arrived, ms (nil before any).
    func soundAgeMs() -> Double? {
        statsLock.lock()
        defer { statsLock.unlock() }
        guard !soundAges.isEmpty else { return nil }
        return soundAges.sorted()[soundAges.count / 2]
    }
    private let soundSetup = DispatchQueue(label: "remoteman.sound.setup")
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

    /// What was chosen for this computer last time; where nothing was, what the settings say.
    private func remembered(_ what: String) -> Bool {
        if let chosen = UserDefaults.standard.object(forKey: "\(what).\(target)") as? Bool { return chosen }
        switch what {
        case "sound": return ViewerPrefs.soundDefault
        case "clipboard": return ViewerPrefs.clipboardDefault
        default: return false
        }
    }

    #if os(iOS)
        /// Whether this device is on mobile data now.
        private static let path: NWPathMonitor = {
            let m = NWPathMonitor()
            m.start(queue: .global(qos: .utility))
            return m
        }()
        static func watchNetwork() { _ = path }
        static var onMobileData: Bool { path.currentPath.usesInterfaceType(.cellular) }
    #endif
    private func remember(_ what: String, _ on: Bool) { UserDefaults.standard.set(on, forKey: "\(what).\(target)") }

    /// Play the computer's sound here. While on, the computer's own speakers are silent.
    func setSound(_ on: Bool) {
        guard let client, has("sound"), on != soundOn else { return }
        soundOn = on
        remember("sound", on)
        client.send(["t": "sound", "on": on])
        if !on { soundSetup.async { [sound] in sound.stop() } }
    }

    /// This device's microphone as a microphone on the computer. Never switched on by itself.
    func setMic(_ on: Bool) {
        guard let client, has("mic"), on != micOn else { return }
        remember("mic", on)
        if !on {
            micOn = false
            mic?.stop()
            micSent = mic?.totals ?? micSent
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

    /// This device's camera as a camera on the computer. Never switched on by itself.
    func setCamera(_ on: Bool) {
        guard let client, has("camera"), on != cameraOn else { return }
        remember("camera", on)
        if !on {
            cameraOn = false
            camera?.stop()
            camera = nil
            client.send(["t": "camera", "on": false])
            return
        }
        CameraSender.allowed { [weak self] ok in
            guard let self, let client = self.client, !self.cameraOn else { return }
            guard ok else {
                self.notice = "remoteman isn't allowed to use the camera. Allow it in Settings, under Privacy & Security."
                return
            }
            let c = CameraSender()
            c.send = { [weak client] data, key, timeUs in client?.sendCamera(data, key: key, timeUs: timeUs) }
            client.send(["t": "camera", "on": true, "codec": "h264", "width": 1280, "height": 720, "fps": 30])
            if let why = c.start() {
                client.send(["t": "camera", "on": false])
                self.notice = why
                return
            }
            self.camera = c
            self.cameraOn = true
        }
    }

    /// For tests: what the microphone has sent. Kept after it goes off, until the next time it is on.
    private var micSent = (packets: 0, bytes: 0)
    var micReport: String {
        let t = mic?.totals ?? micSent
        return "\(t.packets) packets, \(t.bytes) bytes\(mic == nil ? " (the microphone is off)" : "")"
    }

    /// For tests: what the camera has sent.
    var cameraReport: String {
        guard let c = camera else { return "the camera is off" }
        let t = c.totals
        return "\(t.frames) frames, \(t.bytes) bytes, \(t.keyframes) keyframes, last \(t.width)x\(t.height), turned \(Int(c.turned)) degrees to stay upright; forced \(c.forced), holding an IDR picture \(c.idr), the computer asked \(c.asked) times; encoder:\(c.encoderNote)"
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
        let ask = PictureSize.saved(for: target).fit(d)
        var display: [String: Any] = ["width": ask.width, "height": ask.height, "scale": Double(ask.scale), "refresh": ask.refresh, "placement": p.rawValue]
        if let h = Self.hdrAsk {
            display["hdr"] = true
            display["headroom"] = Double(h)
        }
        return client.send(["t": "configure", "display": display])
    }

    /// One of the host's feature messages (core thread).
    private func message(_ m: [String: Any], sharing: Sharing, generation gen: Int) {
        switch m["t"] as? String {
        case "sound_format":
            let channels = (m["channels"] as? NSNumber)?.intValue ?? 2
            let frameUs = (m["frame_us"] as? NSNumber)?.intValue ?? 10000
            // Off the core's thread: a sound output that is slow to start (or stuck) must not
            // hold up the picture. Packets that come before it is ready are let go.
            soundSetup.async { [self] in
                let why = (m["codec"] as? String ?? "opus") == "opus" ? sound.start(channels: channels, frameUs: frameUs) : "The computer's sound is in a form this device can't play."
                if let why { DispatchQueue.main.async { if self.generation == gen { self.notice = why } } }
            }
        case "stream_light":
            // Before an HDR stream: where SDR white sits in it (nits).
            let white = (m["sdr_white"] as? NSNumber)?.floatValue ?? 203
            let second = (m["index"] as? NSNumber)?.intValue == 1
            DispatchQueue.main.async { (second ? self.renderer2 : self.renderer)?.sdrWhite = white > 0 ? white : 203 }
        case "status":
            if let b = (m["bitrate"] as? NSNumber)?.intValue {
                DispatchQueue.main.async { if self.generation == gen { self.hostBitrate = b } }
            }
        case "features":
            // The host changed what it offers while connected: the controls follow, and what
            // was switched off there stops here.
            let list = Set((m["features"] as? [String]) ?? [])
            DispatchQueue.main.async { [self] in
                guard generation == gen, client != nil else { return }
                features = list
                if soundOn, !has("sound") {
                    soundOn = false
                    soundSetup.async { [sound] in sound.stop() }
                }
                if micOn, !has("mic") {
                    micOn = false
                    mic?.stop()
                    mic = nil
                }
                if cameraOn, !has("camera") {
                    cameraOn = false
                    camera?.stop()
                    camera = nil
                }
                if clipboardOn, !has("clipboard") {
                    clipboardOn = false
                    sharing.setClipboard(false, sendCurrent: false)
                }
            }
        case "camera_keyframe":
            DispatchQueue.main.async { self.camera?.keyframe() }
        case "file_result":
            sharing.fileResult(m)
        case "display":
            // The computer couldn't make the second display, or took it away.
            if (m["index"] as? NSNumber)?.intValue == 1, m["on"] as? Bool == false {
                let why = m["message"] as? String
                DispatchQueue.main.async {
                    guard self.generation == gen else { return }
                    self.endSecond()
                    self.notice = why.map { "The second display ended: \($0)" } ?? "The second display ended."
                }
            }
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
    private var gamepads: Gamepads?

    /// The decoders this device has in hardware, most preferred first, as the core wants them.
    static var decoders: String {
        #if os(macOS)
            VTRegisterSupplementalVideoDecoderIfAvailable(kCMVideoCodecType_AV1)
        #endif
        var list: [String] = []
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_AV1) { list.append("av1") }
        if VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) { list += ["hevc444", "hevc"] }
        // "Sharp text": full colour detail (4:4:4) ahead of everything else.
        if ViewerPrefs.sharp, let i = list.firstIndex(of: "hevc444") { list.insert(list.remove(at: i), at: 0) }
        return list.joined(separator: ",")
    }

    /// Forget a computer's key, so that it has to be paired again. `host` as given to connect.
    @discardableResult
    static func forget(host: String) -> Bool {
        rm_forget_host(stateDir, host.contains(":") ? host : host + ":7791") == 1
    }

    /// A state directory of its own for a test run (--state), so that it keeps its own paired hosts.
    static var stateOverride: String?
    static var stateDir: String {
        if let o = stateOverride {
            try? FileManager.default.createDirectory(atPath: o, withIntermediateDirectories: true)
            return o
        }
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
        placement: Placement = .extend, unlock: String? = nil, attachOnly: Bool = false
    ) {
        guard !isActive || retrying else { return }
        if !retrying { loginRetries = 0 }
        retrying = false
        again = { [weak self] in
            self?.connect(
                host: host, pair: pair, name: name, display: display, decoders: decoders, placement: placement, unlock: unlock,
                attachOnly: attachOnly)
        }
        lockedAsk = nil
        let target = host.contains(":") ? host : host + ":7791"
        state = .connecting(target)
        generation += 1
        let gen = generation
        self.target = target
        stage = 0
        closeCode = -1
        self.display = display
        self.placement = placement
        // `self.display` stays this screen's own; what is asked for may be smaller.
        let display = PictureSize.saved(for: target).fit(display)
        let config = CoreClient.Config(
            host: target, stateDir: Self.stateDir, name: name, pair: pair, width: display.width,
            height: display.height, scale: display.scale, refresh: display.refresh, decoders: decoders,
            options: [
                placement == .extend ? nil : "placement=\(placement.rawValue)",
                // Only a session that is already running, never the computer's login screen
                // (a computer that lists "way_in"; others take no notice).
                attachOnly ? "way-in=attach" : nil,
                // This screen can show HDR, so many times brighter than its SDR white.
                Self.hdrAsk.map { String(format: "hdr=%.2f", $0) },
                // The locked computer's password, as hex (the options are split on ";" and "=").
                unlock.map { "unlock-hex=" + $0.utf8.map { String(format: "%02x", $0) }.joined() },
            ].compactMap { $0 }.joined(separator: ";"))
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            do {
                var shared: Sharing?
                let client = try CoreClient.connect(config) { c in
                    let sharing = Sharing(client: c)
                    shared = sharing
                    c.onStage = { [weak self] stage in
                        DispatchQueue.main.async {
                            guard let self, self.generation == gen else { return }
                            self.stage = stage
                            // Past the hello the owner has said yes.
                            if stage >= 3 { self.pairingCode = nil }
                        }
                    }
                    c.onPairing = { [weak self] code, _ in
                        DispatchQueue.main.async { if let self, self.generation == gen { self.pairingCode = code } }
                    }
                    c.onMessage = { [weak self] m in self?.message(m, sharing: sharing, generation: gen) }
                    c.onSound = { [weak self] data, start, seq, timeUs in
                        guard let self else { return }
                        // The packet's age on arrival, on the host's clock as the frames give it.
                        self.statsLock.lock()
                        if let off = self.hostOffsetUs {
                            let age = Double(Int64(DispatchTime.now().uptimeNanoseconds / 1000) + off - Int64(timeUs)) / 1000
                            self.soundAges.append(age)
                            if self.soundAges.count > 400 { self.soundAges.removeFirst(200) }
                        }
                        self.statsLock.unlock()
                        self.sound.packet(data, start: start, seq: seq)
                    }
                    c.onBlob = { what, id, kind, header, data, complete in
                        sharing.blob(what: what, id: id, kind: kind, header: header, data: data, complete: complete)
                    }
                    c.onStream = { [weak self] info in
                        self?.streamStarted(info)
                        DispatchQueue.main.async { if let self, self.generation == gen { self.letIn?() } }
                    }
                    c.onStream2 = { [weak self] info in self?.stream2Started(info) }
                    c.onFrame2 = { [weak self] data, flags, number, captureUs, ageUs in
                        self?.frame2(data, key: flags & 1 != 0, number: number, captureUs: captureUs, ageUs: ageUs)
                    }
                    c.onFrame = { [weak self] data, flags, number, captureUs, ageUs in
                        // The host's clock against this one, from a frame whose age the core knows.
                        if let self, ageUs >= 0 {
                            self.statsLock.lock()
                            self.hostOffsetUs = Int64(captureUs) + ageUs - Int64(DispatchTime.now().uptimeNanoseconds / 1000)
                            self.statsLock.unlock()
                        }
                        self?.frame(
                            data, key: flags & 1 != 0, recovery: flags & 2 != 0, number: number, captureUs: captureUs,
                            ageUs: ageUs)
                    }
                    // Without this the host draws its cursor into the picture.
                    if InputSettings.localPointer {
                    c.onCursorShape = { [weak self] image, hotX, hotY in
                        DispatchQueue.main.async {
                            guard let self, self.generation == gen else { return }
                            self.hostCursor = .some(image.map { HostCursor(image: $0, hotX: hotX, hotY: hotY) })
                            self.cursorShapeChanged?()
                        }
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
                    client.onSecondFactor { [weak self, weak client] left in
                        DispatchQueue.main.async {
                            guard let self, let client, self.generation == gen else { return }
                            if let code = Self.automaticCode {
                                print("remoteman: second factor asked (\(left) tries left), answering with the given code")
                                Self.automaticCode = left >= 0 ? nil : code
                                client.secondFactor(code: code, remember: false)
                            } else {
                                self.secondFactor = SecondFactorAsk(left: left)
                            }
                        }
                    }
                    // A computer that lets this device watch only: say so once, since nothing it does will reach it.
                    if !features.contains("input") { self.notice = "View only: this computer shows its screen but takes no input from this device." }
                    self.sharing = shared
                    shared?.transferChanged = { [weak self] t in
                        guard let self, self.generation == gen else { return }
                        self.transferChanged(t)
                    }
                    self.pairingCode = nil
                    self.loggingIn = false
                    self.state = .streaming(hostName.isEmpty ? target : hostName)
                    self.startStats()
                    let pads = Gamepads(wanted: { [weak self] in self?.has("gamepad") ?? false }) { [weak self] pad, flags, buttons, lt, rt, lx, ly, rx, ry in
                        self?.client?.gamepad(pad: pad, flags: flags, buttons: buttons, lt: lt, rt: rt, lx: lx, ly: ly, rx: rx, ry: ry) ?? false
                    }
                    self.gamepads?.stop()
                    self.gamepads = pads
                    pads.start()
                    // What was on for this computer last time comes back on. The microphone and
                    // camera only for those who asked for that, and only where the system has
                    // already said yes (so that nothing is asked as a session starts).
                    if self.remembered("sound") { self.setSound(true) }
                    if self.remembered("clipboard") { self.setClipboard(true) }
                    if ViewerPrefs.rememberMedia {
                        if self.remembered("mic"), AVCaptureDevice.authorizationStatus(for: .audio) == .authorized { self.setMic(true) }
                        if self.remembered("camera"), AVCaptureDevice.authorizationStatus(for: .video) == .authorized { self.setCamera(true) }
                    }
                    // The ceiling chosen for this computer; on mobile data the saving one, if lower.
                    let cap = BitrateCap.saved(for: self.target) * 1_000_000
                    if cap > 0, self.bitrateLimit == 0, self.has("configure") { self.setBitrateLimit(cap) }
                    #if os(iOS)
                        if Self.onMobileData, ViewerPrefs.saveData, self.bitrateLimit == 0 || self.bitrateLimit > ViewerPrefs.mobileLimit, self.has("configure") {
                            self.setBitrateLimit(ViewerPrefs.mobileLimit)
                            self.notice = "On mobile data: the picture is kept to \(ViewerPrefs.mobileLimit / 1_000_000) Mb/s."
                        }
                    #endif
                }
            } catch {
                DispatchQueue.main.async {
                    guard let self, self.generation == gen else { return }
                    // A computer that is logging its owner in for this device (it was locked,
                    // or nobody was logged in) is tried again until it is ready.
                    if self.comeBack(after: "\(error)", generation: gen) { return }
                    self.loggingIn = false
                    self.lockedAsk = Self.lockedWords("\(error)")
                    self.state = .failed(Self.plain("\(error)"))
                }
            }
        }
    }

    /// For trying the session's controls where there is no picture to show (the simulator has
    /// no hardware decoder): a session with nothing behind it.
    func demo(name: String) {
        guard !isActive else { return }
        generation += 1
        state = .streaming(name)
    }

    /// Play a recording at its own rate, over and over.
    func play(recording url: URL) {
        guard !isActive else { return }
        let rec: Recording
        do { rec = try Recording(url: url) } catch {
            lockedAsk = Self.lockedWords("\(error)")
            state = .failed(Self.plain("\(error)"))
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

    func answerSecondFactor(code: String, remember: Bool) {
        secondFactor = nil
        client?.secondFactor(code: code, remember: remember)
    }

    func disconnect() {
        generation += 1
        secondFactor = nil
        playing = false
        let c = client
        client = nil
        c?.close()
        teardown()
        state = .idle
    }

    /// A computer that says it is logging its owner in for this device is tried again in a few
    /// seconds, some ten times. The words can come as the refusal of the hello or as the reason
    /// the connection was closed. True if another try is on its way.
    private func comeBack(after reason: String, generation gen: Int) -> Bool {
        guard ["logging in:", "logging_in:"].contains(where: { reason.lowercased().contains($0) }), loginRetries < 10, let again else { return false }
        loginRetries += 1
        let c = client
        client = nil
        c?.close()
        // As after any session: what was on (sound, clipboard, microphone) is asked for afresh.
        teardown()
        loggingIn = true
        state = .connecting(target)
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            guard let self, self.generation == gen else { return }
            self.retrying = true
            again()
        }
        return true
    }

    private func closed(_ reason: String, generation gen: Int) {
        guard gen == generation else { return }
        if comeBack(after: reason, generation: gen) { return }
        closeCode = client?.closeCode ?? -1
        secondFactor = nil
        // On 3 the computer's key is kept: pairing again still checks it is the same computer.
        let c = client
        client = nil
        c?.close()
        teardown()
        lockedAsk = Self.lockedWords(reason)
        state = .failed(Self.plain(reason))
    }

    private func teardown() {
        gamepads?.stop()
        gamepads = nil
        pairingCode = nil
        streamHDR = false
        loggingIn = false
        timer?.invalidate()
        timer = nil
        pipeLock.lock()
        let d = decoder
        decoder = nil
        info = nil
        pipeLock.unlock()
        d?.invalidate()
        renderer?.clear()
        pipeLock.lock()
        let d2 = decoder2
        decoder2 = nil
        pipeLock.unlock()
        d2?.invalidate()
        renderer2?.clear()
        secondOn = false
        secondDisplay = nil
        aimed = 0
        soundSetup.async { [sound] in sound.stop() }
        mic?.stop()
        mic = nil
        camera?.stop()
        camera = nil
        cameraOn = false
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
        DispatchQueue.main.async { [weak self] in
            self?.renderer?.headroom = { ViewerSession.headroomNow }
            self?.renderer?.setHDR(info.hdr)
            self?.streamHDR = info.hdr
        }
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
        let onScreen = renderer?.takeOnScreenLog() ?? []
        out += "  arrival to on screen ms: median \(pct(onScreen, 0.5)) p95 \(pct(onScreen, 0.95)) p99 \(pct(onScreen, 0.99)) (\(onScreen.count) frames; drawn \(VideoRenderer.onArrival ? "as they arrive" : "at the display's tick"))\n"
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
            if soundOn {
                s += "\nsound buffer \(f(soundBufferMs, 0)) ms · concealed \(soundConcealed) · left out \(soundDropped)"
                #if os(iOS)
                    // What the device itself adds after the buffer, and where the sound goes.
                    let a = AVAudioSession.sharedInstance()
                    let route = a.currentRoute.outputs.map { $0.portType.rawValue }.joined(separator: ",")
                    s += "\nsound out \(route) · output \(f(a.outputLatency * 1000, 0)) ms · I/O \(f(a.ioBufferDuration * 1000, 1)) ms · \(wifiPausing ? "deep" : "shallow")"
                #endif
            }
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
