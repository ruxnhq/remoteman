// This Mac's sound for the device: what ScreenCaptureKit hears (everything the Mac plays, bar
// this app) becomes Opus at 48 kHz stereo in 10 ms packets. Nothing is sent while there is only
// silence; the first packet afterwards is marked, and the numbering has no hole across a pause.
import AVFoundation
import AudioToolbox
import CoreMedia

final class HostSound {
    static let format = #"{"t":"sound_format","codec":"opus","rate":48000,"channels":2,"frame_us":10000}"#
    private static let frames = 480

    private let pcm = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: 2, interleaved: false)!
    private var encoder: AVAudioConverter?
    private var opus: AVAudioFormat?
    private var resampler: AVAudioConverter?
    private var resamplerFrom: AVAudioFormat?
    private var left: [Float] = [], right: [Float] = []
    private var seq: UInt32 = 0
    private var quiet = Int.max  // packets of silence in a row; "max": nothing sent yet
    /// Packets and bytes sent, and pauses begun, since the start.
    private(set) var totals = (packets: 0, bytes: 0, pauses: 0)
    /// One packet, on the sound queue.
    /// `behindUs`: how long before now the packet's first sample was captured, so that each
    /// packet of a capture buffer gets its own time, 10 ms apart.
    var send: ((UnsafeRawBufferPointer, _ start: Bool, _ seq: UInt32, _ behindUs: UInt64) -> Void)?
    /// For tests: the times between capture buffers, in milliseconds.
    private var gaps: [Double] = []
    private var lastBuffer: UInt64 = 0
    var gapReport: String {
        guard gaps.count > 2 else { return "too few buffers" }
        let s = gaps.sorted()
        return String(format: "between capture buffers: median %.1f ms, p99 %.1f ms, max %.1f ms", s[s.count / 2], s[min(s.count - 1, s.count * 99 / 100)], s[s.count - 1])
    }
    /// For tests on a Mac that plays nothing: a quiet 440 Hz tone in place of what was captured,
    /// at the capture's own pace.
    var testTone = false
    private var phase = 0
    private(set) var buffers = 0

    init?() {
        var d = AudioStreamBasicDescription(
            mSampleRate: 48000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0, mFramesPerPacket: UInt32(Self.frames),
            mBytesPerFrame: 0, mChannelsPerFrame: 2, mBitsPerChannel: 0, mReserved: 0)
        guard let opus = AVAudioFormat(streamDescription: &d), let e = AVAudioConverter(from: pcm, to: opus) else { return nil }
        e.bitRate = 128_000
        self.opus = opus
        encoder = e
    }

    /// One buffer of sound from the capture (the sound queue).
    func take(_ sb: CMSampleBuffer) {
        guard let desc = sb.formatDescription, let asbd = desc.audioStreamBasicDescription else { return }
        let n = AVAudioFrameCount(sb.numSamples)
        var a = asbd
        guard n > 0, let from = AVAudioFormat(streamDescription: &a), let buffer = AVAudioPCMBuffer(pcmFormat: from, frameCapacity: n) else { return }
        buffer.frameLength = n
        guard CMSampleBufferCopyPCMDataIntoAudioBufferList(sb, at: 0, frameCount: Int32(n), into: buffer.mutableAudioBufferList) == noErr
        else { return }
        var ready = buffer
        if from != pcm {
            // Not 48 kHz stereo float: bring it there.
            if resampler == nil || resamplerFrom != from {
                resampler = AVAudioConverter(from: from, to: pcm)
                resamplerFrom = from
            }
            let capacity = AVAudioFrameCount(Double(n) * 48000 / from.sampleRate) + 64
            guard let resampler, let out = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: capacity) else { return }
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
            guard status != .error, out.frameLength > 0 else { return }
            ready = out
        }
        guard let data = ready.floatChannelData else { return }
        let count = Int(ready.frameLength)
        buffers += 1
        let arrived = DispatchTime.now().uptimeNanoseconds
        if lastBuffer != 0, gaps.count < 100_000 { gaps.append(Double(arrived - lastBuffer) / 1e6) }
        lastBuffer = arrived
        if testTone {
            for c in 0..<Int(ready.format.channelCount) {
                for i in 0..<count { data[c][i] = 0.05 * Float(sin(2 * Double.pi * 440 * Double(phase + i) / 48000)) }
            }
            phase += count
        }
        left.append(contentsOf: UnsafeBufferPointer(start: data[0], count: count))
        right.append(contentsOf: UnsafeBufferPointer(start: data[ready.format.channelCount > 1 ? 1 : 0], count: count))
        while left.count >= Self.frames {
            packet()
            left.removeFirst(Self.frames)
            right.removeFirst(Self.frames)
        }
    }

    private func packet() {
        // Silence: after 200 ms of it nothing more is sent, until there is sound again.
        var loud = false
        for i in 0..<Self.frames where abs(left[i]) > 1e-4 || abs(right[i]) > 1e-4 {
            loud = true
            break
        }
        if loud {
            let start = quiet >= 20
            quiet = 0
            encode(start: start)
        } else if quiet < 20 {
            quiet += 1
            if quiet == 20 { totals.pauses += 1 }
            encode(start: false)
        }
    }

    private func encode(start: Bool) {
        guard let encoder, let opus, let buffer = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: AVAudioFrameCount(Self.frames)) else { return }
        buffer.frameLength = AVAudioFrameCount(Self.frames)
        left.withUnsafeBufferPointer { buffer.floatChannelData![0].update(from: $0.baseAddress!, count: Self.frames) }
        right.withUnsafeBufferPointer { buffer.floatChannelData![1].update(from: $0.baseAddress!, count: Self.frames) }
        if start { encoder.reset() }
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
            return buffer
        }
        guard status != .error, let descriptions = out.packetDescriptions else { return }
        var first = start
        for i in 0..<Int(out.packetCount) {
            let d = descriptions[i]
            // What is still waiting, this packet included, was captured that long before now.
            let behind = UInt64(left.count) * 1_000_000 / 48000
            send?(UnsafeRawBufferPointer(start: out.data + Int(d.mStartOffset), count: Int(d.mDataByteSize)), first, seq, behind)
            first = false
            seq &+= 1
            totals.packets += 1
            totals.bytes += Int(d.mDataByteSize)
        }
    }
}
