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
    private var n = 0
    private var size = CGSize.zero

    func show(on id: CGDirectDisplayID) {
        guard window == nil,
            let screen = NSScreen.screens.first(where: {
                ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id
            })
        else { return }
        let f = screen.frame
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
            view.layer?.wantsExtendedDynamicRangeContent = true
            bright.wantsExtendedDynamicRangeContent = true
            bright.frame = CGRect(x: f.width / 2 - 200, y: f.height / 2 - 100, width: 400, height: 200)
            if let space = CGColorSpace(name: CGColorSpace.extendedLinearSRGB) {
                bright.backgroundColor = CGColor(colorSpace: space, components: [4, 4, 4, 1])
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

    @objc private func tick() {
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
        link?.invalidate()
        link = nil
        window?.close()
        window = nil
    }
}
