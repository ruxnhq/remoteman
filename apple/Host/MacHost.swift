// The Mac as a remoteman host: for the client that connects, a virtual display of its size,
// scale and refresh rate, captured with ScreenCaptureKit, encoded as HEVC Main10 and handed
// to the core, which owns the connection. Input from the client goes into the desktop, and
// the cursor is reported apart from the picture when the client asks.
// Adapted from espalier's ScreenController.
import AppKit
import CoreMedia
import CryptoKit
import Foundation
import IOKit.pwr_mgt
import RemotemanCore
import ScreenCaptureKit

/// Where the log goes besides stdout: an app started from Finder or `open` has no terminal.
let hostLogFile: FileHandle? = {
    // At the login window launchd already sends what is printed to the agent's own log.
    if CommandLine.arguments.contains("--login-window") { return nil }
    // A host started for a test (`--log file`) keeps its lines out of the everyday host's log.
    let args = CommandLine.arguments
    let own = args.firstIndex(of: "--log").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
    let url = own.map { URL(fileURLWithPath: $0) } ?? FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Logs/remoteman-host.log")
    if !FileManager.default.fileExists(atPath: url.path) { FileManager.default.createFile(atPath: url.path, contents: nil) }
    let h = try? FileHandle(forWritingTo: url)
    _ = try? h?.seekToEnd()
    return h
}()
private let hostLogLock = NSLock()
private let hostLogDate: DateFormatter = {
    let f = DateFormatter()
    f.dateFormat = "yyyy-MM-dd HH:mm:ss.SSS"
    return f
}()

func hostLog(_ s: String) {
    hostLogLock.lock()
    defer { hostLogLock.unlock() }
    let line = "\(hostLogDate.string(from: Date())) \(s)\n"
    print(line, terminator: "")
    hostLogFile?.write(Data(line.utf8))
}

final class MacHost: NSObject, SCStreamOutput, SCStreamDelegate {
    struct Options {
        var listen = "0.0.0.0:7791"
        var name = Host.current().localizedName ?? "Mac"
        var stateDir = ""
        var pair = false
        /// Make the virtual display the main one (menu bar and new windows) while a client is on.
        var makeMain = false
        /// What ScreenCaptureKit delivers: "x420" (10-bit YCbCr, what the encoder takes as it is),
        /// "l10r" (10-bit RGB) or "420v" (8-bit YCbCr); the last two cost the encoder a conversion.
        var capture = "x420"
        /// How many frames may be inside the encoder at once.
        var inFlight = 2
        /// Capture no faster than the encoder manages (off: capture at the rate asked, drop what is too much).
        var pace = false
        var bitrate = 0
        /// Fill the virtual display with moving test content while a client is on.
        var testWindow = false
        var testPen = false
        /// A still picture is sent once more, sharper (off: `--refine no`).
        var refine = true
        /// Let the core lower and raise the bitrate as the network loses or doesn't (off: `--follow-loss no`).
        var followLoss = true
        /// Offer this Mac's sound (--sound no leaves it out).
        var sound = true
        /// Silence this Mac's own speakers while a device plays its sound, so that it is heard in
        /// one place, as on the Linux host. The owner can turn that off in Settings.
        var silenceSpeakers = true
        var testTone = false
        /// What this Mac's owner allows paired devices: the clipboard both ways, files into Downloads.
        var allowClipboard = false
        var allowFiles = false
        /// Say on the network that this Mac can be reached (DNS-SD). Off until the owner turns it on.
        var advertise = false
        /// A new device waits until this Mac's owner has compared the code and said yes.
        var confirm = false
        /// Running in the login window's session (LoginWindow.swift): the picture is the login
        /// window, so the device's display is always made the main one.
        var loginWindow = false
        /// Where a device's display may sit (0 extra, 1 main, 2 only), and where it goes when
        /// it asks for a place that isn't allowed.
        var allowedPlacements: Set<Int> = [0, 1, 2]
        var defaultPlacement = 0
        /// Lock this Mac when the device leaves.
        var lockOnLeave = false
        /// 10 bits a colour (the picture as the Mac draws it); off, 8 bits, which every device decodes.
        var tenBit = true
        /// HDR: an HDR virtual display, captured as HDR10 (BT.2020 primaries, the PQ curve) and
        /// said to be so in the stream. For now a switch for the whole host (`--hdr yes`), until a
        /// device can ask for it in its hello.
        var hdr = false
        /// For tests: capture this Mac's own main screen, as the login window's host does.
        var ownScreen = false
        /// For tests: so many seconds after a device starts waiting, say yes (negative: say no).
        var testDecide: Double?
        var testAllow: Double?
        var testDrop: Double?
        var testRevoke: Double?
        /// What happens when a device connects while another is using this Mac: 0 it takes over, 1 it watches, 2 it can control too.
        var secondDevice = 0
        /// For tests: turn the second factor on at start and log its secret.
        var secondFactorTest = false
        /// For tests: make the first device that connects view-only, and log the connection log when sharing stops.
        var testViewOnly = false
        var logConnections = false
    }

    /// The device that is connected, as the interface shows it.
    struct Client {
        var name: String
        var width: Int
        var height: Int
        var refresh: Int
        /// 0 an extra display, 1 the main display, 2 the only display.
        var placement: Int
        var since = Date()
    }

    /// For the interface, on the main queue: the device that connected (nil: it left), and
    /// once a second while a picture is going out, the frames and bits sent in that second.
    var onClient: ((Client?) -> Void)?
    var onStats: ((_ fps: Int, _ bitsPerSecond: Int) -> Void)?
    /// A device was paired, connected or refused (the core's notice, as JSON), on the core's thread.
    var onNotice: (([String: Any]) -> Void)?

    private var options: Options

    /// What this host offers now: the camera and the microphone only while remoteman's own are installed on this Mac.
    private var featureList: String {
        "video,input,cursor,placement,configure,mirror,displays,pen" + (MacHost.hdrCapture && options.tenBit ? ",hdr" : "") + (options.sound ? ",sound" : "") + (options.allowClipboard ? ",clipboard" : "")
            + (options.allowFiles ? ",files" : "") + (HostCamera.installed ? ",camera" : "") + (HostMicrophone.installed ? ",mic" : "")
    }
    private var announced = ""

    /// Tell the core (and through it a connected device) what is offered, if that changed.
    /// Nothing restarts; what was switched off is stopped here.
    func refreshFeatures() {
        guard let host else { return }
        let list = featureList
        guard list != announced else { return }
        announced = list
        rm_host_set_features(host, list)
        hostLog("host: offering \(list)")
        if !options.allowClipboard, let s = sharing { DispatchQueue.main.async { s.setClipboard(false, sendCurrent: false) } }
    }

    /// What paired devices may do, as the owner switched it.
    func allow(clipboard: Bool, files: Bool) {
        options.allowClipboard = clipboard
        options.allowFiles = files
        refreshFeatures()
    }
    private var host: OpaquePointer?

    var isRunning: Bool { host != nil }

    /// "address:port" this host listens on, and its key's fingerprint.
    var info: (address: String, fingerprint: String)? {
        guard let h = host else { return nil }
        var info = [CChar](repeating: 0, count: 256)
        rm_host_info(h, &info, info.count)
        let parts = String(cString: info).split(separator: "\t").map(String.init)
        return (parts.first ?? options.listen, parts.count > 1 ? parts[1] : "")
    }

    /// Pairing mode: the next unknown device is let in and remembered, and the core then turns
    /// the mode off by itself.
    var pairing: Bool {
        get { host.map { rm_host_pairing($0) != 0 } ?? false }
        set { if let h = host { rm_host_set_pairing(h, newValue ? 1 : 0) } }
    }
    /// A new device that is waiting for the owner's yes, and the code both sides show.
    struct Pending: Equatable {
        var name: String
        var fingerprint: String
        var code: String
        var seconds: Int
    }

    /// Hold a new device until the owner confirms it (off: it is trusted as it connects).
    func setConfirm(_ on: Bool) {
        options.confirm = on
        if let h = host { rm_host_set_confirm(h, on ? 1 : 0) }
    }

    var pending: Pending? {
        guard let h = host else { return nil }
        var out = [CChar](repeating: 0, count: 1024)
        guard rm_host_pending(h, &out, out.count) > 0,
            let o = try? JSONSerialization.jsonObject(with: Data(String(cString: out).utf8)) as? [String: Any],
            let code = o["code"] as? String
        else { return nil }
        return Pending(
            name: o["name"] as? String ?? "A device", fingerprint: o["fingerprint"] as? String ?? "", code: code,
            seconds: (o["seconds"] as? NSNumber)?.intValue ?? 0)
    }

    /// The owner's answer for the device that is waiting. True if one was.
    @discardableResult
    func decide(_ accept: Bool) -> Bool { host.map { rm_host_decide($0, accept ? 1 : 0) != 0 } ?? false }

    /// A device this Mac trusts.
    struct Device: Identifiable, Equatable {
        var fingerprint: String
        var name: String
        var connected: Bool
        var id: String { fingerprint }
    }

