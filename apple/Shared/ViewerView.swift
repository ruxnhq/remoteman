// The view the stream is drawn in, and the input it sends back. One class per platform with
// the same job: host the Metal layer, drive the renderer from a display link, and turn mouse,
// keyboard and touch events into the core's input calls.
import QuartzCore
import SwiftUI

/// What a display link on this screen really delivers, measured before connecting: a mode's
/// nominal rate can be more than the link ticks at (a virtual display saying 144 Hz and
/// ticking at 120), and frames the host sends beyond that are only thrown away.
final class RefreshMeter {
    private var stamps: [CFTimeInterval] = []
    private(set) var rate: Int?

    func tick(_ t: CFTimeInterval) {
        stamps.append(t)
        if stamps.count > 90 { stamps.removeFirst(stamps.count - 90) }
        guard stamps.count >= 60, let first = stamps.first, let last = stamps.last, last > first else { return }
        let hz = Double(stamps.count - 1) / (last - first)
        // Only a steady link counts: just after a wake or an unlock it runs slow for a moment,
        // and the two halves of the window then disagree.
        let mid = stamps[stamps.count / 2]
        let a = Double(stamps.count / 2) / (mid - first), b = Double(stamps.count - 1 - stamps.count / 2) / (last - mid)
        guard abs(a - b) / hz < 0.02 else { return }
        // Snap to the rate it is evidently aiming for.
        let common = [24, 30, 48, 50, 60, 72, 75, 90, 100, 120, 144, 165, 240]
        rate = common.first { abs(Double($0) - hz) / Double($0) < 0.04 } ?? Int(hz.rounded())
    }
}

