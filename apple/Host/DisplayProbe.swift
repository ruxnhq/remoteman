// A look at what a virtual display on this Mac can be: HDR (a transfer function on its mode),
// a range of refresh rates, and what the system then reports. Findings only; run with
// --probe-display yes, which logs and quits. Nothing here is used for a client.
import AppKit
import ScreenCaptureKit

enum DisplayProbe {
    static func run() {
        Task { @MainActor in
            for (tf, deadline, modes) in [
                (UInt32(0), 0.0, [[1920, 1080, 120]]),
                (1, 0.0, [[1920, 1080, 120]]),
                (2, 0.0, [[1920, 1080, 120]]),
                (3, 0.0, [[1920, 1080, 120]]),
                (0, 0.0, [[1920, 1080, 240], [1920, 1080, 120], [1920, 1080, 60], [1920, 1080, 48]]),
                (0, 1.0 / 30, [[1920, 1080, 120]]),
            ] as [(UInt32, Double, [[Int]])] {
                guard let d = RMVirtualDisplay(name: "remoteman probe", maxWidth: 3840, maxHeight: 2160, serial: UInt32.random(in: 70_000...80_000)) else {
                    hostLog("probe: no display")
                    continue
                }
                let ok = d.applyModes(modes.map { $0.map { NSNumber(value: $0) } }, transferFunction: tf, refreshDeadline: deadline)
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                let id = d.displayID
                let screen = NSScreen.screens.first { ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id }
                let mode = CGDisplayCopyDisplayMode(id)
                let all = (CGDisplayCopyAllDisplayModes(id, nil) as? [CGDisplayMode]) ?? []
                var line = "probe: transferFunction \(tf) deadline \(deadline) modes \(modes.map { "\($0[2])" }.joined(separator: "/")): applied \(ok), id \(id)"
                line += ", mode \(mode?.pixelWidth ?? 0)x\(mode?.pixelHeight ?? 0)@\(mode?.refreshRate ?? 0), offered rates \(Set(all.map { Int($0.refreshRate) }).sorted())"
                if let s = screen {
                    line += String(
                        format: ", EDR potential %.2f reference %.2f current %.2f, refresh interval min %.5f max %.5f, max fps %d, space %@",
                        s.maximumPotentialExtendedDynamicRangeColorComponentValue, s.maximumReferenceExtendedDynamicRangeColorComponentValue,
                        s.maximumExtendedDynamicRangeColorComponentValue, s.minimumRefreshInterval, s.maximumRefreshInterval, s.maximumFramesPerSecond,
                        s.colorSpace?.localizedName ?? "?")
                } else {
                    line += ", not among the screens"
                }
                hostLog(line)
            }
            hostLog("probe: done")
            exit(0)
        }
    }
}
