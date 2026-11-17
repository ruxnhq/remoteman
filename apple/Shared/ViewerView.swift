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
        }

        @objc private func tick() {
            flushPointer()
            session.renderer?.draw()
        }

        // MARK: Pointer

        private func flushPointer() {
            guard let p = pendingPointer else { return }
            pendingPointer = nil
            session.client?.pointerTo(x: p.0, y: p.1)
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
            session.client?.button(b, down: down)
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
            session.client?.scroll(dx: Float(-e.scrollingDeltaX * k), dy: Float(-e.scrollingDeltaY * k))
        }

        // MARK: Keys

        private func key(_ e: NSEvent, down: Bool) {
            guard let usage = hidUsage(forMacKeyCode: e.keyCode) else { return }
            flushPointer()
            session.client?.key(usage: usage, down: down)
        }

        override func keyDown(with e: NSEvent) { if !e.isARepeat { key(e, down: true) } }
        override func keyUp(with e: NSEvent) { key(e, down: false) }

        /// ⌘ shortcuts go to the host too, except ⌘Q, which stays with this app.
        override func performKeyEquivalent(with e: NSEvent) -> Bool {
            guard session.client != nil, window?.firstResponder === self, e.type == .keyDown else { return false }
            if e.modifierFlags.contains(.command), e.charactersIgnoringModifiers == "q" { return false }
            if !e.isARepeat { key(e, down: true) }
            return true
        }

        override func flagsChanged(with e: NSEvent) {
            guard let usage = hidUsage(forMacKeyCode: e.keyCode) else { return }
            flushPointer()
            if e.keyCode == 57 {  // caps lock reports a change of state, not a press
                session.client?.key(usage: usage, down: true)
                session.client?.key(usage: usage, down: false)
                return
            }
            let masks: [UInt16: UInt] = [
                59: 0x1, 56: 0x2, 60: 0x4, 55: 0x8, 54: 0x10, 58: 0x20, 61: 0x40, 62: 0x2000,
            ]
            guard let mask = masks[e.keyCode] else { return }
            let down = e.modifierFlags.rawValue & mask != 0
            if down == modifiersDown.contains(e.keyCode) { return }
            if down { modifiersDown.insert(e.keyCode) } else { modifiersDown.remove(e.keyCode) }
            session.client?.key(usage: usage, down: down)
        }

        /// Let go of anything still held when the window loses the keyboard.
        override func resignFirstResponder() -> Bool {
            for code in modifiersDown {
                if let u = hidUsage(forMacKeyCode: code) { session.client?.key(usage: u, down: false) }
            }
            modifiersDown.removeAll()
            return super.resignFirstResponder()
        }
    }

    struct ViewerRepresentable: NSViewRepresentable {
        let session: ViewerSession
        func makeNSView(context: Context) -> ViewerView { ViewerView(session: session) }
        func updateNSView(_ v: ViewerView, context: Context) {}
    }
#else
    import UIKit

    final class ViewerView: UIView {
        let session: ViewerSession
        private var link: CADisplayLink?
        private var slots: [UITouch: UInt8] = [:]

        init(session: ViewerSession) {
            self.session = session
            super.init(frame: .zero)
            isMultipleTouchEnabled = true
            backgroundColor = .black
            if let l = session.renderer?.layer { layer.addSublayer(l) }
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
        }

        @objc private func tick() { session.renderer?.draw() }

        // MARK: Touch

        private func send(_ touches: Set<UITouch>, phase: UInt8) {
            guard let r = session.renderer else { return }
            let rect = r.videoRect(in: bounds.size)
            guard rect.width > 0, rect.height > 0 else { return }
            for t in touches {
                var slot = slots[t]
                if slot == nil, phase == 0 {
                    slot = (0..<10).first { s in !slots.values.contains(s) }
                    if let slot { slots[t] = slot }
                }
                guard let slot else { continue }
                let p = t.location(in: self)
                let x = min(max((p.x - rect.minX) / rect.width, 0), 1)
                let y = min(max((p.y - rect.minY) / rect.height, 0), 1)
                session.client?.touch(phase: phase, slot: slot, x: Float(x), y: Float(y))
                if phase == 2 { slots[t] = nil }
            }
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, phase: 0) }
        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, phase: 1) }
        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, phase: 2) }
        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) { send(touches, phase: 2) }

        // MARK: Hardware keyboard (a UIKey's code is already the USB HID usage)

        private func keys(_ presses: Set<UIPress>, down: Bool) -> Bool {
            var handled = false
            for p in presses {
                guard let k = p.key else { continue }
                session.client?.key(usage: UInt16(k.keyCode.rawValue), down: down)
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
        func makeUIView(context: Context) -> ViewerView { ViewerView(session: session) }
        func updateUIView(_ v: ViewerView, context: Context) {}
    }
#endif