#if os(macOS)
    import AppKit

    /// An invisible view that runs a display link to feed the meter while the connect form is up.
    final class RefreshProbeView: NSView {
        let meter: RefreshMeter
        private var link: CADisplayLink?
        init(meter: RefreshMeter) {
            self.meter = meter
            super.init(frame: .zero)
        }
        required init?(coder: NSCoder) { fatalError() }
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            link?.invalidate()
            link = nil
            guard window != nil else { return }
            let l = displayLink(target: self, selector: #selector(tick(_:)))
            l.add(to: .main, forMode: .common)
            link = l
        }
        @objc private func tick(_ l: CADisplayLink) { meter.tick(l.timestamp) }
    }

    struct RefreshProbe: NSViewRepresentable {
        let meter: RefreshMeter
        func makeNSView(context: Context) -> RefreshProbeView { RefreshProbeView(meter: meter) }
        func updateNSView(_ v: RefreshProbeView, context: Context) {}
    }

    final class ViewerView: NSView {
        let session: ViewerSession
        private var link: CADisplayLink?
        private var pendingPointer: (Float, Float)?
        private var modifiersDown: Set<UInt16> = []

        init(session: ViewerSession) {
            self.session = session
            super.init(frame: .zero)
            wantsLayer = true
            layerContentsRedrawPolicy = .never
            session.cursorShapeChanged = { [weak self] in self?.updateCursor() }
        }

        // MARK: The host's cursor, as this window's cursor

        private var hostCursor: NSCursor?
        private static let noCursor: NSCursor = {
            let image = NSImage(size: NSSize(width: 16, height: 16))
            return NSCursor(image: image, hotSpot: .zero)
        }()

        private func updateCursor() {
            hostCursor = nil
            if let known = session.hostCursor {
                if let c = known, let r = session.renderer, let v = r.videoSize, v.width > 0 {
                    // The image is in the host display's pixels; draw it at the size it has there.
                    let k = r.videoRect(in: bounds.size).width / v.width
                    let size = NSSize(width: CGFloat(c.image.width) * k, height: CGFloat(c.image.height) * k)
                    hostCursor = NSCursor(
                        image: NSImage(cgImage: c.image, size: size),
                        hotSpot: NSPoint(x: CGFloat(c.hotX) * k, y: CGFloat(c.hotY) * k))
                } else {
                    hostCursor = Self.noCursor
                }
            }
            window?.invalidateCursorRects(for: self)
        }

        override func resetCursorRects() {
            // With the pointer drawn by the computer (in the picture) there is none of this Mac's.
            addCursorRect(bounds, cursor: hostCursor ?? (InputSettings.localPointer || session.input == nil ? .arrow : Self.noCursor))
        }

        required init?(coder: NSCoder) { fatalError() }

        override func makeBackingLayer() -> CALayer { session.renderer?.layer ?? CALayer() }
        override var isFlipped: Bool { true }
        override var acceptsFirstResponder: Bool { true }
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            link?.invalidate()
            link = nil
            NotificationCenter.default.removeObserver(self, name: NSWindow.didResignKeyNotification, object: nil)
            guard let window else {
                capture(false)
                return
            }
            NotificationCenter.default.addObserver(self, selector: #selector(lostKey), name: NSWindow.didResignKeyNotification, object: window)
            let l = displayLink(target: self, selector: #selector(tick))
            l.add(to: .main, forMode: .common)
            link = l
            window.acceptsMouseMovedEvents = true
            window.makeFirstResponder(self)
            resize()
        }

        override func updateTrackingAreas() {
            super.updateTrackingAreas()
            trackingAreas.forEach(removeTrackingArea)
            addTrackingArea(
                NSTrackingArea(
                    rect: bounds, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect], owner: self))
        }

        override func layout() {
            super.layout()
            resize()
        }

        override func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            resize()
        }

        private func resize() {
            guard let r = session.renderer else { return }
            let scale = window?.backingScaleFactor ?? 2
            r.layer.contentsScale = scale
            r.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
            r.setNeedsRedraw()
            updateCursor()
        }

        private var cursorVideoSize: CGSize?

        @objc private func tick() {
            flushPointer()
            session.renderer?.draw()
            // The cursor's size follows the picture's, which is known once a frame is in.
            let v = session.renderer?.videoSize
            if v != cursorVideoSize {
                cursorVideoSize = v
                updateCursor()
            }
            if capturing { placeCapturedCursor() }
        }

        // MARK: A captured pointer (raw movement, for 3D apps and games)

        /// While captured this Mac's pointer is hidden and held still, and its movement goes to
        /// the computer as movement. A click in the picture captures; Control-Option, the window
        /// losing the keyboard or the session ending lets go.
        private(set) var capturing = false
        private let capturedCursor = NSImageView()

        private func capture(_ on: Bool) {
            guard on != capturing else { return }
            capturing = on
            CGAssociateMouseAndMouseCursorPosition(on ? 0 : 1)
            if on { NSCursor.hide() } else { NSCursor.unhide() }
            pendingPointer = nil
            if on, capturedCursor.superview == nil {
                capturedCursor.imageScaling = .scaleAxesIndependently
                addSubview(capturedCursor)
            }
            capturedCursor.isHidden = true
            session.pointerCaptured?(on)
        }

        @objc private func lostKey() { capture(false) }

        /// The computer's cursor where the computer says it is: this Mac's own is hidden.
        private func placeCapturedCursor() {
            guard let known = session.hostCursor, let c = known, let at = session.hostCursorAt, at.2, let r = session.renderer,
                let v = r.videoSize, v.width > 0
            else {
                capturedCursor.isHidden = true
                return
            }
            let rect = r.videoRect(in: bounds.size)
            let k = rect.width / v.width
            capturedCursor.image = NSImage(cgImage: c.image, size: NSSize(width: c.image.width, height: c.image.height))
            capturedCursor.frame = CGRect(
                x: rect.minX + CGFloat(at.0) * rect.width - CGFloat(c.hotX) * k, y: rect.minY + CGFloat(at.1) * rect.height - CGFloat(c.hotY) * k,
                width: CGFloat(c.image.width) * k, height: CGFloat(c.image.height) * k)
            capturedCursor.isHidden = false
        }

        // MARK: Pointer

        private func flushPointer() {
            guard let p = pendingPointer else { return }
            pendingPointer = nil
            session.input?.pointerTo(x: p.0, y: p.1)
        }

        /// Pointer moves are kept to one per refresh, and sent ahead of any click or key.
        private func moved(_ e: NSEvent) {
            guard let r = session.renderer else { return }
            let p = convert(e.locationInWindow, from: nil)
            let rect = r.videoRect(in: bounds.size)
            guard rect.width > 0, rect.height > 0 else { return }
            if capturing {
                // Points moved here, as the computer's pixels.
                let k = (r.videoSize?.width ?? rect.width) / rect.width
                if e.deltaX != 0 || e.deltaY != 0 { session.input?.pointerBy(dx: Float(e.deltaX * k), dy: Float(e.deltaY * k)) }
                return
            }
            let x = min(max((p.x - rect.minX) / rect.width, 0), 1)
            let y = min(max((p.y - rect.minY) / rect.height, 0), 1)
            pendingPointer = (Float(x), Float(y))
        }

        private func button(_ e: NSEvent, _ b: UInt8, down: Bool) {
            moved(e)
            flushPointer()
            session.input?.button(b, down: down)
        }

        override func mouseMoved(with e: NSEvent) { moved(e) }
        override func mouseDragged(with e: NSEvent) { moved(e) }
        override func rightMouseDragged(with e: NSEvent) { moved(e) }
        override func otherMouseDragged(with e: NSEvent) { moved(e) }
        private var swallowedDown = false
        override func mouseDown(with e: NSEvent) {
            window?.makeFirstResponder(self)
            if InputSettings.captured, !capturing, session.input != nil {
                // The click that captures isn't one for the computer.
                capture(true)
                swallowedDown = true
                return
            }
            button(e, 0, down: true)
        }
        override func mouseUp(with e: NSEvent) {
            if swallowedDown {
                swallowedDown = false
                return
            }
            button(e, 0, down: false)
        }

        /// A pinch on the trackpad zooms on the computer: Control with the wheel.
        override func magnify(with e: NSEvent) {
            guard InputSettings.pinchZooms, let c = session.input, e.magnification != 0 else { return }
            flushPointer()
            c.key(usage: 0xE0, down: true)
            c.scroll(dx: 0, dy: Float(-e.magnification * 600))
            c.key(usage: 0xE0, down: false)
        }
        override func rightMouseDown(with e: NSEvent) { button(e, 1, down: true) }
        override func rightMouseUp(with e: NSEvent) { button(e, 1, down: false) }
        override func otherMouseDown(with e: NSEvent) { button(e, 2, down: true) }
        override func otherMouseUp(with e: NSEvent) { button(e, 2, down: false) }

        override func scrollWheel(with e: NSEvent) {
            guard let r = session.renderer, let v = r.videoSize else { return }
            let rect = r.videoRect(in: bounds.size)
            // View points to display pixels; a wheel click without precise deltas is a few lines.
            let k = (rect.width > 0 ? v.width / rect.width : 1) * (e.hasPreciseScrollingDeltas ? 1 : 10)
            flushPointer()
            session.wheel(dx: Float(-e.scrollingDeltaX * k), dy: Float(-e.scrollingDeltaY * k))
        }

        // MARK: Keys

        private func key(_ e: NSEvent, down: Bool) {
            guard let usage = hidUsage(forMacKeyCode: e.keyCode) else { return }
            flushPointer()
            session.hardKey(usage, down: down)
        }

        override func keyDown(with e: NSEvent) { if !e.isARepeat { key(e, down: true) } }
        override func keyUp(with e: NSEvent) { key(e, down: false) }

        /// ⌘ shortcuts go to the host too, except ⌘Q, which stays with this app.
        override func performKeyEquivalent(with e: NSEvent) -> Bool {
            guard session.input != nil, window?.firstResponder === self, e.type == .keyDown else { return false }
            if e.modifierFlags.contains(.command), e.charactersIgnoringModifiers == "q" { return false }
            if !e.isARepeat { key(e, down: true) }
            return true
        }

        override func flagsChanged(with e: NSEvent) {
            guard let usage = hidUsage(forMacKeyCode: e.keyCode) else { return }
            flushPointer()
            if e.keyCode == 57 {  // caps lock reports a change of state, not a press
                session.input?.key(usage: usage, down: true)
                session.input?.key(usage: usage, down: false)
                return
            }
            let masks: [UInt16: UInt] = [
                59: 0x1, 56: 0x2, 60: 0x4, 55: 0x8, 54: 0x10, 58: 0x20, 61: 0x40, 62: 0x2000,
            ]
            guard let mask = masks[e.keyCode] else { return }
            let down = e.modifierFlags.rawValue & mask != 0
            if down == modifiersDown.contains(e.keyCode) { return }
            if down { modifiersDown.insert(e.keyCode) } else { modifiersDown.remove(e.keyCode) }
            session.hardKey(usage, down: down)
            // Control and Option together, and nothing else: the captured pointer is let go.
            let held = e.modifierFlags.intersection([.control, .option, .command, .shift])
            if capturing, down, held == [.control, .option] { capture(false) }
        }

        /// Let go of anything still held when the window loses the keyboard.
        override func resignFirstResponder() -> Bool {
            for code in modifiersDown {
                if let u = hidUsage(forMacKeyCode: code) { session.hardKey(u, down: false) }
            }
            modifiersDown.removeAll()
            capture(false)
            return super.resignFirstResponder()
        }
    }

    struct ViewerRepresentable: NSViewRepresentable {
        let session: ViewerSession
        var keyboardShown = false
        func makeNSView(context: Context) -> ViewerView { ViewerView(session: session) }
        func updateNSView(_ v: ViewerView, context: Context) {}
    }
