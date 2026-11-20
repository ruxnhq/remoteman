// A client's input on the virtual display, posted at the HID tap. Positions arrive as
// fractions of the display; double and triple clicks are derived here. A touch is played as
// the left mouse button (macOS has no touchscreen to inject into).
// Adapted from espalier's ScreenInput.
import AppKit
import CoreGraphics
import Foundation
import IOKit.pwr_mgt

final class HostInput {
    /// The display's bounds in global coordinates (points) and its pixels per point.
    var display: () -> (bounds: CGRect, scale: CGFloat)? = { nil }

    private let queue = DispatchQueue(label: "remoteman.host.input", qos: .userInteractive)
    private let source = CGEventSource(stateID: .hidSystemState)
    private var pressed: [CGMouseButton] = []
    private var modifiers: CGEventFlags = []
    private var last = CGPoint.zero
    /// When this host last put the pointer somewhere (uptime seconds).
    private var movedAt = 0.0
    private var lastDown: (button: CGMouseButton, at: CGPoint, time: TimeInterval, count: Int)?
    private var touchSlot: UInt8?

    /// HID usage to macOS virtual key code, from the viewer's table the other way round.
    private static let keys: [UInt16: CGKeyCode] = {
        var m: [UInt16: CGKeyCode] = [:]
        for code in UInt16(0)...127 {
            if let usage = hidUsage(forMacKeyCode: code), m[usage] == nil { m[usage] = CGKeyCode(code) }
        }
        return m
    }()

    private static let modifierFlags: [UInt16: CGEventFlags] = [
        0xE0: .maskControl, 0xE1: .maskShift, 0xE2: .maskAlternate, 0xE3: .maskCommand,
        0xE4: .maskControl, 0xE5: .maskShift, 0xE6: .maskAlternate, 0xE7: .maskCommand,
    ]

    /// Count as user activity: wakes a dimmed or sleeping display. At most once a second: each is a
    /// call to the power manager, and a fast mouse sends thousands of events a second.
    private static let wakeLock = NSLock()
    private static var wokenAt = 0.0
    static func wake() {
        let now = ProcessInfo.processInfo.systemUptime
        wakeLock.lock()
        let due = now - wokenAt >= 1
        if due { wokenAt = now }
        wakeLock.unlock()
        guard due else { return }
        var id: IOPMAssertionID = 0
        IOPMAssertionDeclareUserActivity("remoteman input" as CFString, kIOPMUserActiveLocal, &id)
    }

    /// A pointer move waiting in the queue. Moves that come while it waits only change where it goes,
    /// so the queue can't fall behind a fast mouse; anything else seals it, so that a click lands
    /// where the pointer was when the click was made.
    private final class Move {
        var x: Float, y: Float
        init(_ x: Float, _ y: Float) { self.x = x; self.y = y }
    }
    private let moveLock = NSLock()
    private var waitingMove: Move?
    /// The longest an event waited in the queue, and how many moves were merged, since last asked.
    private var longestWait = 0.0, merged = 0
    func takeQueueFigures() -> (longestWaitMs: Double, merged: Int) {
        moveLock.lock()
        defer { longestWait = 0; merged = 0; moveLock.unlock() }
        return (longestWait * 1000, merged)
    }

    enum Event {
        case pointerTo(Float, Float)
        case pointerBy(Float, Float)
        case button(UInt8, Bool)
        case scroll(Float, Float)
        case key(UInt16, Bool)
        case touch(phase: UInt8, slot: UInt8, Float, Float)
        case text(String)
    }

    func handle(_ e: Event) {
        HostInput.wake()
        let queued = ProcessInfo.processInfo.systemUptime
        moveLock.lock()
        if case .pointerTo(let x, let y) = e {
            if let m = waitingMove {
                m.x = x
                m.y = y
                merged += 1
                moveLock.unlock()
                return
            }
            let m = Move(x, y)
            waitingMove = m
            moveLock.unlock()
            queue.async { [self] in
                moveLock.lock()
                if waitingMove === m { waitingMove = nil }
                moveLock.unlock()
                run(.pointerTo(m.x, m.y), queued: queued)
            }
            return
        }
        waitingMove = nil
        moveLock.unlock()
        queue.async { [self] in run(e, queued: queued) }
    }

