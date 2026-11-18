// The computer's sound on this device, and this device's microphone on the computer.
// Both are Opus at 48 kHz in 10 ms packets through the system's codec (AVAudioConverter), which
// conceals a lost packet when handed an empty one in its place.
import AVFoundation
import AudioToolbox

private func opusFormat(channels: UInt32, framesPerPacket: UInt32) -> AVAudioFormat? {
    var d = AudioStreamBasicDescription(
        mSampleRate: 48000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0,
        mFramesPerPacket: framesPerPacket, mBytesPerFrame: 0, mChannelsPerFrame: channels, mBitsPerChannel: 0, mReserved: 0)
    return AVAudioFormat(streamDescription: &d)
}

#if os(iOS)
    /// The app's audio session: playback, and recording too while the microphone is on.
    enum AudioSession {
        private static var playing = false
        private static var recording = false
        static func set(playing p: Bool? = nil, recording r: Bool? = nil) {
            if let p { playing = p }
            if let r { recording = r }
            let s = AVAudioSession.sharedInstance()
            do {
                if recording {
                    try s.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .allowBluetoothA2DP])
                } else if playing {
                    try s.setCategory(.playback, mode: .default)
                }
                try s.setPreferredIOBufferDuration(0.005)
                try s.setActive(playing || recording, options: .notifyOthersOnDeactivation)
            } catch {
                NSLog("audio session: \(error)")
            }
        }
    }
#endif

/// Plays the sound packets as they arrive, through a short buffer.
final class SoundPlayer {
    struct Counters {
        var packets = 0
        var concealed = 0
        var dropped = 0
        var underruns = 0
        var queuedMs = 0.0
    }

    private let lock = NSLock()
    private var engine: AVAudioEngine?
    private var node: AVAudioPlayerNode?
    private var decoder: AVAudioConverter?
    private var opus: AVAudioFormat?
    private var pcm: AVAudioFormat?
    private var framesPerPacket: UInt32 = 480
    private var nextSeq: UInt32?
    private var queued = 0  // frames handed to the node and not played yet
    private var counters = Counters()
    /// Over the whole time since `start`: packets, bytes, concealed, left out, times run dry, starts marked.
    private(set) var totals = (packets: 0, bytes: 0, concealed: 0, dropped: 0, underruns: 0, starts: 0)
    private(set) var format = ""
    /// For tests: the decoded sound's level and pitch (upward zero crossings of the left channel).
    private var crossings = 0, measured = 0, squares = 0.0, lastSample: Float = 0
    var tone: String {
        lock.lock()
        defer { lock.unlock() }
        guard measured > 0 else { return "nothing decoded" }
        return String(
            format: "%.2f s decoded, %.1f Hz by zero crossings, level %.4f rms", Double(measured) / 48000,
            Double(crossings) * 48000 / Double(measured), (squares / Double(measured)).squareRoot())
    }
    /// A deeper buffer, for a link that pauses at intervals (the iPad's Wi-Fi does, see
    /// ViewerSession): what a pause holds back then plays instead of being cut.
    var deep = false
    /// For tests on one Mac, where the host would hear the viewer: decode and count, play nothing.
    static var silent = false

    /// Frames kept ahead when starting or after running dry, and the most that may wait.
    private var prime: Int { deep ? 4800 : 1200 }  // 100 ms or 25 ms
    private var most: Int { deep ? 8160 : 3360 }  // 170 ms or 70 ms

    var running: Bool {
        lock.lock()
        defer { lock.unlock() }
        return decoder != nil
    }

