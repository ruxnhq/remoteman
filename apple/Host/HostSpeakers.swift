// This Mac's own speakers while a device plays its sound: silenced, so that the sound is heard
// in one place (as the Linux host does by moving its output to the device), and put back as
// they were when the device stops listening or leaves. ScreenCaptureKit hears what apps play
// before the output's mute, so the device still gets everything.
import AudioToolbox
import CoreAudio

final class HostSpeakers {
    private var device = AudioObjectID(kAudioObjectUnknown)
    private var wasMuted: UInt32?
    private var volume: Float32?

    private static var output: AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyDefaultOutputDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var id = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &id) == noErr, id != kAudioObjectUnknown
        else { return nil }
        return id
    }

    private static func settable(_ id: AudioObjectID, _ a: inout AudioObjectPropertyAddress) -> Bool {
        var can: DarwinBoolean = false
        return AudioObjectHasProperty(id, &a) && AudioObjectIsPropertySettable(id, &a, &can) == noErr && can.boolValue
    }

    /// Silence the Mac's current output. Returns how it was done, for the log.
    func silence() -> String {
        guard wasMuted == nil, volume == nil, let id = Self.output else { return "no output to silence" }
        device = id
        var mute = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyMute, mScope: kAudioDevicePropertyScopeOutput, mElement: kAudioObjectPropertyElementMain)
        if Self.settable(id, &mute) {
            var now: UInt32 = 0
            var size = UInt32(MemoryLayout<UInt32>.size)
            AudioObjectGetPropertyData(id, &mute, 0, nil, &size, &now)
            var on: UInt32 = 1
            if AudioObjectSetPropertyData(id, &mute, 0, nil, size, &on) == noErr {
                wasMuted = now
                return now == 1 ? "it was muted already" : "muted"
            }
        }
        // An output without a mute: its volume goes to nothing instead.
        var level = AudioObjectPropertyAddress(
            mSelector: kAudioHardwareServiceDeviceProperty_VirtualMainVolume, mScope: kAudioDevicePropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMain)
        if Self.settable(id, &level) {
            var now: Float32 = 0
            var size = UInt32(MemoryLayout<Float32>.size)
            AudioObjectGetPropertyData(id, &level, 0, nil, &size, &now)
            var zero: Float32 = 0
            if AudioObjectSetPropertyData(id, &level, 0, nil, size, &zero) == noErr {
                volume = now
                return "volume to nothing"
            }
        }
        return "this output can't be silenced"
    }

    /// Put the output back as it was.
    func restore() {
        if let was = wasMuted {
            var mute = AudioObjectPropertyAddress(
                mSelector: kAudioDevicePropertyMute, mScope: kAudioDevicePropertyScopeOutput, mElement: kAudioObjectPropertyElementMain)
            var value = was
            AudioObjectSetPropertyData(device, &mute, 0, nil, UInt32(MemoryLayout<UInt32>.size), &value)
            wasMuted = nil
        }
        if let was = volume {
            var level = AudioObjectPropertyAddress(
                mSelector: kAudioHardwareServiceDeviceProperty_VirtualMainVolume, mScope: kAudioDevicePropertyScopeOutput,
                mElement: kAudioObjectPropertyElementMain)
            var value = was
            AudioObjectSetPropertyData(device, &level, 0, nil, UInt32(MemoryLayout<Float32>.size), &value)
            volume = nil
        }
    }
}