    private func run(_ e: Event, queued: TimeInterval) {
        let waited = ProcessInfo.processInfo.systemUptime - queued
        moveLock.lock()
        longestWait = max(longestWait, waited)
        moveLock.unlock()
        do {
            guard let (b, scale) = display() else { return }
            func at(_ x: Float, _ y: Float) -> CGPoint {
                CGPoint(
                    x: b.minX + b.width * CGFloat(min(1, max(0, x))), y: b.minY + b.height * CGFloat(min(1, max(0, y))))
            }
            switch e {
            case .pointerTo(let x, let y):
                move(to: at(x, y))
            case .pointerBy(let dx, let dy):
                // From where the pointer really is, when this host hasn't just put it somewhere
                // itself: the first relative movement of a session used to start from the display's
                // corner, and so did the first after anything else had moved the pointer. Between
                // quick movements the running position is kept, so that none of them is lost to
                // an event that hasn't landed yet.
                let now = ProcessInfo.processInfo.systemUptime
                if now - movedAt > 0.3, let real = CGEvent(source: nil)?.location { last = real }
                // Display pixels to points, kept on the display.
                let p = CGPoint(x: last.x + CGFloat(dx) / scale, y: last.y + CGFloat(dy) / scale)
                move(to: CGPoint(x: min(b.maxX - 1, max(b.minX, p.x)), y: min(b.maxY - 1, max(b.minY, p.y))))
            case .button(let n, let down):
                button(n == 1 ? .right : n == 2 ? .center : .left, down: down)
            case .scroll(let dx, let dy):
                // Positive dy means towards the end of the document, which is a negative wheel delta.
                let e = CGEvent(
                    scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2,
                    wheel1: Int32((-CGFloat(dy) / scale).rounded()), wheel2: Int32((-CGFloat(dx) / scale).rounded()),
                    wheel3: 0)
                e?.setIntegerValueField(.scrollWheelEventIsContinuous, value: 1)
                e?.location = last
                post(e)
            case .key(let usage, let down):
                key(usage: usage, down: down)
            case .touch(let phase, let slot, let x, let y):
                // The first finger down is the mouse; other fingers are ignored for now.
                switch phase {
                case 0 where touchSlot == nil:
                    touchSlot = slot
                    move(to: at(x, y))
                    button(.left, down: true)
                case 1 where touchSlot == slot:
                    move(to: at(x, y))
                case 2 where touchSlot == slot:
                    move(to: at(x, y))
                    button(.left, down: false)
                    touchSlot = nil
                default: break
                }
            case .text(let s):
                let text = Array(s.utf16)
                for chunk in stride(from: 0, to: text.count, by: 20).map({ Array(text[$0..<min($0 + 20, text.count)]) }) {
                    for down in [true, false] {
                        let e = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                        chunk.withUnsafeBufferPointer {
                            e?.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: $0.baseAddress)
                        }
                        post(e)
                    }
                }
            }
        }
    }

    private func move(to p: CGPoint) {
        last = p
        movedAt = ProcessInfo.processInfo.systemUptime
        let type: CGEventType =
            pressed.contains(.left)
            ? .leftMouseDragged
            : pressed.contains(.right) ? .rightMouseDragged : pressed.isEmpty ? .mouseMoved : .otherMouseDragged
        post(mouse(type, p, button: pressed.first ?? .left))
    }

    private func button(_ button: CGMouseButton, down: Bool) {
        var count = 1
        if down {
            let now = ProcessInfo.processInfo.systemUptime
            if let l = lastDown, l.button == button, now - l.time < NSEvent.doubleClickInterval,
                abs(l.at.x - last.x) < 5, abs(l.at.y - last.y) < 5
            {
                count = l.count + 1
            }
            lastDown = (button, last, now, count)
            if !pressed.contains(button) { pressed.append(button) }
        } else {
            count = lastDown?.button == button ? lastDown!.count : 1
            pressed.removeAll { $0 == button }
        }
        let type: CGEventType =
            switch (button, down) {
            case (.left, true): .leftMouseDown
            case (.left, false): .leftMouseUp
            case (.right, true): .rightMouseDown
            case (.right, false): .rightMouseUp
            case (_, true): .otherMouseDown
            case (_, false): .otherMouseUp
            }
        let e = mouse(type, last, button: button)
        e?.setIntegerValueField(.mouseEventClickState, value: Int64(count))
        post(e)
    }

    private func mouse(_ type: CGEventType, _ p: CGPoint, button: CGMouseButton) -> CGEvent? {
        let e = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: p, mouseButton: button)
        e?.flags = modifiers
        return e
    }

    private func post(_ e: CGEvent?) { e?.post(tap: .cghidEventTap) }

    private func key(usage: UInt16, down: Bool) {
        guard let code = HostInput.keys[usage] else { return }
        if let flag = HostInput.modifierFlags[usage] {
            if down { modifiers.insert(flag) } else { modifiers.remove(flag) }
            let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
            e?.type = .flagsChanged
            e?.flags = modifiers
            post(e)
            return
        }
        let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)
        e?.flags = modifiers
        post(e)
    }

    /// Let go of held buttons and modifiers when the client leaves.
    func release() {
        queue.async { [self] in
            for b in pressed {
                post(mouse(b == .left ? .leftMouseUp : b == .right ? .rightMouseUp : .otherMouseUp, last, button: b))
            }
            pressed = []
            touchSlot = nil
            if !modifiers.isEmpty {
                modifiers = []
                let e = CGEvent(source: source)
                e?.type = .flagsChanged
                e?.flags = []
                post(e)
            }
        }
    }
}