    /// Set up for the format the host announced. Returns why not, if it can't be played.
    @discardableResult
    func start(channels: Int, frameUs: Int) -> String? {
        stop()
        let fpp = UInt32(max(120, 48 * frameUs / 1000))
        guard let pcm = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: UInt32(channels), interleaved: false),
            let opus = opusFormat(channels: UInt32(channels), framesPerPacket: fpp), let decoder = AVAudioConverter(from: opus, to: pcm)
        else { return "This device has no decoder for the computer's sound." }
        var engine: AVAudioEngine?
        var node: AVAudioPlayerNode?
        if !Self.silent {
            #if os(iOS)
                AudioSession.set(playing: true)
            #endif
            let e = AVAudioEngine()
            let n = AVAudioPlayerNode()
            e.attach(n)
            e.connect(n, to: e.mainMixerNode, format: pcm)
            do { try e.start() } catch { return "The sound output couldn't be started: \(error.localizedDescription)" }
            n.play()
            engine = e
            node = n
        }
        lock.lock()
        self.engine = engine
        self.node = node
        self.decoder = decoder
        self.opus = opus
        self.pcm = pcm
        framesPerPacket = fpp
        totals = (0, 0, 0, 0, 0, 0)
        crossings = 0
        measured = 0
        squares = 0
        lastSample = 0
        format = "opus 48000 Hz, \(channels) channels, \(frameUs / 1000) ms"
        nextSeq = nil
        queued = 0
        lock.unlock()
        return nil
    }

    func stop() {
        lock.lock()
        let e = engine, n = node
        engine = nil
        node = nil
        decoder = nil
        nextSeq = nil
        queued = 0
        lock.unlock()
        n?.stop()
        e?.stop()
        #if os(iOS)
            if e != nil { AudioSession.set(playing: false) }
        #endif
    }

    func takeCounters() -> Counters {
        lock.lock()
        defer { lock.unlock() }
        var c = counters
        c.queuedMs = Double(queued) / 48
        counters = Counters()
        return c
    }

    /// One packet from the core (its thread).
    func packet(_ data: UnsafeRawBufferPointer, start: Bool, seq: UInt32) {
        lock.lock()
        defer { lock.unlock() }
        guard decoder != nil else { return }
        counters.packets += 1
        totals.packets += 1
        totals.bytes += data.count
        if start { totals.starts += 1 }
        if start {
            decoder?.reset()
        } else if let expected = nextSeq {
            let missing = Int(seq &- expected)
            if missing < 0 || missing > 1 << 30 { return }  // late or doubled: already concealed
            // A few lost packets are concealed; a long hole is played as the pause it was.
            if missing > 0, missing <= 5 {
                for _ in 0..<missing {
                    counters.concealed += 1
                    totals.concealed += 1
                    decode(nil)
                }
            } else if missing > 5 {
                decoder?.reset()
            }
        }
        nextSeq = seq &+ 1
        decode(data)
    }

    /// Decode one packet (nil: a lost one) and queue what comes out. Call with the lock held.
    private func decode(_ data: UnsafeRawBufferPointer?) {
        guard let decoder, let opus, let pcm else { return }
        let size = data?.count ?? 0
        let packet = AVAudioCompressedBuffer(format: opus, packetCapacity: 1, maximumPacketSize: max(size, 1))
        if let data, size > 0 { packet.data.copyMemory(from: data.baseAddress!, byteCount: size) }
        packet.byteLength = UInt32(size)
        packet.packetCount = 1
        packet.packetDescriptions?[0] = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(size))
        guard let out = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: framesPerPacket * 4) else { return }
        var fed = false
        var err: NSError?
        let status = decoder.convert(to: out, error: &err) { _, s in
            if fed {
                s.pointee = .noDataNow
                return nil
            }
            fed = true
            s.pointee = .haveData
            return packet
        }
        guard status != .error, out.frameLength > 0 else { return }
        if let p = out.floatChannelData?[0] {
            for i in 0..<Int(out.frameLength) {
                if lastSample <= 0, p[i] > 0 { crossings += 1 }
                lastSample = p[i]
                squares += Double(p[i] * p[i])
            }
            measured += Int(out.frameLength)
        }
        guard let node else { return }  // silent: decoded and counted, not played
        if queued > most {
            // More is waiting than should be (a burst after a pause, or the clocks drifting):
            // leave this one out, so the delay comes back down.
            counters.dropped += 1
            totals.dropped += 1
            return
        }
        if queued <= 0 {
            // Ran dry (or just started): put a little silence ahead, so the next packets have
            // time to arrive before the sound catches up with them.
            counters.underruns += 1
            totals.underruns += 1
            if let silence = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: AVAudioFrameCount(prime)) {
                silence.frameLength = AVAudioFrameCount(prime)
                schedule(silence, on: node)
            }
        }
        schedule(out, on: node)
    }

    private func schedule(_ buffer: AVAudioPCMBuffer, on node: AVAudioPlayerNode) {
        let frames = Int(buffer.frameLength)
        queued += frames
        node.scheduleBuffer(buffer, completionCallbackType: .dataConsumed) { [weak self, weak node] _ in
            guard let self else { return }
            self.lock.lock()
            if self.node === node { self.queued = max(0, self.queued - frames) }
            self.lock.unlock()
        }
    }
}

/// For tests: a quiet 440 Hz tone from this device's own output, for so many seconds.
enum TestTone {
    private static var engine: AVAudioEngine?
    static func play(seconds: Double) -> String {
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: 2, interleaved: false)!
        let n = AVAudioFrameCount(48000 * seconds)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: n) else { return "no buffer" }
        buffer.frameLength = n
        for c in 0..<2 {
            let p = buffer.floatChannelData![c]
            for i in 0..<Int(n) { p[i] = 0.05 * Float(sin(2 * Double.pi * 440 * Double(i) / 48000)) }
        }
        let e = AVAudioEngine(), node = AVAudioPlayerNode()
        e.attach(node)
        e.connect(node, to: e.mainMixerNode, format: format)
        do { try e.start() } catch { return "the output couldn't be started: \(error.localizedDescription)" }
        node.scheduleBuffer(buffer)
        node.play()
        engine = e
        return "playing 440 Hz for \(seconds) s"
    }
}