#else
    import UIKit

    /// An invisible view that runs a display link to feed the meter while the connect form is up.
    final class RefreshProbeView: UIView {
        let meter: RefreshMeter
        private var link: CADisplayLink?
        init(meter: RefreshMeter) {
            self.meter = meter
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }
        required init?(coder: NSCoder) { fatalError() }
        override func didMoveToWindow() {
            super.didMoveToWindow()
            link?.invalidate()
            link = nil
            guard let window else { return }
            let l = CADisplayLink(target: self, selector: #selector(tick(_:)))
            let top = Float(window.screen.maximumFramesPerSecond)
            l.preferredFrameRateRange = CAFrameRateRange(minimum: top, maximum: top, preferred: top)
            l.add(to: .main, forMode: .common)
            link = l
        }
        @objc private func tick(_ l: CADisplayLink) { meter.tick(l.timestamp) }
    }

    struct RefreshProbe: UIViewRepresentable {
        let meter: RefreshMeter
        func makeUIView(context: Context) -> RefreshProbeView { RefreshProbeView(meter: meter) }
        func updateUIView(_ v: RefreshProbeView, context: Context) {}
    }

    final class ViewerView: UIView, UIKeyInput, UIPointerInteractionDelegate, UIGestureRecognizerDelegate {
        let session: ViewerSession
        private var link: CADisplayLink?
        private var slots: [UITouch: UInt8] = [:]
        private var pendingPointer: (Float, Float)?
        private var buttonsDown: [UITouch: UInt8] = [:]
        private var lastScroll = CGPoint.zero

        /// Whether the on-screen keyboard is up. The view is always first responder (for a
        /// hardware keyboard); an empty input view stands in for the keyboard while it is hidden.
        var keyboardShown = false {
            didSet { if keyboardShown != oldValue { reloadInputViews() } }
        }
        private let noKeyboard = UIView()
        override var inputView: UIView? { keyboardShown ? nil : noKeyboard }

        init(session: ViewerSession) {
            self.session = session
            super.init(frame: .zero)
            isMultipleTouchEnabled = true
            backgroundColor = .black
            if let l = session.renderer?.layer { layer.addSublayer(l) }
            // A trackpad or mouse: the pointer hovering, and two-finger or wheel scrolling.
            addGestureRecognizer(UIHoverGestureRecognizer(target: self, action: #selector(hover)))
            let scroll = UIPanGestureRecognizer(target: self, action: #selector(scrolled))
            scroll.allowedScrollTypesMask = .all
            scroll.allowedTouchTypes = []
            addGestureRecognizer(scroll)
            // The iPad's own pointer is hidden over the stream; the host's cursor is drawn in
            // its place, at the pointer's position (see below).
            addInteraction(UIPointerInteraction(delegate: self))
            // Three fingers tapped anywhere: the session's controls. One and two fingers belong
            // to the computer; the three that made the tap are taken back from it.
            let three = UITapGestureRecognizer(target: self, action: #selector(threeFingers))
            three.numberOfTouchesRequired = 3
            three.cancelsTouchesInView = true
            addGestureRecognizer(three)
            // A pinch magnifies this device's view of the picture (for those who leave that on);
            // the fingers that made it are taken back from the computer.
            let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinched))
            pinch.cancelsTouchesInView = true
            pinch.delegate = self
            addGestureRecognizer(pinch)
            cursorView.isHidden = true
            cursorView.isUserInteractionEnabled = false
            addSubview(cursorView)
            session.cursorShapeChanged = { [weak self] in self?.updateCursorShape() }
            session.cursorMoved = { [weak self] x, y, visible in self?.hostMovedCursor(x: x, y: y, visible: visible) }
        }

        // MARK: The host's cursor, drawn here at the local pointer

        private let cursorView = UIImageView()
        private var cursorHot = CGPoint.zero
        private var pointerAt: CGPoint?
        private var lastLocalMove: CFTimeInterval = 0

        private func updateCursorShape() {
            guard let known = session.hostCursor, let c = known, let r = session.renderer, let v = r.videoSize,
                v.width > 0
            else {
                cursorView.image = nil
                cursorView.isHidden = true
                return
            }
            let k = r.videoRect(in: bounds.size).width / v.width
            cursorView.image = UIImage(cgImage: c.image)
            cursorView.bounds = CGRect(x: 0, y: 0, width: CGFloat(c.image.width) * k, height: CGFloat(c.image.height) * k)
            cursorHot = CGPoint(x: CGFloat(c.hotX) * k, y: CGFloat(c.hotY) * k)
            placeCursor()
        }

        private func placeCursor() {
            guard let p = pointerAt, cursorView.image != nil else {
                cursorView.isHidden = true
                return
            }
            let size = cursorView.bounds.size
            cursorView.center = CGPoint(x: p.x - cursorHot.x + size.width / 2, y: p.y - cursorHot.y + size.height / 2)
            cursorView.isHidden = false
        }

        private func localPointer(at p: CGPoint?) {
            pointerAt = p
            lastLocalMove = CACurrentMediaTime()
            placeCursor()
        }

        /// The host's own report of where its cursor is. It echoes our moves a pipeline's delay
        /// later, so it only counts when the local pointer has been still (the host moved it).
        private func hostMovedCursor(x: Float, y: Float, visible: Bool) {
            guard CACurrentMediaTime() - lastLocalMove > 0.3, let r = session.renderer else { return }
            if !visible {
                pointerAt = nil
            } else {
                let rect = r.videoRect(in: bounds.size)
                pointerAt = CGPoint(x: rect.minX + CGFloat(x) * rect.width, y: rect.minY + CGFloat(y) * rect.height)
            }
            placeCursor()
        }

        func pointerInteraction(_ interaction: UIPointerInteraction, styleFor region: UIPointerRegion) -> UIPointerStyle? {
            .hidden()
        }

        required init?(coder: NSCoder) { fatalError() }

        override var canBecomeFirstResponder: Bool { true }

        @objc private func threeFingers() { session.toggleControls?() }

        // MARK: Magnifying the view

        private var pinchStart: CGFloat = 1
        private var pinchAnchor = CGPoint.zero  // the spot of the picture under the fingers, as fractions

        override func gestureRecognizerShouldBegin(_ g: UIGestureRecognizer) -> Bool {
            g is UIPinchGestureRecognizer ? InputSettings.viewZoom && session.renderer?.videoSize != nil : super.gestureRecognizerShouldBegin(g)
        }

        @objc private func pinched(_ g: UIPinchGestureRecognizer) {
            guard let r = session.renderer, bounds.width > 0, bounds.height > 0 else { return }
            let at = g.location(in: self)
            switch g.state {
            case .began:
                // Any touches of this pinch that reached the computer are over.
                for (_, slot) in slots { session.input?.touch(phase: 2, slot: slot, x: 0, y: 0) }
                slots = [:]
                pad = [:]
                pinchStart = max(1, r.zoom)
                let rect = r.videoRect(in: bounds.size)
                pinchAnchor = CGPoint(x: (at.x - rect.minX) / rect.width, y: (at.y - rect.minY) / rect.height)
            case .changed, .ended:
                let z = min(8, max(1, pinchStart * g.scale))
                let base = r.videoRect(in: bounds.size, zoomed: false)
                // The spot under the fingers stays under them.
                r.zoomOrigin = CGPoint(
                    x: (at.x - pinchAnchor.x * base.width * z) / bounds.width, y: (at.y - pinchAnchor.y * base.height * z) / bounds.height)
                r.zoom = g.state == .ended && z < 1.08 ? 1 : z
                updateCursorShape()
            default: break
            }
        }

        /// The system's own three-finger gestures for text (undo, copy, paste) stay out of it.
        override var editingInteractionConfiguration: UIEditingInteractionConfiguration { .none }

        // MARK: Shortcuts that stay on this device (shown when Command is held)

        /// Control-Option-Command with M shows or hides the controls, with D disconnects: three
        /// modifiers together are nothing a desktop is likely to want, and nothing iPadOS takes.
        private static let localKeys: [UIKeyboardHIDUsage: Int] = [.keyboardM: 0, .keyboardD: 1]
        private var keysDown: Set<UInt16> = []
        private var lastLocal: CFTimeInterval = 0

        override var keyCommands: [UIKeyCommand]? {
            let flags: UIKeyModifierFlags = [.control, .alternate, .command]
            let show = UIKeyCommand(title: "Show or Hide Session Controls", action: #selector(localShortcut(_:)), input: "m", modifierFlags: flags)
            let leave = UIKeyCommand(title: "Disconnect", action: #selector(localShortcut(_:)), input: "d", modifierFlags: flags)
            for c in [show, leave] { c.wantsPriorityOverSystemBehavior = true }
            return [show, leave]
        }

        @objc private func localShortcut(_ command: UIKeyCommand) { runLocal(command.input == "d" ? 1 : 0) }

        private func runLocal(_ which: Int) {
            // The key press and the key command both lead here: once is enough.
            let now = CACurrentMediaTime()
            guard now - lastLocal > 0.3 else { return }
            lastLocal = now
            // The modifiers went to the computer as they were pressed: let them go there.
            for usage in keysDown { session.hardKey(usage, down: false) }
            keysDown = []
            if which == 0 { session.toggleControls?() } else { session.leaveRequested?() }
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            link?.invalidate()
            link = nil
            guard let window else { return }
            let l = CADisplayLink(target: self, selector: #selector(tick))
            let top = Float(window.screen.maximumFramesPerSecond)
            l.preferredFrameRateRange = CAFrameRateRange(minimum: min(60, top), maximum: top, preferred: top)
            l.add(to: .main, forMode: .common)
            link = l
            becomeFirstResponder()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            guard let r = session.renderer else { return }
            let scale = window?.screen.nativeScale ?? contentScaleFactor
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            r.layer.frame = bounds
            r.layer.contentsScale = scale
            r.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
            CATransaction.commit()
            r.setNeedsRedraw()
            updateCursorShape()
        }

        private var cursorVideoSize: CGSize?

        @objc private func tick() {
            flushPointer()
            session.renderer?.draw()
            // The cursor's size follows the picture's, which is known once a frame is in.
            let v = session.renderer?.videoSize
            if v != cursorVideoSize {
                cursorVideoSize = v
                updateCursorShape()
            }
        }

        // MARK: Pointer (trackpad or mouse)

        private func fraction(_ p: CGPoint) -> (Float, Float)? {
            guard let r = session.renderer else { return nil }
            let rect = r.videoRect(in: bounds.size)
            guard rect.width > 0, rect.height > 0 else { return nil }
            return (
                Float(min(max((p.x - rect.minX) / rect.width, 0), 1)),
                Float(min(max((p.y - rect.minY) / rect.height, 0), 1))
            )
        }

        /// Pointer moves are kept to one per refresh, and sent ahead of any click or key.
        private func flushPointer() {
            guard let p = pendingPointer else { return }
            pendingPointer = nil
            session.input?.pointerTo(x: p.0, y: p.1)
        }

        /// Hover events already come at most once per refresh, so they go straight out; waiting
        /// for the next display tick would only add delay to the host's cursor.
        @objc private func hover(_ g: UIHoverGestureRecognizer) {
            let p = g.location(in: self)
            if g.state == .ended || g.state == .cancelled {
                localPointer(at: nil)
                return
            }
            guard let f = fraction(p) else { return }
            localPointer(at: p)
            pendingPointer = nil
            session.input?.pointerTo(x: f.0, y: f.1)
        }

        @objc private func scrolled(_ g: UIPanGestureRecognizer) {
            let t = g.translation(in: self)
            if g.state == .began { lastScroll = .zero }
            guard g.state == .began || g.state == .changed, let r = session.renderer, let v = r.videoSize else { return }
            let rect = r.videoRect(in: bounds.size)
            let k = rect.width > 0 ? v.width / rect.width : 1
            flushPointer()
            session.wheel(dx: Float(-(t.x - lastScroll.x) * k), dy: Float(-(t.y - lastScroll.y) * k))
            lastScroll = t
        }

        /// A click or drag with a trackpad or mouse arrives as a touch of its own kind.
        private func pointer(_ t: UITouch, _ event: UIEvent?, phase: UInt8) {
            localPointer(at: t.location(in: self))
            if let f = fraction(t.location(in: self)) { pendingPointer = f }
            flushPointer()
            switch phase {
            case 0:
                let b: UInt8 = event?.buttonMask.contains(.secondary) == true ? 1 : 0
                buttonsDown[t] = b
                session.input?.button(b, down: true)
            case 2:
                if let b = buttonsDown.removeValue(forKey: t) { session.input?.button(b, down: false) }
            default: break
            }
        }

        // MARK: The finger as a trackpad

        /// One finger dragged anywhere moves the pointer; a tap clicks, a tap with two fingers
        /// is the right button, two fingers dragged scroll, and a tap followed at once by a
        /// finger held drags.
        private var pad: [UITouch: CGPoint] = [:]
        private var padBegan: CFTimeInterval = 0
        private var padTravel: CGFloat = 0
        private var padMost = 0
        private var padLastTap: CFTimeInterval = 0
        private var padDragging = false

        private func trackpad(_ touches: Set<UITouch>, phase: UInt8) {
            guard let r = session.renderer, let c = session.input else { return }
            let rect = r.videoRect(in: bounds.size)
            let now = CACurrentMediaTime()
            switch phase {
            case 0:
                if pad.isEmpty {
                    padBegan = now
                    padTravel = 0
                    padMost = 0
                    if now - padLastTap < 0.3 {
                        padDragging = true
                        c.button(0, down: true)
                    }
                }
                for t in touches { pad[t] = t.location(in: self) }
                padMost = max(padMost, pad.count)
            case 1:
                var d = CGPoint.zero
                var n: CGFloat = 0
                for t in touches {
                    guard let was = pad[t] else { continue }
                    let p = t.location(in: self)
                    d.x += p.x - was.x
                    d.y += p.y - was.y
                    n += 1
                    pad[t] = p
                }
                guard n > 0 else { return }
                d.x /= n
                d.y /= n
                padTravel += hypot(d.x, d.y)
                if pad.count >= 2 {
                    let k = (r.videoSize?.width ?? rect.width) / rect.width
                    session.wheel(dx: Float(-d.x * k), dy: Float(-d.y * k))
                } else {
                    let speed = InputSettings.pointerFactor
                    var p = pointerAt ?? CGPoint(x: rect.midX, y: rect.midY)
                    p.x = min(max(p.x + d.x * speed, rect.minX), rect.maxX)
                    p.y = min(max(p.y + d.y * speed, rect.minY), rect.maxY)
                    localPointer(at: p)
                    if let f = fraction(p) { c.pointerTo(x: f.0, y: f.1) }
                }
            default:
                for t in touches { pad[t] = nil }
                guard pad.isEmpty else { return }
                if padDragging {
                    padDragging = false
                    c.button(0, down: false)
                } else if now - padBegan < 0.3, padTravel < 8 {
                    // Where the pointer is: the tap itself is not a place.
                    if let p = pointerAt, let f = fraction(p) { c.pointerTo(x: f.0, y: f.1) }
                    if padMost == 1, InputSettings.tapToClick {
                        c.button(0, down: true)
                        c.button(0, down: false)
                        padLastTap = now
                        Haptics.key()
                    } else if padMost == 2, InputSettings.twoFingerRight {
                        c.button(1, down: true)
                        c.button(1, down: false)
                        Haptics.key()
                    }
                }
            }
        }

        // MARK: Touch

        private func send(_ touches: Set<UITouch>, _ event: UIEvent?, phase: UInt8) {
            guard let r = session.renderer else { return }
            let rect = r.videoRect(in: bounds.size)
            guard rect.width > 0, rect.height > 0 else { return }
            if InputSettings.trackpadTouch || !pad.isEmpty {
                let fingers = touches.filter { $0.type != .indirectPointer }
                for t in touches where t.type == .indirectPointer { pointer(t, event, phase: phase) }
                if !fingers.isEmpty { trackpad(fingers, phase: phase) }
                return
            }
            for t in touches {
                if t.type == .indirectPointer {
                    pointer(t, event, phase: phase)
                    continue
                }
                var slot = slots[t]
                if slot == nil, phase == 0 {
                    slot = (0..<10).first { s in !slots.values.contains(s) }
                    if let slot { slots[t] = slot }
                }
                guard let slot else { continue }
                let p = t.location(in: self)
                let x = min(max((p.x - rect.minX) / rect.width, 0), 1)
                let y = min(max((p.y - rect.minY) / rect.height, 0), 1)
                session.input?.touch(phase: phase, slot: slot, x: Float(x), y: Float(y))
                if phase == 2 { slots[t] = nil }
            }
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, event, phase: 0) }
        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, event, phase: 1) }
        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, event, phase: 2) }
        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, event, phase: 2) }

        // MARK: On-screen keyboard (text goes in as typed; Return and Delete as keys)

        var hasText: Bool { true }
        var autocorrectionType: UITextAutocorrectionType { get { .no } set {} }
        var autocapitalizationType: UITextAutocapitalizationType { get { .none } set {} }
        var spellCheckingType: UITextSpellCheckingType { get { .no } set {} }
        var smartQuotesType: UITextSmartQuotesType { get { .no } set {} }
        var smartDashesType: UITextSmartDashesType { get { .no } set {} }
        var smartInsertDeleteType: UITextSmartInsertDeleteType { get { .no } set {} }
        var keyboardAppearance: UIKeyboardAppearance { get { .dark } set {} }

        private func tap(_ usage: UInt16) {
            session.input?.key(usage: usage, down: true)
            session.input?.key(usage: usage, down: false)
        }

        /// The key for a plain character, by position on a US layout (letters and digits only).
        private func usage(for ch: Character) -> UInt16? {
            guard let a = ch.lowercased().unicodeScalars.first?.value, ch.unicodeScalars.count == 1 else { return nil }
            if a >= 97, a <= 122 { return UInt16(0x04 + a - 97) }
            if a >= 49, a <= 57 { return UInt16(0x1E + a - 49) }
            if a == 48 { return 0x27 }
            if a == 32 { return 0x2C }
            return nil
        }

        func insertText(_ text: String) {
            flushPointer()
            if session.modifiersLatched, text.allSatisfy({ usage(for: $0) != nil }) {
                for ch in text { if let u = usage(for: ch) { tap(u) } }
                return
            }
            // Send runs of plain text as text, and the keys an input method can't express as keys.
            var run = ""
            func flush() {
                if !run.isEmpty { session.input?.text(run) }
                run = ""
            }
            for ch in text {
                switch ch {
                case "\n", "\r": flush(); tap(0x28)
                case "\t": flush(); tap(0x2B)
                default: run.append(ch)
                }
            }
            flush()
        }

        func deleteBackward() {
            flushPointer()
            tap(0x2A)
        }

        // MARK: Hardware keyboard (a UIKey's code is already the USB HID usage)

        private func keys(_ presses: Set<UIPress>, down: Bool) -> Bool {
            var handled = false
            for p in presses {
                guard let k = p.key else { continue }
                // A shortcut of this app's own: it doesn't go to the computer.
                if k.modifierFlags.isSuperset(of: [.control, .alternate, .command]), let which = Self.localKeys[k.keyCode] {
                    if down { runLocal(which) }
                    handled = true
                    continue
                }
                flushPointer()
                let usage = UInt16(k.keyCode.rawValue)
                if down { keysDown.insert(usage) } else if keysDown.remove(usage) == nil { handled = true; continue }
                session.hardKey(usage, down: down)
                handled = true
            }
            return handled
        }

        override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            if !keys(presses, down: true) { super.pressesBegan(presses, with: event) }
        }
        override func pressesEnded(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            if !keys(presses, down: false) { super.pressesEnded(presses, with: event) }
        }
        override func pressesCancelled(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
            if !keys(presses, down: false) { super.pressesCancelled(presses, with: event) }
        }
    }

    struct ViewerRepresentable: UIViewRepresentable {
        let session: ViewerSession
        var keyboardShown = false
        func makeUIView(context: Context) -> ViewerView { ViewerView(session: session) }
        func updateUIView(_ v: ViewerView, context: Context) { v.keyboardShown = keyboardShown }
    }
#endif
