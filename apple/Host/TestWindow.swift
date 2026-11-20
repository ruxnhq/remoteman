// --test-window yes: a window of the host's own filling the virtual display, redrawn at every
// refresh with a frame counter and a moving bar, so capture, encode and the whole path to a
// client can be measured with real motion. Adapted from espalier's TestWindow.
import AppKit
import QuartzCore

/// Takes clicks and keys and writes them to the log, so input can be checked end to end
/// without touching anything of the user's.
private final class TestView: NSView {
    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    private func note(_ what: String, _ e: NSEvent) {
        let p = convert(e.locationInWindow, from: nil)
        hostLog(String(format: "test window: %@ at %.3f, %.3f (clicks %d)", what, p.x / bounds.width, 1 - p.y / bounds.height, e.clickCount))
    }
    override func mouseDown(with e: NSEvent) {
        window?.makeKey()
        note("left down", e)
    }
    override func mouseUp(with e: NSEvent) { note("left up", e) }
    override func rightMouseDown(with e: NSEvent) { note("right down", e) }
    override func scrollWheel(with e: NSEvent) { hostLog(String(format: "test window: scroll %.1f, %.1f", e.scrollingDeltaX, e.scrollingDeltaY)) }
    override func keyDown(with e: NSEvent) { hostLog("test window: key down code \(e.keyCode) characters \"\(e.characters ?? "")\" flags \(e.modifierFlags.intersection(.deviceIndependentFlagsMask).rawValue)") }
}

private final class KeyableWindow: NSWindow {
    override var canBecomeKey: Bool { true }
}

final class TestWindow {
    private var window: NSWindow?
    private var link: CADisplayLink?
    private let text = CATextLayer()
    private let bar = CALayer()
    private let bright = CALayer()
    /// `--test-bright yes`: a patch above SDR white in the middle.
    static var bright = false
    /// The middle patch's level (`--test-bright-level`), 4 by default; at 1 nothing on screen asks for headroom.
    static var level: CGFloat = 4
    private var n = 0
    private var size = CGSize.zero

    /// The display it covers. A display changed in place (or made anew) can reach NSScreen a moment
    /// after the host asks for it, at its old size or not at all: the window follows every change
    /// of the screens and tries again until the display is there.
    private var displayID: CGDirectDisplayID = 0
    private var watching: NSObjectProtocol?

    private func screen(_ id: CGDirectDisplayID) -> NSScreen? {
        NSScreen.screens.first {
            ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id
        }
    }

    private func refit() {
        guard displayID != 0 else { return }
        guard let s = screen(displayID) else { return }
        guard let w = window else { return show(on: displayID) }
        let f = s.frame
        guard w.frame != f else { return }
        w.setFrame(f, display: true)
        w.contentView?.frame = NSRect(origin: .zero, size: f.size)
        size = f.size
        text.frame = CGRect(x: 40, y: f.height - 160, width: f.width - 80, height: 120)
        hostLog("test window: now \(Int(f.width))×\(Int(f.height)) points")
    }