    /// The trusted devices, in the order they were paired.
    var devices: [Device] {
        guard let h = host else { return [] }
        var buf = [CChar](repeating: 0, count: 8192)
        var n = rm_host_devices(h, &buf, buf.count)
        if n >= buf.count {
            buf = [CChar](repeating: 0, count: n + 1)
            n = rm_host_devices(h, &buf, buf.count)
        }
        guard let list = (try? JSONSerialization.jsonObject(with: Data(String(cString: buf).utf8))) as? [[String: Any]] else { return [] }
        return list.map {
            Device(fingerprint: $0["fingerprint"] as? String ?? "", name: $0["name"] as? String ?? "", connected: $0["connected"] as? Bool ?? false)
        }
    }

    /// What one device may do (everything, until the owner says otherwise).
    struct Permissions: Equatable {
        var input = true
        var clipboard = true
        var files = true
        var sound = true
        var mic = true
        var camera = true
    }

    func permissions(for fingerprint: String) -> Permissions {
        var p = rm_permissions(input: 1, clipboard: 1, files: 1, sound: 1, mic: 1, camera: 1)
        if let h = host { _ = rm_host_get_permissions(h, fingerprint, &p) }
        return Permissions(input: p.input != 0, clipboard: p.clipboard != 0, files: p.files != 0, sound: p.sound != 0, mic: p.mic != 0, camera: p.camera != 0)
    }

    /// The device is told at once if it is connected. False if it couldn't be saved.
    @discardableResult
    func setPermissions(_ p: Permissions, for fingerprint: String) -> Bool {
        guard let h = host else { return false }
        var raw = rm_permissions(
            input: p.input ? 1 : 0, clipboard: p.clipboard ? 1 : 0, files: p.files ? 1 : 0, sound: p.sound ? 1 : 0,
            mic: p.mic ? 1 : 0, camera: p.camera ? 1 : 0)
        return rm_host_set_permissions(h, fingerprint, &raw) == 1
    }

    /// The last `max` lines of the connection log, oldest first: time, event (paired, connected,
    /// left, refused), name, fingerprint, address, duration, reason.
    func connections(_ max: Int) -> [[String: Any]] {
        guard let h = host, let raw = rm_host_connections_json(h, max) else { return [] }
        defer { rm_string_free(raw) }
        return (try? JSONSerialization.jsonObject(with: Data(String(cString: raw).utf8))) as? [[String: Any]] ?? []
    }

    /// RFC 6238 (SHA-1, 30 s, six digits), for the test above only.
    private static func totp(base32: String, at date: Date) -> String {
        let alphabet = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")
        var bits = 0, value = 0
        var key = [UInt8]()
        for ch in base32.uppercased() {
            guard let i = alphabet.firstIndex(of: ch) else { continue }
            value = (value << 5) | i
            bits += 5
            if bits >= 8 {
                bits -= 8
                key.append(UInt8((value >> bits) & 0xff))
            }
        }
        var counter = UInt64(date.timeIntervalSince1970 / 30).bigEndian
        let mac = withUnsafeBytes(of: &counter) {
            Array(HMAC<Insecure.SHA1>.authenticationCode(for: Data($0), using: SymmetricKey(data: key)))
        }
        let o = Int(mac[19] & 0x0f)
        let n = (UInt32(mac[o] & 0x7f) << 24) | (UInt32(mac[o + 1]) << 16) | (UInt32(mac[o + 2]) << 8) | UInt32(mac[o + 3])
        return String(format: "%06d", n % 1_000_000)
    }

    func setSecondDevice(_ mode: Int) {
        options.secondDevice = mode
        if let h = host { rm_host_set_second_device(h, UInt8(clamping: mode)) }
    }

    /// Stop trusting a device (it is dropped if connected). 1 done, 0 it wasn't trusted, -1 the list couldn't be saved.
    func revoke(_ fingerprint: String) -> Int {
        guard let h = host else { return 0 }
        return Int(rm_host_revoke(h, fingerprint))
    }

    /// Drop the connected device and keep listening; it stays trusted.
    @discardableResult
    func dropClient() -> Bool { host.map { rm_host_drop_client($0) != 0 } ?? false }

    private let frames = DispatchQueue(label: "remoteman.host.frames", qos: .userInteractive)
    private let input = HostInput()

    // Main queue.
    private var display: RMVirtualDisplay?
    private var stream: SCStream?
    private var previousMain: CGDirectDisplayID?
    private var mirrored: [CGDirectDisplayID] = []
    private var awake: IOPMAssertionID = 0
    private var statsTimer: Timer?
    private var cursorTimer: Timer?
    private var generation = 0

    // Under `lock`.
    private let lock = NSLock()
    private var encoder: HostEncoder?
    private var lastBuffer: CVPixelBuffer?
    private var geometry: (bounds: CGRect, scale: CGFloat)?
    /// The device's second display, while it has one (SecondDisplay.swift).
    private let second = HostSecondDisplay()
    private var aimedAtSecond = false
    private var sent = 0, bytes = 0, encodeMs = 0.0, dropped = 0, keyframes = 0
    private var captureMs = 0.0, captured = 0, ageMs = 0.0
    private var askedFps = 60, saidSlow = false
    private var inputs = 0, inputMax = 0.0
    private var captureConfig: SCStreamConfiguration?, pacedFps = 0
    private var liveStream: SCStream?
    /// Static refinement: when the screen has been still this long, the last picture once more, sharper.
    private var lastCaptureAt = 0.0, refined = true, refineTimer: Timer?, refinements = 0
    private func refineIfStill() {
        lock.lock()
        let still = CACurrentMediaTime() - lastCaptureAt
        let go = options.refine && !refined && still >= 0.4 && lastBuffer != nil
        if go { refined = true; refinements += 1 }
        let enc = encoder, pb = lastBuffer, rate = followedRate > 0 ? followedRate : picture.map { bitrate(for: $0) } ?? 25_000_000
        lock.unlock()
        guard go, let enc, let pb else { return }
        frames.async {
            if let path = MacHost.sourceDump, !FileManager.default.fileExists(atPath: path) { MacHost.dumpLuma(pb, to: path) }
            enc.refine(pb, pts: CMClockGetTime(CMClockGetHostTimeClock()), bitrate: rate)
        }
    }
    /// For measuring (`--dump-source file`): the luma of the first picture refined, raw 16-bit.
    static let sourceDump: String? = {
        let a = CommandLine.arguments
        return a.firstIndex(of: "--dump-source").flatMap { $0 + 1 < a.count ? a[$0 + 1] : nil }
    }()
    static func dumpLuma(_ pb: CVPixelBuffer, to path: String) {
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddressOfPlane(pb, 0) else { return }
        let w = CVPixelBufferGetWidthOfPlane(pb, 0), h = CVPixelBufferGetHeightOfPlane(pb, 0), row = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
        var out = Data(capacity: w * h * 2)
        for y in 0..<h { out.append(Data(bytes: base + y * row, count: w * 2)) }
        try? out.write(to: URL(fileURLWithPath: path))
        hostLog("host: source luma \(w)×\(h) written for measuring")
    }
    /// For checking the stream with other decoders (`--dump file.hevc`): every frame as encoded.
    private let dumpFile: FileHandle? = {
        let args = CommandLine.arguments
        guard let path = args.firstIndex(of: "--dump").flatMap({ $0 + 1 < args.count ? args[$0 + 1] : nil }) else { return nil }
        FileManager.default.createFile(atPath: path, contents: nil)
        return FileHandle(forWritingAtPath: path)
    }()
    private var displayMax = (3840, 2880), shownPlacement = 0, shownHDR = false
    private var sendMs = 0.0, sendMax = 0.0, largest = 0, keysSent = 0, captureGap = 0.0, lastCaptured = 0.0, refused = 0

    init(options: Options) {
        self.options = options
        super.init()
        input.display = { [weak self] in
            guard let self else { return nil }
            self.lock.lock()
            defer { self.lock.unlock() }
            // Pointer and touch events land on whichever display the device last said they are on.
            return self.aimedAtSecond ? (self.second.geometry ?? self.geometry) : self.geometry
        }
        second.send = { [weak self] o in self?.sendSecond(o) }
    }

    // MARK: Start and stop

