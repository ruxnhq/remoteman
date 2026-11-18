// The connected device's microphone as a microphone on this Mac. The device sends Opus (mono,
// 48 kHz, 10 ms packets); this decodes it and hands the samples to remoteman's microphone
// driver (HostMicrophone/), which apps then record from as "remoteman Microphone".
// The driver has to be installed once, with the owner's password.
import AVFoundation
import AudioToolbox
import CoreAudio

final class HostMicrophone {
    static let deviceUID = "remoteman-microphone"
    static let driverName = "RemotemanMicrophone.driver"
    static let installedPath = "/Library/Audio/Plug-Ins/HAL/" + driverName
    private static let feed = AudioObjectPropertySelector(0x726D_7063)  // 'rmpc'

    /// Packets taken, concealed, and samples handed over, since the microphone went on.
    private(set) var totals = (packets: 0, concealed: 0, frames: 0, refused: 0)
    private let device: AudioObjectID
    private let pcm = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: 1, interleaved: false)!
    private var opus: AVAudioFormat?
    private var decoder: AVAudioConverter?
    private var next: UInt32?

    /// The driver's device, when the audio server has it.
    static var device: AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyTranslateUIDToDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var uid = deviceUID as CFString
        var id = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let status = withUnsafeMutablePointer(to: &uid) {
            AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, UInt32(MemoryLayout<CFString>.size), $0, &size, &id)
        }
        return status == noErr && id != kAudioObjectUnknown ? id : nil
    }

    static var installed: Bool { device != nil }

    init?() {
        guard let d = Self.device else { return nil }
        device = d
        var desc = AudioStreamBasicDescription(
            mSampleRate: 48000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0, mFramesPerPacket: 480, mBytesPerFrame: 0,
            mChannelsPerFrame: 1, mBitsPerChannel: 0, mReserved: 0)
        guard let o = AVAudioFormat(streamDescription: &desc), let c = AVAudioConverter(from: o, to: pcm) else { return nil }
        opus = o
        decoder = c
    }

    /// One packet from the device (the core's thread).
    func packet(_ data: UnsafeRawBufferPointer, start: Bool, seq: UInt32) {
        totals.packets += 1
        if start {
            decoder?.reset()
        } else if let expected = next {
            let missing = Int(seq &- expected)
            if missing < 0 || missing > 1 << 30 { return }
            if missing > 0, missing <= 5 {
                for _ in 0..<missing {
                    totals.concealed += 1
                    decode(nil)
                }
            } else if missing > 5 {
                decoder?.reset()
            }
        }
        next = seq &+ 1
        decode(data)
    }

    private func decode(_ data: UnsafeRawBufferPointer?) {
        guard let decoder, let opus else { return }
        let size = data?.count ?? 0
        let packet = AVAudioCompressedBuffer(format: opus, packetCapacity: 1, maximumPacketSize: max(size, 1))
        if let data, size > 0 { packet.data.copyMemory(from: data.baseAddress!, byteCount: size) }
        packet.byteLength = UInt32(size)
        packet.packetCount = 1
        packet.packetDescriptions?[0] = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(size))
        guard let out = AVAudioPCMBuffer(pcmFormat: pcm, frameCapacity: 1920) else { return }
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
        guard status != .error, out.frameLength > 0, let samples = out.floatChannelData?[0] else { return }
        hand(Data(bytes: samples, count: Int(out.frameLength) * MemoryLayout<Float32>.size))
        totals.frames += Int(out.frameLength)
    }

    /// Samples for the driver (empty: the microphone went off, let go of what waits).
    private func hand(_ samples: Data) {
        var address = AudioObjectPropertyAddress(mSelector: Self.feed, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var value = samples as CFData
        let status = withUnsafePointer(to: &value) {
            AudioObjectSetPropertyData(device, &address, 0, nil, UInt32(MemoryLayout<CFData>.size), $0)
        }
        if status != noErr { totals.refused += 1 }
    }

    func stop() { hand(Data()) }

    // MARK: Installing

    /// The driver inside this app.
    static var bundled: URL? { Bundle.main.resourceURL?.appendingPathComponent(driverName) }

    /// Install or remove the driver. macOS asks for an administrator's password; the audio
    /// server is restarted, which interrupts sound on this Mac for a moment. Returns what came of it.
    static func setInstalled(_ install: Bool) -> String {
        var commands: [String]
        if install {
            guard let src = bundled, FileManager.default.fileExists(atPath: src.path) else { return "This copy of remoteman has no microphone driver inside." }
            commands = [
                "mkdir -p /Library/Audio/Plug-Ins/HAL", "rm -rf '\(installedPath)'", "cp -R '\(src.path)' '\(installedPath)'",
                "chown -R root:wheel '\(installedPath)'",
            ]
        } else {
            commands = ["rm -rf '\(installedPath)'"]
        }
        commands.append("killall coreaudiod")
        let shell = commands.joined(separator: " && ").replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
        var error: NSDictionary?
        NSAppleScript(source: "do shell script \"\(shell)\" with administrator privileges")?.executeAndReturnError(&error)
        if let error {
            let cancelled = (error[NSAppleScript.errorNumber] as? Int) == -128
            return cancelled ? "" : "It didn't work: \(error[NSAppleScript.errorMessage] as? String ?? "unknown")"
        }
        hostLog("microphone: driver \(install ? "installed" : "removed")")
        return install ? "Installed. It appears among the Mac's microphones in a few seconds." : "Removed."
    }
}
