// --test-window yes: a window of the host's own filling the virtual display, redrawn at every
// refresh with a frame counter and a moving bar, so capture, encode and the whole path to a
// client can be measured with real motion. Adapted from espalier's TestWindow.
import AppKit
import QuartzCore

final class TestWindow {
    private var window: NSWindow?
    private var link: CADisplayLink?
    private let text = CATextLayer()
    private let bar = CALayer()
    private var n = 0
    private var size = CGSize.zero

    func show(on id: CGDirectDisplayID) {
        guard window == nil,
            let screen = NSScreen.screens.first(where: {
                ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == id
            })
        else { return }
        let f = screen.frame
        let w = NSWindow(contentRect: f, styleMask: [.borderless], backing: .buffered, defer: false, screen: screen)
        w.setFrame(f, display: true)
        w.isReleasedWhenClosed = false
        w.level = .normal
        let view = NSView(frame: NSRect(origin: .zero, size: f.size))
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
        view.layer?.addSublayer(text)
        view.layer?.addSublayer(bar)
        w.contentView = view
        w.orderFrontRegardless()
        window = w
        let l = view.displayLink(target: self, selector: #selector(tick))
        l.add(to: .main, forMode: .common)
        link = l
    }

    @objc private func tick() {
        n += 1
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