    func start() throws {
        let callbacks = rm_host_callbacks(
            ctx: Unmanaged.passUnretained(self).toOpaque(),
            on_client: { ctx, client, accept in
                let me = Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue()
                return me.clientArrived(client!.pointee, accept!)
            },
            on_keyframe: { ctx in Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().keyframeWanted() },
            on_input: { ctx, input in
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().inputArrived(input!.pointee)
            },
            on_client_left: { ctx in Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().clientLeft() },
            on_message: { ctx, json in
                guard let json, let m = (try? JSONSerialization.jsonObject(with: Data(String(cString: json).utf8))) as? [String: Any]
                else { return }
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().message(m)
            },
            on_mic: { ctx, data, len, start, seq, _ in
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().microphone?.packet(UnsafeRawBufferPointer(start: data, count: len), start: start != 0, seq: seq)
            },
            on_camera: { ctx, data, len, key, _, _ in
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().camera?.frame(UnsafeRawBufferPointer(start: data, count: len), key: key != 0)
            },
            on_blob: { ctx, what, id, kind, header, data, len, complete in
                let me = Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue()
                let h = header.flatMap { (try? JSONSerialization.jsonObject(with: Data(String(cString: $0).utf8))) as? [String: Any] } ?? [:]
                me.blob(what: what, id: id, kind: kind, header: h, data: UnsafeRawBufferPointer(start: data, count: data == nil ? 0 : len), complete: complete != 0)
            })
        var err = [CChar](repeating: 0, count: 512)
        guard
            let h = rm_host_start(
                options.listen, options.stateDir, options.name, "macos", featureList, options.pair ? 1 : 0,
                callbacks, &err, err.count)
        else { throw NSError(domain: "remoteman", code: 1, userInfo: [NSLocalizedDescriptionKey: String(cString: err)]) }
        host = h
        announced = featureList
        var info = [CChar](repeating: 0, count: 256)
        rm_host_info(h, &info, info.count)
        let parts = String(cString: info).split(separator: "\t").map(String.init)
        hostLog("host: listening on \(parts.first ?? options.listen), fingerprint \(parts.count > 1 ? parts[1] : "?"), pairing \(rm_host_pairing(h) != 0 ? "on" : "off")")
        listening = (parts.first ?? options.listen, parts.count > 1 ? parts[1] : "")
        if options.confirm { rm_host_set_confirm(h, 1) }
        rm_host_set_second_device(h, UInt8(clamping: options.secondDevice))
        hostLog("host: when a second device connects: \(["it takes over", "it watches", "it can control too"][min(max(options.secondDevice, 0), 2)])")
        rm_host_set_notice_callback(h, { ctx, json in
            guard let json, let m = (try? JSONSerialization.jsonObject(with: Data(String(cString: json).utf8))) as? [String: Any] else { return }
            let me = Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue()
            hostLog("host: notice: \(m["event"] as? String ?? "?") \(m["name"] as? String ?? "?") \((m["fingerprint"] as? String ?? "").prefix(8)) \(m["address"] as? String ?? "")\(m["reason"].map { " (\($0))" } ?? "")")
            me.onNotice?(m)
        }, Unmanaged.passUnretained(self).toOpaque())
        if options.secondFactorTest {
            _ = rm_host_disable_second_factor(h)  // a new secret each start
            var out = [CChar](repeating: 0, count: 512)
            let r = rm_host_enable_second_factor(h, &out, out.count)
            let parts = String(cString: out).split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
            hostLog("host: second factor test: enable \(r), secret \(parts.first ?? "") uri \(parts.count > 2 ? parts[2] : "")")
            // A secret counts once a right code has been given: do that here, so that devices are asked.
            if r == 0, let secret = parts.first {
                let ok = rm_host_confirm_second_factor(h, Self.totp(base32: secret, at: Date()))
                hostLog("host: second factor test: confirmed \(ok), devices are asked for a code now")
            }
        }
        // The core follows loss: it lowers the rate when the device reports losses and raises it
        // again while all is well, up to the device's ceiling or this; the encoder takes it live.
        if options.followLoss {
            rm_host_set_bitrate_callback(h, 40_000_000, { ctx, bits in
                Unmanaged<MacHost>.fromOpaque(ctx!).takeUnretainedValue().followRate(Int(bits))
            }, Unmanaged.passUnretained(self).toOpaque())
        }
        advertise(options.advertise)
    }

    /// The core's thread: the rate the network allows now.
    private func followRate(_ bits: Int) {
        lock.lock()
        let enc = encoder
        let changed = abs(bits - followedRate) * 10 > followedRate
        followedRate = bits
        lock.unlock()
        enc?.setBitrate(bits)
        if changed { hostLog("host: bitrate following the network: \(bits / 1000) kbit/s") }
    }
    private var followedRate = 0

    private var listening = ("", "")
    /// The device is shown this Mac's own screen as it is (it asked to mirror).
    private var mirroring = false
    private var loggingIn = false
    private var logInFailure: String?
    private let advertiser = Advertise()

    /// Show this Mac to devices on the network, or stop showing it.
    func advertise(_ on: Bool) {
        options.advertise = on
        guard on, host != nil else {
            if advertiser.on { hostLog("host: no longer shown on the network") }
            advertiser.stop()
            return
        }
        let at = listening.0
        let port = UInt16(at.split(separator: ":").last ?? "") ?? 7791
        if at.hasPrefix("127.") || at.hasPrefix("[::1]") {
            hostLog("host: not shown on the network: it listens on this Mac only")
            return
        }
        if let why = advertiser.start(name: options.name, port: port, fingerprint: listening.1) {
            hostLog("host: couldn't be shown on the network: \(why)")
        } else {
            hostLog("host: shown on the network as \"\(options.name)\" (_remoteman._udp, port \(port), cards \(Advertise.macs.joined(separator: ",")))")
        }
    }

    func stop() {
        advertiser.stop()
        soundQueue.sync { speakers.restore() }
        teardown()
        // `host` stays set until the core has stopped: its last callbacks (the client leaving) use it.
        if let h = host {
            if options.logConnections { hostLog("host: connection log: \(connections(20))") }
            rm_host_stop(h)
            host = nil
        }
    }

    // MARK: Core callbacks (the core's thread; they may block)

    private func clientArrived(_ c: rm_client_info, _ accept: UnsafeMutablePointer<rm_accept>) -> Int32 {
        let name = c.name.map { String(cString: $0) } ?? "?"
        let decoders = (c.decoders.map { String(cString: $0) } ?? "").split(separator: ",").map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        hostLog("host: client \"\(name)\" \(c.width)×\(c.height) scale \(c.scale) at \(c.refresh) Hz, decoders \(decoders.joined(separator: ",")), cursor apart \(c.cursor_apart != 0)")
        func refuse(_ why: String) -> Int32 {
            hostLog("host: refused: \(why)")
            withUnsafeMutableBytes(of: &accept.pointee.refuse) { buf in
                let bytes = Array(why.utf8.prefix(buf.count - 1))
                buf.copyBytes(from: bytes)
                buf[bytes.count] = 0
            }
            return 0
        }
        if options.loginWindow {
            // Nothing is shown from here: the owner is logged in for the device, which then
            // connects again to the owner's own host. (A viewer tries again by itself on "logging in:".)
            // The device is told at once to come back shortly, and the log in is done after
            // that answer has gone: once someone is logged in this agent is stopped, and an
            // answer still on its way then would reach the device only as "the host stopped".
            // One attempt at a time; a failed one is said to the device on its next try.
            if let why = logInFailure {
                logInFailure = nil
                return refuse("this Mac is at its login window and couldn't log in for the device: \(why)")
            }
            if !loggingIn {
                loggingIn = true
                hostLog("host: login window: \(name) connected; logging in for it")
                DispatchQueue.global().asyncAfter(deadline: .now() + 0.4) { [weak self] in
                    let why = Unattended.logInAtLoginWindow()
                    if let why { hostLog("host: login window: \(why)") } else { hostLog("host: login window: logged in for \(name)") }
                    self?.logInFailure = why
                    self?.loggingIn = false
                }
            }
            return refuse("logging in: this Mac is logging in for you")
        }
        if Unattended.screenLocked, Unattended.enabled, CommandLine.arguments.contains("--no-unlock") == false {
            // Locked: unlocked for the paired device, which never sees the lock screen.
            if let why = Unattended.submit() {
                hostLog("host: locked, and couldn't unlock for the device: \(why). The kept password \(Unattended.keptIsRight).")
            } else {
                hostLog("host: locked: unlocked for \(name)")
            }
        }
        guard decoders.contains("hevc") else { return refuse("this host sends HEVC 4:2:0 and the client doesn't decode it") }
        guard RMVirtualDisplay.isSupported() else { return refuse("this macOS has no virtual displays") }
        if !CGPreflightScreenCaptureAccess() {
            // At the login window the question can't be asked or answered: try all the same.
            guard options.loginWindow else {
                return refuse("the Mac host needs Screen Recording permission (System Settings, Privacy & Security)")
            }
            hostLog("host: Screen Recording isn't reported as granted here; trying all the same")
        }
        // Where the client wants its display: 0 extra, 1 main, 2 only. --main yes still forces main.
        var p = atLoginWindow(Picture(
            width: Int(c.width), height: Int(c.height), fps: Int(c.refresh), hiDPI: c.scale >= 1.5, cursorApart: c.cursor_apart != 0,
            placement: c.placement == 3 ? 1 : placed(Int(c.placement))), mirror: c.placement == 3)
        p.hdr = c.hdr != 0 && c.placement != 3
        if p.hdr { hostLog("host: the device's screen shows HDR, headroom \(c.headroom)") }
        mirroring = c.placement == 3
        limit = 0
        if let failure = begin(p) { return refuse(failure) }
        clientName = name
        picture = p
        let s = Sharing(client: link, announces: false)
        s.transferChanged = { t in
            if t.state != .moving { hostLog("host: file \(t.incoming ? "from" : "to") the device: \(t.name), \(t.size) bytes: \(t.state)") }
        }
        sharing = s
        fill(accept, p)
        let summary = Client(name: name, width: p.width, height: p.height, refresh: p.fps, placement: mirroring ? 3 : min(p.placement, 2))
        DispatchQueue.main.async { self.onClient?(summary) }
        return 1
    }

