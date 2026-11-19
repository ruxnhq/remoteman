// A check of the system's Opus codec (AudioToolbox through AVAudioConverter), the way sound will
// use it: 48 kHz, short packets, one packet per call, and an empty packet for one that was lost.
// Run with --opus-test yes; the result is printed and shown.
import AVFoundation
import AudioToolbox
import QuartzCore

func opusProbe() -> String {
    var out = "Opus through AVAudioConverter\n"
    let add: (String) -> Void = { out += $0 + "\n" }
    opusCase(channels: 2, framesPerPacket: 480, bitrate: 128000, add)
    opusCase(channels: 1, framesPerPacket: 480, bitrate: 48000, add)
    opusCase(channels: 2, framesPerPacket: 960, bitrate: 128000, add)
    opusCase(channels: 2, framesPerPacket: 240, bitrate: 128000, add)
    return out
}

private func opusCase(channels: UInt32, framesPerPacket: UInt32, bitrate: Int, _ print: (String) -> Void) {
    let pcm = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: channels, interleaved: false)!
    var d = AudioStreamBasicDescription(mSampleRate: 48000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0, mFramesPerPacket: framesPerPacket, mBytesPerFrame: 0, mChannelsPerFrame: channels, mBitsPerChannel: 0, mReserved: 0)
    guard let opus = AVAudioFormat(streamDescription: &d) else { print("no opus format"); return }
    guard let enc = AVAudioConverter(from: pcm, to: opus) else { print("ch \(channels) fpp \(framesPerPacket): no encoder"); return }
    enc.bitRate = bitrate
    guard let dec = AVAudioConverter(from: opus, to: pcm) else { print("no decoder"); return }
    print("ch \(channels) frames/packet \(framesPerPacket) (\(Double(framesPerPacket) / 48.0) ms): encoder bitRate \(enc.bitRate), encoder primeInfo \(enc.primeInfo.leadingFrames)/\(enc.primeInfo.trailingFrames), maxOut \(enc.maximumOutputPacketSize)")
    // 1 s of a 440 Hz tone, fed one packet's worth at a time.
    var phase = 0.0
    var packets: [Data] = []
    var encTimes: [Double] = []
    let n = Int(48000 / framesPerPacket)
    for _ in 0..<n {
        let inBuf = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: framesPerPacket)!
        inBuf.frameLength = framesPerPacket
        for c in 0..<Int(channels) { let p = inBuf.floatChannelData![c]; var ph = phase; for i in 0..<Int(framesPerPacket) { p[i] = Float(sin(ph)) * 0.5; ph += 2 * .pi * 440 / 48000 } }
        phase += 2 * .pi * 440 / 48000 * Double(framesPerPacket)
        let out = AVAudioCompressedBuffer(format: opus, packetCapacity: 4, maximumPacketSize: max(1500, enc.maximumOutputPacketSize))
        var fed = false
        var err: NSError?
        let t0 = CACurrentMediaTime()
        let st = enc.convert(to: out, error: &err) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true; status.pointee = .haveData; return inBuf
        }
        encTimes.append((CACurrentMediaTime() - t0) * 1000)
        if st == .error { print("  encode error \(err!)"); return }
        var off = 0
        for i in 0..<Int(out.packetCount) {
            let size = Int(out.packetDescriptions![i].mDataByteSize)
            packets.append(Data(bytes: out.data + Int(out.packetDescriptions![i].mStartOffset), count: size)); off += size
        }
    }
    let sizes = packets.map { $0.count }.sorted()
    print("  encoded \(packets.count) packets from \(n) inputs, bytes median \(sizes[sizes.count / 2]) max \(sizes.last!), encode ms median \(String(format: "%.3f", encTimes.sorted()[encTimes.count / 2]))")
    // Decode, dropping packet 50, one packet per call.
    var total = 0, decTimes: [Double] = [], framesPerCall: [Int] = [], lostResult = ""
    var rms: [Float] = []
    for (i, pkt) in packets.enumerated() {
        let lost = i == 50
        let cb = AVAudioCompressedBuffer(format: opus, packetCapacity: 1, maximumPacketSize: max(pkt.count, 1))
        if !lost {
            pkt.withUnsafeBytes { cb.data.copyMemory(from: $0.baseAddress!, byteCount: pkt.count) }
            cb.byteLength = UInt32(pkt.count); cb.packetCount = 1
            cb.packetDescriptions![0] = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(pkt.count))
        } else {
            // A lost packet: an empty packet, which Opus decoders take as "conceal".
            cb.byteLength = 0; cb.packetCount = 1
            cb.packetDescriptions![0] = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: 0)
        }
        let outBuf = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: framesPerPacket * 4)!
        var fed = false
        var err: NSError?
        let t0 = CACurrentMediaTime()
        let st = dec.convert(to: outBuf, error: &err) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true; status.pointee = .haveData; return cb
        }
        decTimes.append((CACurrentMediaTime() - t0) * 1000)
        if lost { lostResult = "lost packet: status \(st.rawValue) error \(err?.code ?? 0) frames out \(outBuf.frameLength)" }
        if st == .error && !lost { print("  decode error at \(i): \(err!)"); return }
        total += Int(outBuf.frameLength); framesPerCall.append(Int(outBuf.frameLength))
        if outBuf.frameLength > 0 { var s: Float = 0; let p = outBuf.floatChannelData![0]; for k in 0..<Int(outBuf.frameLength) { s += p[k] * p[k] }; rms.append((s / Float(outBuf.frameLength)).squareRoot()) } else { rms.append(-1) }
    }
    print("  decoded \(total) frames of \(n * Int(framesPerPacket)) (first calls gave \(framesPerCall.prefix(4).map { $0 })), decode ms median \(String(format: "%.3f", decTimes.sorted()[decTimes.count / 2])), decoder primeInfo \(dec.primeInfo.leadingFrames)")
    print("  \(lostResult); rms around it: \(rms[max(0, 47)...min(rms.count - 1, 54)].map { String(format: "%.2f", $0) })")
}
