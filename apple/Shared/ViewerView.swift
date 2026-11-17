// The view the stream is drawn in, and the input it sends back. One class per platform with
// the same job: host the Metal layer, drive the renderer from a display link, and turn mouse,
// keyboard and touch events into the core's input calls.
import QuartzCore
import SwiftUI

#if os(macOS)
    import AppKit

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
            addCursorRect(bounds, cursor: hostCursor ?? .arrow)
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
            guard window != nil else { return }
            let l = displayLink(target: self, selector: #selector(tick))
            l.add(to: .main, forMode: .common)
            link = l
            window?.acceptsMouseMovedEvents = true
            window?.makeFirstResponder(self)
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
            r.layer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
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
        override func mouseDown(with e: NSEvent) {
            window?.makeFirstResponder(self)
            button(e, 0, down: true)
        }
        override func mouseUp(with e: NSEvent) { button(e, 0, down: false) }
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
            session.input?.scroll(dx: Float(-e.scrollingDeltaX * k), dy: Float(-e.scrollingDeltaY * k))
        }

        // MARK: Keys

        private func key(_ e: NSEvent, down: Bool) {
            guard let usage = hidUsage(forMacKeyCode: e.keyCode) else { return }
            flushPointer()
            session.input?.key(usage: usage, down: down)
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
            session.input?.key(usage: usage, down: down)
        }

        /// Let go of anything still held when the window loses the keyboard.
        override func resignFirstResponder() -> Bool {
            for code in modifiersDown {
                if let u = hidUsage(forMacKeyCode: code) { session.input?.key(usage: u, down: false) }
            }
            modifiersDown.removeAll()
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

    final class ViewerView: UIView, UIKeyInput, UIPointerInteractionDelegate {
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
            r.layer.drawableSize = CGSize(width: bounds.width * scale, height: bounds.height * scale)
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
            session.input?.scroll(dx: Float(-(t.x - lastScroll.x) * k), dy: Float(-(t.y - lastScroll.y) * k))
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

        // MARK: Touch

        private func send(_ touches: Set<UITouch>, _ event: UIEvent?, phase: UInt8) {
            guard let r = session.renderer else { return }
            let rect = r.videoRect(in: bounds.size)
            guard rect.width > 0, rect.height > 0 else { return }
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

        func insertText(_ text: String) {
            flushPointer()
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
                flushPointer()
                session.input?.key(usage: UInt16(k.keyCode.rawValue), down: down)
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