    /// What a client's display is: its pixels, rate, whether the desktop is drawn at 2×, and where it sits.
    private struct Picture: Equatable {
        var width: Int, height: Int, fps: Int
        /// macOS draws at 1× or 2×: anything from 1.5 up gets the 2× desktop.
        var hiDPI: Bool
        var cursorApart: Bool
        var placement: Int
        /// The device's screen can show HDR and asked for it.
        var hdr = false
        /// The Mac's own main screen is captured; no display is made.
        var ownScreen = false
        init(width: Int, height: Int, fps: Int, hiDPI: Bool, cursorApart: Bool, placement: Int) {
            self.width = min(7680, max(640, width & ~1))
            self.height = min(4320, max(360, height & ~1))
            self.fps = min(240, max(24, fps))
            self.hiDPI = hiDPI
            self.cursorApart = cursorApart
            self.placement = min(max(placement, 0), 2)
        }
    }
    // The core's thread.
    private var picture: Picture?
    private var clientName = ""
    /// The ceiling the client asked for, in bits a second (0: this host chooses).
    private var limit = 0

    private func bitrate(for p: Picture) -> Int {
        let own = options.bitrate > 0 ? options.bitrate : (p.width * p.height >= 3840 * 2160 ? 40_000_000 : 25_000_000)
        // A device's ceiling is taken up to 300 Mbit/s: it may ask for more than this host would
        // choose by itself, and the encoder uses what the picture needs under it.
        return limit > 0 ? max(500_000, min(limit, 300_000_000)) : own
    }

    private func fill(_ accept: UnsafeMutablePointer<rm_accept>, _ p: Picture) {
        accept.pointee.codec = 0
        accept.pointee.depth = options.tenBit ? 10 : 8
        accept.pointee.chroma = 420
        accept.pointee.width = UInt32(p.width)
        accept.pointee.height = UInt32(p.height)
        accept.pointee.refresh = UInt32(p.fps)
        accept.pointee.hdr = hdrNow(p) ? 1 : 0
        // macOS puts SDR white at 100 nits in an HDR capture for a canonical display (a patch at
        // four times white read back as 397 nits), and a virtual HDR display has headroom 5.
        accept.pointee.sdr_white = hdrNow(p) ? 100 : 0
        accept.pointee.peak = hdrNow(p) ? 500 : 0
    }