/// Sends this device's microphone: mono Opus, 10 ms packets.
final class MicSender {
    private let engine = AVAudioEngine()
    private var resampler: AVAudioConverter?
    private var encoder: AVAudioConverter?
    private var mono: AVAudioFormat?
    private var opus: AVAudioFormat?
    private var pending: [Float] = []
    private var seq: UInt32 = 0
    private var first = true
    private let began = DispatchTime.now().uptimeNanoseconds
    private let queue = DispatchQueue(label: "remoteman.mic")
    private(set) var running = false
    var send: ((UnsafeRawBufferPointer, _ start: Bool, _ seq: UInt32, _ timeUs: UInt64) -> Void)?

    static func allowed(_ then: @escaping (Bool) -> Void) {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: then(true)
        case .notDetermined: AVCaptureDevice.requestAccess(for: .audio) { ok in DispatchQueue.main.async { then(ok) } }
        default: then(false)
        }
    }

    /// Returns why not, if the microphone can't be started.
    func start() -> String? {
        guard !running else { return nil }
        #if os(iOS)
            AudioSession.set(recording: true)
        #endif
        let input = engine.inputNode
        let native = input.outputFormat(forBus: 0)
        guard native.sampleRate > 0, native.channelCount > 0,
            let mono = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: 1, interleaved: false),
            let opus = opusFormat(channels: 1, framesPerPacket: 480), let resampler = AVAudioConverter(from: native, to: mono),
            let encoder = AVAudioConverter(from: mono, to: opus)
        else { return "This device has no microphone to use." }
        encoder.bitRate = 32000
        self.mono = mono
        self.opus = opus
        self.resampler = resampler
        self.encoder = encoder
        first = true
        pending.removeAll()
        input.installTap(onBus: 0, bufferSize: 480, format: native) { [weak self] buffer, _ in
            self?.queue.async { self?.take(buffer) }
        }
        do { try engine.start() } catch {
            input.removeTap(onBus: 0)
            #if os(iOS)
                AudioSession.set(recording: false)
            #endif
            return "The microphone couldn't be started: \(error.localizedDescription)"
        }
        running = true
        return nil
    }

    func stop() {
        guard running else { return }
        running = false
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        #if os(iOS)
            AudioSession.set(recording: false)
        #endif
    }

    private func take(_ buffer: AVAudioPCMBuffer) {
        guard running, let resampler, let mono else { return }
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * 48000 / buffer.format.sampleRate) + 64
        guard let out = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: capacity) else { return }
        var fed = false
        var err: NSError?
        let status = resampler.convert(to: out, error: &err) { _, s in
            if fed {
                s.pointee = .noDataNow
                return nil
            }
            fed = true
            s.pointee = .haveData
            return buffer
        }
        guard status != .error, out.frameLength > 0, let samples = out.floatChannelData?[0] else { return }
        pending.append(contentsOf: UnsafeBufferPointer(start: samples, count: Int(out.frameLength)))
        while pending.count >= 480 {
            encode(Array(pending.prefix(480)))
            pending.removeFirst(480)
        }
    }

    private func encode(_ samples: [Float]) {
        guard let encoder, let mono, let opus, let pcm = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: 480) else { return }
        pcm.frameLength = 480
        samples.withUnsafeBufferPointer { pcm.floatChannelData![0].update(from: $0.baseAddress!, count: 480) }
        let out = AVAudioCompressedBuffer(format: opus, packetCapacity: 4, maximumPacketSize: max(1500, encoder.maximumOutputPacketSize))
        var fed = false
        var err: NSError?
        let status = encoder.convert(to: out, error: &err) { _, s in
            if fed {
                s.pointee = .noDataNow
                return nil
            }
            fed = true
            s.pointee = .haveData
            return pcm
        }
        guard status != .error, let descriptions = out.packetDescriptions else { return }
        let timeUs = (DispatchTime.now().uptimeNanoseconds - began) / 1000
        for i in 0..<Int(out.packetCount) {
            let d = descriptions[i]
            let bytes = UnsafeRawBufferPointer(start: out.data + Int(d.mStartOffset), count: Int(d.mDataByteSize))
            send?(bytes, first, seq, timeUs)
            first = false
            seq &+= 1
        }
    }
}