    func show(on id: CGDirectDisplayID) {
        displayID = id
        if watching == nil {
            watching = NotificationCenter.default.addObserver(
                forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
            ) { [weak self] _ in self?.refit() }
        }
        // Not there yet: again shortly (and on the next change of the screens).
        if window == nil, screen(id) == nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) { [weak self] in
                guard let self, self.displayID == id, self.window == nil else { return }
                self.show(on: id)
            }
            return
        }
        for delay in [0.3, 1.0] {
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [weak self] in self?.refit() }
        }
        guard window == nil, let screen = screen(id) else { return }
        let f = screen.frame
        hostLog("test window: shown at \(Int(f.width))×\(Int(f.height)) points on display \(id)")
        let w = KeyableWindow(contentRect: f, styleMask: [.borderless], backing: .buffered, defer: false, screen: screen)
        w.setFrame(f, display: true)
        w.isReleasedWhenClosed = false
        w.level = .normal
        let view = TestView(frame: NSRect(origin: .zero, size: f.size))
        view.wantsLayer = true
        view.layer?.backgroundColor = CGColor(red: 0.07, green: 0.08, blue: 0.11, alpha: 1)
        size = f.size
        text.frame = CGRect(x: 40, y: f.height - 160, width: f.width - 80, height: 120)
        text.fontSize = 28
        text.font = NSFont.monospacedSystemFont(ofSize: 28, weight: .regular)
        text.foregroundColor = .white
        text.contentsScale = screen.backingScaleFactor
        bar.frame = CGRect(x: 0, y: 80, width: 120, height: 120)
        bar.backgroundColor = CGColor(red: 0.2, green: 0.8, blue: 0.7, alpha: 1)
        if Self.bright {
            // A patch four times as bright as SDR white, for checking HDR: it needs a layer that
            // may go over 1.0 and a colour in an extended space.
            view.layer?.wantsExtendedDynamicRangeContent = Self.level > 1
            bright.wantsExtendedDynamicRangeContent = Self.level > 1
            bright.frame = CGRect(x: f.width / 2 - 200, y: f.height / 2 - 100, width: 400, height: 200)
            if let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB) {
                bright.backgroundColor = CGColor(colorSpace: space, components: [Self.level, Self.level, Self.level, 1])
                // Beside it: plain white (1.0) on the left and twice white on the right, for
                // finding where SDR white sits in the signal.
                for (i, v) in (Self.level > 1 ? [(0, 1.0), (1, 2.0)] : []) {
                    let l = CALayer()
                    l.wantsExtendedDynamicRangeContent = true
                    l.frame = CGRect(x: i == 0 ? f.width * 0.1 : f.width * 0.9 - 300, y: f.height / 2 - 100, width: 300, height: 200)
                    l.backgroundColor = CGColor(colorSpace: space, components: [CGFloat(v), CGFloat(v), CGFloat(v), 1])
                    view.layer?.addSublayer(l)
                }
            }
            view.layer?.addSublayer(bright)
        }
        view.layer?.addSublayer(text)
        view.layer?.addSublayer(bar)
        w.contentView = view
        w.orderFrontRegardless()
        window = w
        let l = view.displayLink(target: self, selector: #selector(tick))
        l.add(to: .main, forMode: .common)
        link = l
    }

    private var rateFrom = CACurrentMediaTime()
    private var rateCount = 0

    /// For measuring (`--test-bursts yes`): move for 0.4 s, be still for 1.1 s, and so on, so
    /// that every burst has a last frame.
    static var bursts = false

    /// `--test-noise yes`: the whole window is fresh noise at every refresh, the hardest picture
    /// an encoder gets (for measuring it at high bitrates).
    static var noise = false
    private var noiseImages: [CGImage] = []
    private let noiseLayer = CALayer()
    private func makeNoise() {
        let w = 960, h = 540
        for _ in 0..<6 {
            var px = [UInt8](repeating: 0, count: w * h * 4)
            px.withUnsafeMutableBytes { arc4random_buf($0.baseAddress, $0.count) }
            if let p = CGDataProvider(data: Data(px) as CFData),
                let i = CGImage(width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(),
                    bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue), provider: p, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
            { noiseImages.append(i) }
        }
        noiseLayer.frame = CGRect(origin: .zero, size: size)
        noiseLayer.magnificationFilter = .nearest
        window?.contentView?.layer?.insertSublayer(noiseLayer, at: 0)
    }

    @objc private func tick() {
        if Self.noise {
            if noiseImages.isEmpty { makeNoise() }
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            if !noiseImages.isEmpty { noiseLayer.contents = noiseImages[n % noiseImages.count] }
            CATransaction.commit()
        }
        if Self.bursts, CACurrentMediaTime().truncatingRemainder(dividingBy: 1.5) > 0.4 { return }
        n += 1
        rateCount += 1
        let now = CACurrentMediaTime()
        if now - rateFrom >= 5 {
            hostLog(String(format: "test window: redrawing %.1f times a second", Double(rateCount) / (now - rateFrom)))
            rateFrom = now
            rateCount = 0
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        text.string = "remoteman Mac host test  frame \(n)\nthe quick brown fox jumps over the lazy dog 0123456789"
        bar.frame.origin.x = CGFloat((n * 12) % max(1, Int(size.width) - 120))
        bar.frame.origin.y = 80 + CGFloat((n * 5) % max(1, Int(size.height) - 360))
        CATransaction.commit()
    }

    func close() {
        displayID = 0
        link?.invalidate()
        link = nil
        window?.close()
        window = nil
    }
}