    /// HDR needs 10 bits and a display of this host's making (not the Mac's own screen, not a mirror).
    private func hdrNow(_ p: Picture) -> Bool { MacHost.hdrCapture && (options.hdr || p.hdr) && options.tenBit && !p.ownScreen }
    /// ScreenCaptureKit captures HDR from macOS 15.
    static var hdrCapture: Bool { if #available(macOS 15.0, *) { return true } else { return false } }

    /// Take down whatever picture there is and start this one. Blocks the core's thread until
    /// it runs; returns why not, if it doesn't.
    /// At the login window the picture is the Mac's own screen as it is, not a display made for
    /// the device: the login window is only dependably on a screen the Mac already has.
    private func atLoginWindow(_ p: Picture, mirror: Bool = false) -> Picture {
        guard mirror || options.loginWindow || options.ownScreen, let m = CGDisplayCopyDisplayMode(CGMainDisplayID()), m.pixelWidth >= 640, m.pixelHeight >= 360 else { return p }
        var p = Picture(
            width: m.pixelWidth, height: m.pixelHeight, fps: m.refreshRate >= 24 ? Int(m.refreshRate.rounded()) : 60, hiDPI: m.pixelWidth > m.width,
            cursorApart: p.cursorApart, placement: 1)
        p.ownScreen = true
        return p
    }

    /// The place a device gets when it asks for `asked`: that one if the owner allows it,
    /// otherwise the owner's default (or the first allowed).
    private func placed(_ asked: Int) -> Int {
        let wanted = max(min(asked, 2), options.makeMain ? 1 : 0)
        if options.allowedPlacements.contains(wanted) { return wanted }
        if options.allowedPlacements.contains(options.defaultPlacement) { return options.defaultPlacement }
        return options.allowedPlacements.min() ?? 0
    }

    func setPolicy(allowed: Set<Int>, fallback: Int) {
        options.allowedPlacements = allowed.isEmpty ? [0] : allowed
        options.defaultPlacement = fallback
    }

    func setLockOnLeave(_ on: Bool) { options.lockOnLeave = on }
    /// From the next connection (or the next change of display).
    func setTenBit(_ on: Bool) { options.tenBit = on }

    /// The most this Mac encodes at, in bits a second (0: it chooses), from now on.
    func setOwnBitrate(_ bits: Int) {
        options.bitrate = max(0, bits)
        guard let p = picture else { return }
        let rate = bitrate(for: p)
        lock.lock()
        let enc = encoder
        lock.unlock()
        enc?.setBitrate(rate)
        if let host { rm_host_send_message(host, "{\"t\":\"status\",\"bitrate\":\(rate)}") }
        hostLog("host: this Mac's own ceiling \(options.bitrate), encoding at \(rate / 1000) kbit/s")
    }

    /// `keep`: change the display there is in place if that is all the change needs.
    private func begin(_ p: Picture, keep: Bool = false) -> String? {
        let done = DispatchSemaphore(value: 0)
        var failure: String? = "starting the display timed out"
        let rate = bitrate(for: p)
        DispatchQueue.main.async { [self] in
            // Only the size, the refresh rate or 1×/2× differ: the same display takes the new mode,
            // so it stays where it is (and stays main) and the windows on it stay on it.
            let inPlace = keep && display != nil && !p.ownScreen && p.placement == shownPlacement && p.placement < 2
                && p.hdr == shownHDR
                && p.width <= displayMax.0 && p.height <= displayMax.1
            teardown(keepDisplay: inPlace)
            generation += 1
            let gen = generation
            Task { @MainActor in
                failure = await self.setUp(
                    width: p.width, height: p.height, fps: p.fps, hiDPI: p.hiDPI, cursorApart: p.cursorApart, placement: p.placement,
                    bitrate: rate, gen: gen, ownScreen: p.ownScreen, inPlace: inPlace, wantsHDR: p.hdr)
                done.signal()
            }
        }
        if done.wait(timeout: .now() + 15) == .timedOut { failure = "starting the display timed out" }
        if failure != nil { DispatchQueue.main.async { self.teardown() } }
        return failure
    }

    // MARK: Clipboard and files

    /// The connected device as clipboard and files reach it.
    private final class Link: SharingLink {
        weak var owner: MacHost?
        init(_ owner: MacHost) { self.owner = owner }
        func send(_ message: [String: Any]) -> Bool {
            guard let h = owner?.host, let d = try? JSONSerialization.data(withJSONObject: message), let json = String(data: d, encoding: .utf8)
            else { return false }
            return rm_host_send_message(h, json) != 0
        }
        func sendBlob(kind: UInt8, header: [String: Any], next: () -> Data?) -> Bool {
            guard let h = owner?.host, let d = try? JSONSerialization.data(withJSONObject: header), let json = String(data: d, encoding: .utf8),
                let blob = rm_host_blob_open(h, kind, json)
            else { return false }
            while let piece = next() {
                if piece.isEmpty { continue }
                if piece.withUnsafeBytes({ rm_blob_write(blob, $0.bindMemory(to: UInt8.self).baseAddress, piece.count) }) == 0 {
                    rm_blob_cancel(blob)
                    return false
                }
            }
            return rm_blob_finish(blob) != 0
        }
    }
    private lazy var link = Link(self)
    /// For one client, from its hello to its leaving. Set on the core's thread.
    private var sharing: Sharing?

    private func blob(what: UInt8, id: UInt32, kind: UInt8, header: [String: Any], data: UnsafeRawBufferPointer, complete: Bool) {
        guard let sharing else { return }
        // Only what the owner allowed is taken; the rest is read and thrown away.
        if kind == BlobKind.clipboard, !options.allowClipboard { return }
        if kind == BlobKind.file, !options.allowFiles {
            if what == 0, let fid = header["id"] { link.send(["t": "file_result", "id": fid, "ok": false, "message": "This Mac doesn't take files."]) }
            return
        }
        sharing.blob(what: what, id: id, kind: kind, header: header, data: data, complete: complete)
    }

    // MARK: The device's microphone

    /// On the core's thread: there while the device's microphone is on.
    private var microphone: HostMicrophone?

    private func setMicrophone(_ m: [String: Any]) {
        if let mic = microphone {
            let t = mic.totals
            hostLog("host: the device's microphone is off: packets=\(t.packets) concealed=\(t.concealed) seconds=\(String(format: "%.2f", Double(t.frames) / 48000)) refused=\(t.refused)")
            mic.stop()
            microphone = nil
        }
        guard m["on"] as? Bool == true else { return }
        guard (m["codec"] as? String ?? "opus") == "opus", (m["channels"] as? NSNumber)?.intValue ?? 1 == 1 else {
            return hostLog("host: the device's microphone sends a form this Mac doesn't take")
        }
        guard let mic = HostMicrophone() else { return hostLog("host: remoteman's microphone isn't installed on this Mac") }
        microphone = mic
        hostLog("host: the device's microphone is on this Mac as \"remoteman Microphone\"")
    }

    // MARK: The device's camera

    /// On the core's thread: there while the device's camera is on.
    private var camera: HostCamera?

    private func setCamera(_ m: [String: Any]) {
        guard let host else { return }
        if let c = camera {
            let t = c.totals
            hostLog("host: the device's camera is off: frames=\(t.frames) keyframes=\(t.keyframes) shown=\(t.shown) undecodable=\(t.undecodable)")
            c.stop()
            camera = nil
        }
        guard m["on"] as? Bool == true else { return }
        guard (m["codec"] as? String ?? "h264") == "h264" else { return hostLog("host: the device's camera sends a codec this Mac doesn't take") }
        let c = HostCamera()
        c.wantKeyframe = { [weak self] in
            if let h = self?.host { rm_host_send_message(h, "{\"t\":\"camera_keyframe\"}") }
        }
        if let why = c.start() { return hostLog("host: the device's camera: \(why)") }
        camera = c
        hostLog("host: the device's camera is on this Mac as \"\(HostCamera.deviceName)\"")
        _ = host
    }

    // MARK: Sound

    private let soundQueue = DispatchQueue(label: "remoteman.host.sound", qos: .userInteractive)
    /// On the sound queue: there while the client wants this Mac's sound.
    private var sound: HostSound?

    private let speakers = HostSpeakers()

    /// Whether the Mac's speakers are silenced while a device listens; takes effect at once.
    func setSilenceSpeakers(_ on: Bool) {
        options.silenceSpeakers = on
        soundQueue.sync {
            guard sound != nil else { return }
            if on { hostLog("host: this Mac's speakers: \(speakers.silence())") } else { speakers.restore() }
        }
    }

    private func setSound(_ on: Bool) {
        guard let host else { return }
        soundQueue.sync {
            if !on { speakers.restore() }
            if let s = sound, !on {
                hostLog("host: sound off after \(s.buffers) buffers captured, \(s.totals.packets) packets, \(s.totals.bytes) bytes, \(s.totals.pauses) pauses; \(s.gapReport)")
            }
            sound = nil
            guard on else { return }
            guard let s = HostSound() else { return hostLog("host: no Opus encoder for sound") }
            s.send = { [weak self] data, start, seq, behind in
                guard let h = self?.host else { return }
                let now = rm_host_now_us(h)
                rm_host_send_sound(h, data.bindMemory(to: UInt8.self).baseAddress, data.count, start ? 1 : 0, seq, now > behind ? now - behind : 0)
            }
            s.testTone = options.testTone
            sound = s
            rm_host_send_message(host, HostSound.format)
            let quiet = options.silenceSpeakers ? speakers.silence() : "left playing"
            hostLog("host: sound on for the device; this Mac's own speakers: \(quiet)")
        }
    }

    /// One of the client's feature messages: configure, sound.
    private func message(_ m: [String: Any]) {
        if m["t"] as? String == "sound" { return setSound(m["on"] as? Bool ?? false) }
        if m["t"] as? String == "camera" { return setCamera(m) }
        if m["t"] as? String == "mic" { return setMicrophone(m) }
        if m["t"] as? String == "clipboard", options.allowClipboard, let sharing {
            let on = m["on"] as? Bool ?? false
            hostLog("host: the clipboard is \(on ? "shared with the device" : "no longer shared")")
            return DispatchQueue.main.async { sharing.setClipboard(on, sendCurrent: true) }
        }
        if m["t"] as? String == "file_result" { sharing?.fileResult(m) }
        if m["t"] as? String == "display_keyframe" { return second.keyframe() }
        if m["t"] as? String == "display", (m["index"] as? NSNumber)?.intValue == 1 { return setSecond(m) }
        guard m["t"] as? String == "configure", let host, let old = picture else { return }
        if let b = (m["bitrate"] as? NSNumber)?.intValue {
            limit = max(0, b)
            let rate = bitrate(for: old)
            lock.lock()
            let enc = encoder
            lock.unlock()
            enc?.setBitrate(rate)
            hostLog("host: configure: bitrate ceiling \(limit), encoding at \(rate / 1000) kbit/s")
            rm_host_send_message(host, "{\"t\":\"status\",\"bitrate\":\(rate)}")
        }
        guard let d = m["display"] as? [String: Any] else { return }
        func int(_ k: String, _ now: Int) -> Int { (d[k] as? NSNumber)?.intValue ?? now }
        var placement = old.placement
        if let s = d["placement"] as? String {
            mirroring = s == "mirror"
            placement = mirroring ? 1 : placed(["extend": 0, "main": 1, "only": 2][s] ?? placement)
        }
        var new = atLoginWindow(Picture(
            width: int("width", old.width), height: int("height", old.height), fps: int("refresh", old.fps),
            hiDPI: (d["scale"] as? NSNumber).map { $0.doubleValue >= 1.5 } ?? old.hiDPI, cursorApart: old.cursorApart,
            placement: max(placement, options.makeMain ? 1 : 0)), mirror: mirroring)
        // A display without the word is an SDR one, as in the hello.
        new.hdr = (d["hdr"] as? Bool ?? false) && !mirroring
        guard new != old else { return }
        hostLog("host: configure: display \(new.width)×\(new.height) at \(new.fps) Hz, 2× \(new.hiDPI), placement \(new.placement)")
        var now = new
        if let failure = begin(new, keep: !old.ownScreen) {
            // Back to what worked; if that fails too, the client is dropped.
            hostLog("host: configure failed (\(failure)); going back to \(old.width)×\(old.height)")
            now = old
            if let again = begin(old) {
                hostLog("host: and the old display didn't come back (\(again)); dropping the client")
                rm_host_drop_client(host)
                return
            }
        }
        picture = now
        var accept = rm_accept()
        fill(&accept, now)
        if rm_host_restart(host, &accept) == 0 { hostLog("host: configure: the core didn't restart the stream") }
        // The first frame after a restart has to be a whole one, and the new encoder's first may
        // have gone out before the restart.
        wholeFrameNow()
        rm_host_send_message(host, "{\"t\":\"status\",\"bitrate\":\(bitrate(for: now))}")
        // What was on stays on across the new picture, and the device hears the format again.
        soundQueue.sync { if sound != nil { rm_host_send_message(host, HostSound.format) } }
        let summary = Client(name: clientName, width: now.width, height: now.height, refresh: now.fps, placement: mirroring ? 3 : now.placement)
        DispatchQueue.main.async { self.onClient?(summary) }
    }

    private func keyframeWanted() {
        lock.lock()
        keyframes += 1
        lock.unlock()
        wholeFrameNow()
    }

    private func wholeFrameNow() {
        lock.lock()
        let enc = encoder, last = lastBuffer
        lock.unlock()
        enc?.requestKeyframe()
        // A still screen sends no frames; encode the last one so the keyframe goes out.
        if let enc, let last { frames.async { enc.encode(last, pts: CMClockGetTime(CMClockGetHostTimeClock())) } }
    }

    /// The device asks for a second display, for another size of it, or gives it back.
    private func setSecond(_ m: [String: Any]) {
        guard let host else { return }
        guard m["on"] as? Bool == true, let d = m["display"] as? [String: Any] else {
            lock.lock()
            aimedAtSecond = false
            lock.unlock()
            Task { @MainActor in await self.second.stop() }
            return
        }
        func int(_ k: String, _ otherwise: Int) -> Int { (d[k] as? NSNumber)?.intValue ?? otherwise }
        let width = min(7680, max(640, int("width", 1920) & ~1)), height = min(4320, max(360, int("height", 1080) & ~1))
        let fps = min(240, max(24, int("refresh", 60)))
        let hiDPI = ((d["scale"] as? NSNumber)?.doubleValue ?? 1) >= 1.5
        let tenBit = options.tenBit
        // Which side of the others it sits on; left out, the right.
        let side = m["side"] as? String ?? ""
        Task { @MainActor in
            if let why = await self.second.start(width: width, height: height, fps: fps, hiDPI: hiDPI, tenBit: tenBit, side: side) {
                hostLog("host: no second display: \(why)")
                if why != "replaced" { rm_host_second_stop(host, why) }
                return
            }
            var accept = rm_accept()
            accept.codec = 0
            accept.depth = tenBit ? 10 : 8
            accept.chroma = 420
            accept.width = UInt32(width)
            accept.height = UInt32(height)
            accept.refresh = UInt32(fps)
            accept.hdr = 0
            self.secondAnnounced = rm_host_second_start(host, &accept) != 0
            // The picture starts whole.
            self.second.keyframe()
        }
    }
    private var secondAnnounced = false

    private func sendSecond(_ o: HostEncoder.Output) {
        guard let host, secondAnnounced else { return }
        let age = CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()) - o.pts)
        let now = rm_host_now_us(host)
        guard now != 0 else { return }
        let capture = now &- UInt64(max(0, age) * 1_000_000)
        _ = o.annexB.withUnsafeBytes {
            rm_host_send_frame2(host, $0.bindMemory(to: UInt8.self).baseAddress, o.annexB.count, o.keyframe ? 1 : 0, capture)
        }
    }

    private func inputArrived(_ i: rm_input) {
        let began = CACurrentMediaTime()
        defer {
            let took = (CACurrentMediaTime() - began) * 1000
            lock.lock()
            inputs += 1
            inputMax = max(inputMax, took)
            lock.unlock()
        }
        if i.kind == 8 {
            lock.lock()
            aimedAtSecond = i.slot == 1
            lock.unlock()
            return
        }
        switch i.kind {
        case 1: input.handle(.pointerTo(i.x, i.y))
        case 2: input.handle(.pointerBy(i.x, i.y))
        case 3: input.handle(.button(i.button, i.down != 0))
        case 4: input.handle(.scroll(i.x, i.y))
        case 5: input.handle(.key(i.usage, i.down != 0))
        case 6: input.handle(.touch(phase: i.phase, slot: i.slot, i.x, i.y))
        case 7: if let t = i.text { input.handle(.text(String(cString: t))) }
        case 9:
            input.penAt(
                phase: i.phase, buttons: i.button, x: i.x, y: i.y, pressure: i.pressure, tiltX: i.tilt_x, tiltY: i.tilt_y, twist: i.twist)
        default: break
        }
    }

    private func clientLeft() {
        hostLog("host: client left")
        secondAnnounced = false
        lock.lock()
        aimedAtSecond = false
        lock.unlock()
        Task { @MainActor in await self.second.stop() }
        if options.lockOnLeave, !options.loginWindow {
            hostLog("host: locking this Mac, as set")
            if let lib = dlopen("/System/Library/PrivateFrameworks/login.framework/login", RTLD_NOW), let sym = dlsym(lib, "SACLockScreenImmediate") {
                typealias Lock = @convention(c) () -> Int32
                _ = unsafeBitCast(sym, to: Lock.self)()
            }
        }
        picture = nil
        setCamera(["on": false])
        setMicrophone(["on": false])
        soundQueue.sync {
            sound = nil
            speakers.restore()
        }
        if let s = sharing {
            sharing = nil
            DispatchQueue.main.async {
                for line in s.log { hostLog("host: clipboard: \(line)") }
                s.stop()
            }
        }
        DispatchQueue.main.async {
            self.teardown()
            self.onClient?(nil)
        }
    }

    // MARK: Display and capture (main queue)

    /// An HDR mode is one with a transfer function (1: the system then treats the display as an
    /// HDR screen with headroom); an ordinary one goes the ordinary way. 
    private func applyMode(_ d: RMVirtualDisplay, width: Int, height: Int, fps: Int, hiDPI: Bool, hdr: Bool) -> Bool {
        if hdr { return d.applyHDRWidth(UInt32(width), height: UInt32(height), refreshRate: Double(fps), hiDPI: hiDPI) }
        return d.applyWidth(UInt32(width), height: UInt32(height), refreshRate: Double(fps), hiDPI: hiDPI)
    }

    /// Returns nil when the picture is running, or why it isn't.
    @MainActor
    private func setUp(
        width: Int, height: Int, fps: Int, hiDPI: Bool, cursorApart: Bool, placement: Int, bitrate: Int, gen: Int, ownScreen: Bool = false,
        inPlace: Bool = false, wantsHDR: Bool = false
    ) async -> String? {
        let hdr = MacHost.hdrCapture && (options.hdr || wantsHDR) && options.tenBit && !ownScreen
        holdAwake()
        let id: CGDirectDisplayID
        if inPlace, let display {
            guard applyMode(display, width: width, height: height, fps: fps, hiDPI: hiDPI, hdr: hdr) else {
                return "the virtual display refused \(width)×\(height) at \(fps) Hz"
            }
            id = display.displayID
            await settle(id, width: width, height: height, hiDPI: hiDPI)
            if let m = CGDisplayCopyDisplayMode(id) {
                hostLog("host: display \(id) changed in place: \(m.pixelWidth)×\(m.pixelHeight) px, \(m.width)×\(m.height) pt, \(m.refreshRate) Hz")
            }
        } else if ownScreen {
            id = CGMainDisplayID()
            hostLog("host: this Mac's own screen \(id), \(width)×\(height) px at \(fps) Hz, as it is")
        } else {
            // A fixed serial first; one left behind by a crashed run can't be shared, so then a new one.
            for serial in [UInt32(7791), UInt32.random(in: 10_000...60_000)] {
                displayMax = (max(width, 3840), max(height, 2880))
                display = RMVirtualDisplay(
                    name: "remoteman", maxWidth: UInt32(displayMax.0), maxHeight: UInt32(displayMax.1), serial: serial)
                if let d = display, d.displayID != 0 { break }
                display = nil
            }
            guard let display else { return "couldn't create a virtual display" }
            guard applyMode(display, width: width, height: height, fps: fps, hiDPI: hiDPI, hdr: hdr) else {
                return "the virtual display refused \(width)×\(height) at \(fps) Hz"
            }
            id = display.displayID
            await settle(id, width: width, height: height, hiDPI: hiDPI)
            shownPlacement = placement
            shownHDR = wantsHDR
            if let m = CGDisplayCopyDisplayMode(id) {
                hostLog("host: display \(id): \(m.pixelWidth)×\(m.pixelHeight) px, \(m.width)×\(m.height) pt, \(m.refreshRate) Hz")
            }
            if placement >= 1, CGMainDisplayID() != id {
                previousMain = CGMainDisplayID()
                MacHost.makeMain(id)
            }
            if placement == 2 { mirrored = MacHost.mirrorOthers(onto: id) }
            hostLog("host: placement \(["an extra display", "the main display", "the only display"][min(placement, 2)])")
        }
        // A new display takes a moment to show up in ScreenCaptureKit.
        var found: SCDisplay?
        for _ in 0..<30 {
            guard gen == generation else { return "replaced by a newer client" }
            HostInput.wake()
            if let d = try? await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
                .displays.first(where: { $0.displayID == id }), d.width > 0
            {
                found = d
                break
            }
            try? await Task.sleep(nanoseconds: 150_000_000)
        }
        guard let d = found else { return "the virtual display didn't appear for capture" }
        guard gen == generation else { return "replaced by a newer client" }

        let enc: HostEncoder
        do { enc = try HostEncoder(width: width, height: height, fps: fps, bitrate: bitrate, tenBit: options.tenBit, hdr: hdr) } catch {
            return "no HEVC Main10 hardware encoder for \(width)×\(height) (\(error.localizedDescription))"
        }
        enc.maxInFlight = max(1, options.inFlight)
        enc.queue = frames
        enc.onFrame = { [weak self] o in self?.send(o) }

        let config = captureConfiguration(width: width, height: height, cursorApart: cursorApart, hdr: hdr)
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.queueDepth = 3
        captureConfig = config
        pacedFps = fps
        let s = SCStream(filter: SCContentFilter(display: d, excludingWindows: []), configuration: config, delegate: self)
        lock.lock()
        liveStream = s
        lock.unlock()
        do {
            try s.addStreamOutput(self, type: .screen, sampleHandlerQueue: frames)
            if options.sound { try s.addStreamOutput(self, type: .audio, sampleHandlerQueue: soundQueue) }
            try await s.startCapture()
        } catch {
            return "screen capture failed: \(error.localizedDescription)"
        }
        guard gen == generation else {
            try? await s.stopCapture()
            return "replaced by a newer client"
        }
        let b = CGDisplayBounds(id)
        lock.lock()
        encoder = enc
        lastBuffer = nil
        geometry = (b, b.width > 0 ? CGFloat(width) / b.width : 1)
        sent = 0; bytes = 0; encodeMs = 0; dropped = 0
        askedFps = fps
        saidSlow = false
        lock.unlock()
        stream = s
        statsTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in self?.stats() }
        refineTimer?.invalidate()
        refineTimer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in self?.refineIfStill() }
        if cursorApart { startCursor() }
        if options.testWindow { testWindow.show(on: id) }
        // For testing the pen without a device that sends one: a stroke across the middle, pressing
        // harder and tilting as it goes.
        if options.testPen {
            for i in 0...40 {
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5 + Double(i) * 0.02) { [self] in
                    // Where the display is now: a new display is moved beside the others after it appears.
                    let b = CGDisplayBounds(id)
                    let f = Float(i) / 40
                    let p = CGPoint(x: b.minX + b.width * CGFloat(0.3 + 0.4 * f), y: b.midY)
                    input.pen(phase: i == 0 ? 0 : i == 40 ? 2 : 1, buttons: 0, at: p, pressure: 0.1 + 0.8 * f, tiltX: 30 * f, tiltY: -20 * f, twist: 90 * f)
                    if i == 40 { input.pen(phase: 4, buttons: 0, at: p, pressure: 0, tiltX: 0, tiltY: 0, twist: 0) }
                }
            }
        }
        firstFrame(d, width: width, height: height, cursorApart: cursorApart, gen: gen)
        hostLog("host: sending \(width)×\(height) at \(fps) Hz, HEVC \(options.tenBit ? "Main10" : "Main (8-bit)") 4:2:0\(hdr ? " HDR (BT.2020, PQ)" : ""), \(bitrate / 1000) kbit/s, capture \(options.tenBit ? options.capture : "420v"), cursor \(cursorApart ? "apart" : "in the picture"), \(width * height * fps / 1_000_000) megapixels a second")
        return nil
    }

    private func captureConfiguration(width: Int, height: Int, cursorApart: Bool, hdr: Bool = false) -> SCStreamConfiguration {
        if hdr, #available(macOS 15.0, *) {
            // HDR10 as any HDR screen expects it ("canonical"), not as this Mac's own display would show it.
            let config = SCStreamConfiguration(preset: .captureHDRStreamCanonicalDisplay)
            config.width = width
            config.height = height
            config.pixelFormat = kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
            // What the stream says it is: BT.2020 primaries and matrix, the PQ curve (the preset
            // by itself gives P3 primaries and the BT.709 matrix).
            config.colorSpaceName = CGColorSpace.itur_2100_PQ
            config.colorMatrix = kCVImageBufferYCbCrMatrix_ITU_R_2020
            config.showsCursor = !cursorApart
            config.capturesAudio = options.sound
            config.sampleRate = 48000
            config.channelCount = 2
            config.excludesCurrentProcessAudio = true
            return config
        }
        let config = SCStreamConfiguration()
        config.width = width
        config.height = height
        config.pixelFormat =
            switch options.tenBit ? options.capture : "420v" {
            case "420v": kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            case "l10r": kCVPixelFormatType_ARGB2101010LEPacked
            default: kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
            }
        config.colorSpaceName = CGColorSpace.sRGB
        config.colorMatrix = CGDisplayStream.yCbCrMatrix_ITU_R_709_2
        config.showsCursor = !cursorApart
        // Everything this Mac plays, bar this app; listened to all the time, sent only on request.
        config.capturesAudio = options.sound
        config.sampleRate = 48000
        config.channelCount = 2
        config.excludesCurrentProcessAudio = true
        return config
    }

    /// ScreenCaptureKit sends nothing while the screen doesn't change, so without a frame soon
    /// after the start, a screenshot is encoded instead: the client always gets a picture.
    private func firstFrame(_ d: SCDisplay, width: Int, height: Int, cursorApart: Bool, gen: Int) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { [self] in
            lock.lock()
            let enc = encoder, have = lastBuffer != nil
            lock.unlock()
            guard gen == generation, let enc, !have, stream != nil else { return }
            let config = captureConfiguration(width: width, height: height, cursorApart: cursorApart)
            SCScreenshotManager.captureSampleBuffer(
                contentFilter: SCContentFilter(display: d, excludingWindows: []), configuration: config
            ) { [weak self] sb, error in
                guard let self, let pb = sb?.imageBuffer else {
                    return hostLog("host: no frame yet, and no screenshot either: \(error?.localizedDescription ?? "empty")")
                }
                self.lock.lock()
                let current = self.encoder === enc && self.lastBuffer == nil
                if current { self.lastBuffer = pb }
                self.lock.unlock()
                guard current else { return }
                hostLog("host: no frame yet; sent a screenshot")
                enc.requestKeyframe()
                self.frames.async { _ = enc.encode(pb, pts: CMClockGetTime(CMClockGetHostTimeClock())) }
            }
        }
    }

    private let testWindow = TestWindow()

    private func teardown(keepDisplay: Bool = false) {
        generation += 1
        // A display changed in place keeps its test window, which follows the new size, so the
        // desktop behind it never shows in between.
        if !keepDisplay { testWindow.close() }
        statsTimer?.invalidate()
        statsTimer = nil
        refineTimer?.invalidate()
        refineTimer = nil
        cursorTimer?.invalidate()
        cursorTimer = nil
        input.release()
        lock.lock()
        let enc = encoder
        encoder = nil
        lastBuffer = nil
        geometry = nil
        liveStream = nil
        lock.unlock()
        enc?.invalidate()
        if let s = stream {
            stream = nil
            Task { try? await s.stopCapture() }
        }
        if keepDisplay { return }
        if !mirrored.isEmpty {
            MacHost.unmirror(mirrored)
            mirrored = []
        }
        if let prev = previousMain, prev != CGMainDisplayID() {
            var ids = [CGDirectDisplayID](repeating: 0, count: 16)
            var n: UInt32 = 0
            CGGetOnlineDisplayList(16, &ids, &n)
            if ids.prefix(Int(n)).contains(prev) { MacHost.makeMain(prev) }
        }
        previousMain = nil
        if display != nil {
            display = nil  // the display goes with the object
            hostLog("host: display removed")
        }
        if awake != 0 {
            IOPMAssertionRelease(awake)
            awake = 0
        }
    }

    private func holdAwake() {
        if awake == 0 {
            IOPMAssertionCreateWithName(
                kIOPMAssertionTypePreventUserIdleDisplaySleep as CFString, IOPMAssertionLevel(kIOPMAssertionLevelOn),
                "remoteman host session" as CFString, &awake)
        }
        HostInput.wake()
    }

    /// Wait until the display has the mode last applied. Given a mode, macOS often settles on
    /// its 1× twin even when HiDPI was asked for, so the 2× mode is then picked by hand.
    @MainActor
    private func settle(_ id: CGDirectDisplayID, width: Int, height: Int, hiDPI: Bool) async {
        let points = hiDPI ? width / 2 : width
        func right() -> Bool {
            guard let m = CGDisplayCopyDisplayMode(id) else { return false }
            return m.pixelWidth == width && m.pixelHeight == height && m.width == points
        }
        for attempt in 0..<30 {
            if right() { return }
            if attempt >= 3, attempt % 3 == 0 {
                let opts = [kCGDisplayShowDuplicateLowResolutionModes: kCFBooleanTrue] as CFDictionary
                let modes = (CGDisplayCopyAllDisplayModes(id, opts) as? [CGDisplayMode]) ?? []
                if let m = modes.first(where: { $0.pixelWidth == width && $0.pixelHeight == height && $0.width == points }) {
                    CGDisplaySetDisplayMode(id, m, nil)
                }
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }
        hostLog("host: display \(id): asked for \(width)×\(height)\(hiDPI ? " HiDPI" : ""), still in another mode")
    }

    /// "Only display": macOS can't switch a screen off for a session, so every other display is
    /// made a mirror of the client's instead; there is then one desktop, and it is the client's.
    /// Returns the displays that were changed, to be put back when the client leaves.
    static func mirrorOthers(onto id: CGDirectDisplayID) -> [CGDirectDisplayID] {
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        let others = ids.prefix(Int(n)).filter { $0 != id && CGDisplayMirrorsDisplay($0) == kCGNullDirectDisplay }
        guard !others.isEmpty else { return [] }
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return [] }
        for o in others { CGConfigureDisplayMirrorOfDisplay(config, o, id) }
        guard CGCompleteDisplayConfiguration(config, .forSession) == .success else { return [] }
        hostLog("host: displays \(others) now mirror display \(id)")
        return Array(others)
    }

    static func unmirror(_ displays: [CGDirectDisplayID]) {
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return }
        for o in displays { CGConfigureDisplayMirrorOfDisplay(config, o, kCGNullDirectDisplay) }
        CGCompleteDisplayConfiguration(config, .forSession)
        hostLog("host: displays \(displays) have their own picture again")
    }

    /// Main display = the one at the origin; the others keep their places relative to it.
    static func makeMain(_ id: CGDirectDisplayID) {
        guard CGMainDisplayID() != id else { return }
        var config: CGDisplayConfigRef?
        guard CGBeginDisplayConfiguration(&config) == .success, let config else { return }
        let b = CGDisplayBounds(id)
        var ids = [CGDirectDisplayID](repeating: 0, count: 16)
        var n: UInt32 = 0
        CGGetOnlineDisplayList(16, &ids, &n)
        for other in ids.prefix(Int(n)) {
            let o = CGDisplayBounds(other)
            CGConfigureDisplayOrigin(config, other, Int32(o.minX - b.minX), Int32(o.minY - b.minY))
        }
        CGCompleteDisplayConfiguration(config, .forSession)
        hostLog("host: display \(id) is now the main display")
    }

    // MARK: Frames (the frames queue, then VideoToolbox's thread)

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        if type == .audio { return sound?.take(sb) ?? () }  // on the sound queue
        guard type == .screen,
            let attachments = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false)
                as? [[SCStreamFrameInfo: Any]],
            let raw = (attachments.first?[.status] as? NSNumber)?.intValue, SCFrameStatus(rawValue: raw) == .complete,
            let pb = sb.imageBuffer
        else { return }
        lock.lock()
        // A stream that was stopped can still deliver for a moment (its size may be the old one).
        guard stream === liveStream else { return lock.unlock() }
        let enc = encoder
        let first = lastBuffer == nil
        lastBuffer = pb
        lastCaptureAt = CACurrentMediaTime()
        refined = false
        lock.unlock()
        if first {
            let f = CVPixelBufferGetPixelFormatType(pb)
            let cc = String(bytes: [24, 16, 8, 0].map { UInt8((f >> $0) & 0xff) }, encoding: .ascii) ?? "?"
            let colour = [kCVImageBufferColorPrimariesKey, kCVImageBufferTransferFunctionKey, kCVImageBufferYCbCrMatrixKey]
                .map { (CVBufferCopyAttachment(pb, $0, nil) as? String) ?? "-" }.joined(separator: " / ")
            hostLog("host: capture delivers \(CVPixelBufferGetWidth(pb))×\(CVPixelBufferGetHeight(pb)) '\(cc)', \(colour)")
        }
        // The timestamp is the time the frame was shown, on the host clock; how long capture took
        // to hand it over is counted for the log. A busy encoder drops the frame.
        let waited = CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()) - sb.presentationTimeStamp) * 1000
        let taken = enc?.encode(pb, pts: sb.presentationTimeStamp) ?? true
        lock.lock()
        captureMs += waited
        captured += 1
        let at = CACurrentMediaTime()
        if lastCaptured > 0 { captureGap = max(captureGap, (at - lastCaptured) * 1000) }
        lastCaptured = at
        if !taken { dropped += 1 }
        lock.unlock()
    }

    private func send(_ o: HostEncoder.Output) {
        guard let host else { return }
        // The frame's age on this Mac's clock, taken off the session clock the client compares with.
        let age = CMTimeGetSeconds(CMClockGetTime(CMClockGetHostTimeClock()) - o.pts)
        let now = rm_host_now_us(host)
        guard now != 0 else { return }
        let capture = now &- UInt64(max(0, age) * 1_000_000)
        dumpFile?.write(o.annexB)
        let before = CACurrentMediaTime()
        let taken = o.annexB.withUnsafeBytes {
            rm_host_send_frame(host, $0.bindMemory(to: UInt8.self).baseAddress, o.annexB.count, o.keyframe ? 1 : 0, capture)
        }
        let inSend = (CACurrentMediaTime() - before) * 1000
        guard taken != 0 else {
            lock.lock()
            refused += 1
            let enc = encoder
            lock.unlock()
            // A whole frame the core didn't take has to be made again.
            if o.keyframe { enc?.requestKeyframe() }
            return
        }
        lock.lock()
        sendMs += inSend
        sendMax = max(sendMax, inSend)
        largest = max(largest, o.annexB.count)
        if o.keyframe { keysSent += 1 }
        sent += 1
        bytes += o.annexB.count
        encodeMs += o.encodeMs
        ageMs += max(0, age) * 1000
        lock.unlock()
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        hostLog("host: capture stopped: \(error.localizedDescription)")
        // The device would keep a frozen picture: end the session, so that it sees it ended.
        lock.lock()
        let live = stream === liveStream
        lock.unlock()
        if live, let host {
            hostLog("host: dropping the device, its picture has stopped")
            rm_host_drop_client(host)
        }
    }

    private func stats() {
        lock.lock()
        let counts = encoder?.takeCounts() ?? (late: 0, stale: 0)
        let queueFigures = input.takeQueueFigures()
        let line = sent > 0 || dropped > 0 || inputs > 0
            ? String(format: "host: %d fps, %d kbit/s, capture %.2f ms, encode %.2f ms, %.2f ms old when sent, send %.2f ms (longest %.2f), largest frame %d bytes, %d keyframes sent, longest wait for the screen %.0f ms, %d dropped (busy), %d of them sent late, %d times left stale, %d not taken by the core, %d keyframe requests, %d input events (longest %.2f ms), input queue wait at most %.2f ms, %d moves merged", sent, bytes * 8 / 1000, captureMs / Double(max(captured, 1)), encodeMs / Double(max(sent, 1)), ageMs / Double(max(sent, 1)), sendMs / Double(max(sent, 1)), sendMax, largest, keysSent, captureGap, dropped, counts.late, counts.stale, refused, keyframes, inputs, inputMax, queueFigures.longestWaitMs, queueFigures.merged)
            : nil
        // Asked for more than the encoder does in a frame time: said once a session.
        let frameMs = 1000 / Double(max(askedFps, 1)), took = encodeMs / Double(max(sent, 1))
        let slow = !saidSlow && sent >= 20 && dropped > 0 && took > frameMs
        if slow { saidSlow = true }
        let fps = sent, bits = bytes * 8, wasDropped = dropped
        // With `--pace yes`: capture no faster than the encoder gets through, so frames come
        // evenly at half (or a third, a quarter) of the rate asked instead of unevenly at more.
        var paceTo = 0
        if slow, options.pace {
            for k in 2...4 where paceTo == 0 { if 1000 * Double(k) / Double(askedFps) >= took * 0.9 { paceTo = askedFps / k } }
            if paceTo == 0 { paceTo = askedFps / 4 }
        }
        defer {
            if paceTo > 0 { pace(to: paceTo, encodeMs: took) }
            if slow {
                hostLog(String(format: "host: the device asked for more than this Mac encodes in a frame time: %.1f ms a frame at this size against %.1f ms at %d Hz, so frames are being dropped (%d sent, %d dropped in the last second). A smaller picture would be quicker.", took, frameMs, askedFps, fps, wasDropped))
            }
        }
        sent = 0; bytes = 0; encodeMs = 0; dropped = 0; captureMs = 0; captured = 0; ageMs = 0; sendMs = 0; sendMax = 0; largest = 0; keysSent = 0; captureGap = 0; refused = 0; inputs = 0; inputMax = 0; keyframes = 0
        lock.unlock()
        if let line { hostLog(line) }
        onStats?(fps, bits)
    }

    /// Capture at this rate from now on, and tell the device (status "refresh").
    private func pace(to fps: Int, encodeMs: Double) {
        guard let stream, let config = captureConfig, let host, fps != pacedFps else { return }
        pacedFps = fps
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        stream.updateConfiguration(config) { error in
            if let error { hostLog("host: pacing capture failed: \(error.localizedDescription)") }
        }
        hostLog(String(format: "host: capturing at %d Hz from now on: the encoder takes %.1f ms a frame at this size", fps, encodeMs))
        rm_host_send_message(host, "{\"t\":\"status\",\"refresh\":\(fps)}")
    }

    // MARK: Cursor (main queue)

    private var cursorAt: (Float, Float, Bool)?
    private var cursorPixels: [UInt8]?
    private var cursorTick = 0

    private func startCursor() {
        cursorAt = nil
        cursorPixels = nil
        cursorTick = 0
        let t = Timer(timeInterval: 1.0 / 120, repeats: true) { [weak self] _ in self?.cursorPoll() }
        RunLoop.main.add(t, forMode: .common)
        cursorTimer = t
    }

    private func cursorPoll() {
        guard let host else { return }
        lock.lock()
        let g = geometry
        lock.unlock()
        guard let (b, scale) = g, let p = CGEvent(source: nil)?.location else { return }
        let visible = b.contains(p)
        let at = (Float((p.x - b.minX) / b.width), Float((p.y - b.minY) / b.height), visible)
        if cursorAt == nil || cursorAt! != at {
            // In the log: every time the pointer goes off the device's display or comes back.
            if cursorAt == nil || cursorAt!.2 != visible {
                hostLog(String(format: "host: cursor %@ the device's display (at %.0f, %.0f; the display is %.0f, %.0f, %.0f×%.0f)", visible ? "on" : "off", p.x, p.y, b.minX, b.minY, b.width, b.height))
            }
            cursorAt = at
            rm_host_cursor_at(host, at.0, at.1, visible ? 1 : 0)
        }
        // The shape changes far less often than the position: look at it 20 times a second.
        cursorTick += 1
        guard cursorTick % 6 == 1, let c = NSCursor.currentSystem,
            let cg = c.image.cgImage(forProposedRect: nil, context: nil, hints: nil)
        else { return }
        let w = max(1, Int((c.image.size.width * scale).rounded())), h = max(1, Int((c.image.size.height * scale).rounded()))
        var pixels = [UInt8](repeating: 0, count: w * h * 4)
        let drawn = pixels.withUnsafeMutableBytes { buf -> Bool in
            guard
                let ctx = CGContext(
                    data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
                    space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
            else { return false }
            ctx.interpolationQuality = .high
            ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drawn, cursorPixels == nil || cursorPixels! != pixels else { return }
        cursorPixels = pixels
        // In the log: each shape sent (premultiplied RGBA, 8 bits, sRGB), with what it is made of.
        var opaque = 0, clear = 0, sum = (0, 0, 0), hash: UInt32 = 2166136261
        for i in stride(from: 0, to: pixels.count, by: 4) {
            let a = pixels[i + 3]
            if a == 0 { clear += 1 } else if a == 255 { opaque += 1 }
            sum.0 += Int(pixels[i]); sum.1 += Int(pixels[i + 1]); sum.2 += Int(pixels[i + 2])
        }
        for v in pixels { hash = (hash ^ UInt32(v)) &* 16777619 }
        let n = max(1, w * h - clear)
        hostLog("host: cursor shape \(w)×\(h), hot spot \(Int((c.hotSpot.x * scale).rounded())),\(Int((c.hotSpot.y * scale).rounded())), \(opaque) opaque and \(clear) clear of \(w * h) pixels, mean colour where drawn \(sum.0 / n),\(sum.1 / n),\(sum.2 / n), id \(String(hash, radix: 16))")
        rm_host_cursor_shape(
            host, pixels, UInt32(w), UInt32(h), Int32((c.hotSpot.x * scale).rounded()), Int32((c.hotSpot.y * scale).rounded()))
    }
}
