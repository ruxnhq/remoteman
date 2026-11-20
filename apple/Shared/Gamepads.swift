// Game controllers (docs/PROTOCOL.md, "Game controllers"): what is plugged in or paired with
// this device is sent to a computer that lists "gamepad", in XInput's terms. Started by the
// session while it streams; each controller gets a pad number from 0 to 3.
import Foundation
import GameController

final class Gamepads {
    /// What `send` is given: pad, flags (bit 0 connected), buttons, triggers, sticks (up and right positive).
    typealias Send = (_ pad: UInt8, _ flags: UInt8, _ buttons: UInt16, _ lt: Float, _ rt: Float, _ lx: Float, _ ly: Float, _ rx: Float, _ ry: Float) -> Bool

    struct State: Equatable {
        var buttons: UInt16 = 0
        var lt: Float = 0, rt: Float = 0, lx: Float = 0, ly: Float = 0, rx: Float = 0, ry: Float = 0
        var neutral: Bool { self == State() }
    }

    /// Print what would be sent, and whether the computer takes it (for tests).
    static var debug = false

    private let wanted: () -> Bool
    private let send: Send
    private var pads: [ObjectIdentifier: (controller: GCController, pad: UInt8)] = [:]
    private var last: [UInt8: State] = [:]
    private var observers: [NSObjectProtocol] = []
    private var timer: Timer?

    /// `wanted`: whether the computer lists "gamepad". Called on the main thread.
    init(wanted: @escaping () -> Bool, send: @escaping Send) {
        self.wanted = wanted
        self.send = send
    }

    func start() {
        guard observers.isEmpty else { return }
        let center = NotificationCenter.default
        observers = [
            center.addObserver(forName: .GCControllerDidConnect, object: nil, queue: .main) { [weak self] n in
                if let c = n.object as? GCController { self?.add(c) }
            },
            center.addObserver(forName: .GCControllerDidDisconnect, object: nil, queue: .main) { [weak self] n in
                if let c = n.object as? GCController { self?.remove(c) }
            },
        ]
        GCController.controllers().forEach(add)
        let t = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in self?.repeatNonNeutral() }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    func stop() {
        observers.forEach(NotificationCenter.default.removeObserver)
        observers = []
        timer?.invalidate()
        timer = nil
        for (_, entry) in pads {
            entry.controller.extendedGamepad?.valueChangedHandler = nil
            emit(entry.pad, connected: false, State())
        }
        pads = [:]
        last = [:]
    }

    private func add(_ c: GCController) {
        guard pads[ObjectIdentifier(c)] == nil, let gamepad = c.extendedGamepad else { return }
        let used = Set(pads.values.map(\.pad))
        guard let pad = (UInt8(0)..<4).first(where: { !used.contains($0) }) else { return }
        pads[ObjectIdentifier(c)] = (c, pad)
        gamepad.valueChangedHandler = { [weak self] g, _ in self?.changed(pad, Self.state(of: g)) }
        emit(pad, connected: true, Self.state(of: gamepad))
    }

    private func remove(_ c: GCController) {
        guard let entry = pads.removeValue(forKey: ObjectIdentifier(c)) else { return }
        c.extendedGamepad?.valueChangedHandler = nil
        emit(entry.pad, connected: false, State())
        last[entry.pad] = nil
    }

    private func changed(_ pad: UInt8, _ s: State) {
        guard last[pad] != s else { return }
        emit(pad, connected: true, s)
    }

    private func repeatNonNeutral() {
        for (pad, s) in last where !s.neutral { emit(pad, connected: true, s) }
    }

    private func emit(_ pad: UInt8, connected: Bool, _ s: State) {
        last[pad] = connected ? s : nil
        let sent = wanted() && send(pad, connected ? 1 : 0, s.buttons, s.lt, s.rt, s.lx, s.ly, s.rx, s.ry)
        if Self.debug {
            print("remoteman: gamepad \(pad) \(connected ? "state" : "gone") buttons \(String(s.buttons, radix: 16)): \(sent ? "sent" : "not sent (the computer has no gamepad)")")
        }
    }

    private static func state(of g: GCExtendedGamepad) -> State {
        var b: UInt16 = 0
        func set(_ on: Bool, _ bit: UInt16) { if on { b |= bit } }
        set(g.dpad.up.isPressed, 0x0001)
        set(g.dpad.down.isPressed, 0x0002)
        set(g.dpad.left.isPressed, 0x0004)
        set(g.dpad.right.isPressed, 0x0008)
        set(g.buttonMenu.isPressed, 0x0010)
        set(g.buttonOptions?.isPressed ?? false, 0x0020)
        set(g.leftThumbstickButton?.isPressed ?? false, 0x0040)
        set(g.rightThumbstickButton?.isPressed ?? false, 0x0080)
        set(g.leftShoulder.isPressed, 0x0100)
        set(g.rightShoulder.isPressed, 0x0200)
        set(g.buttonHome?.isPressed ?? false, 0x0400)
        set(g.buttonA.isPressed, 0x1000)
        set(g.buttonB.isPressed, 0x2000)
        set(g.buttonX.isPressed, 0x4000)
        set(g.buttonY.isPressed, 0x8000)
        // GameController's sticks are already up and right positive, as the core wants them.
        return State(
            buttons: b, lt: g.leftTrigger.value, rt: g.rightTrigger.value,
            lx: g.leftThumbstick.xAxis.value, ly: g.leftThumbstick.yAxis.value,
            rx: g.rightThumbstick.xAxis.value, ry: g.rightThumbstick.yAxis.value)
    }
}
