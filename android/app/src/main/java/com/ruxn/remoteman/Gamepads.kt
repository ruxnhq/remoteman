package com.ruxn.remoteman

import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.math.abs

/**
 * Game controllers held at this device, as Xbox-style controllers on the computer
 * (docs/PROTOCOL.md, "Game controllers"). Up to four, numbered in the order they're first
 * used. Each change goes at once with the controller's whole state, and the state is repeated
 * every 100 ms while anything is off neutral. Main thread only.
 *
 * Back is never taken: on a TV the remote's Back (and a controller's own Back key) opens the
 * session's bar, where the controller then moves the focus as usual.
 */
class Gamepads {
    private class Pad(val slot: Int, val name: String) {
        var keys = 0
        var hat = 0
        var lt = 0f
        var rt = 0f
        /** L2 and R2 as buttons, for controllers without analog triggers. */
        var ltKey = false
        var rtKey = false
        var lx = 0f
        var ly = 0f
        var rx = 0f
        var ry = 0f
        var sent: List<Number>? = null

        fun buttons() = keys or hat
        fun left() = if (ltKey) 1f else lt
        fun right() = if (rtKey) 1f else rt
        fun state(): List<Number> = listOf(buttons(), left(), right(), lx, ly, rx, ry)
        fun neutral() = buttons() == 0 && left() == 0f && right() == 0f && lx == 0f && ly == 0f && rx == 0f && ry == 0f
        fun clear() { keys = 0; hat = 0; lt = 0f; rt = 0f; ltKey = false; rtKey = false; lx = 0f; ly = 0f; rx = 0f; ry = 0f }
    }

    private val main = Handler(Looper.getMainLooper())
    /** By InputDevice id (-1 for injected events). */
    private val pads = HashMap<Int, Pad>()
    private var handle = 0L
    private var repeating = false

    /** Whether [e] comes from a game controller (not a TV remote's D-pad or a keyboard). */
    fun isController(e: KeyEvent) = e.isFromSource(InputDevice.SOURCE_GAMEPAD) || e.isFromSource(InputDevice.SOURCE_JOYSTICK)

    /**
     * A controller's own button (a keyboard that also calls itself a gamepad keeps its other
     * keys as keys).
     */
    fun isPadButton(e: KeyEvent) = isController(e) && (KeyEvent.isGamepadButton(e.keyCode) || e.keyCode in BUTTONS)

    /** Start and Select are both held on some controller. */
    fun chord() = pads.values.any { it.keys and CHORD == CHORD }

    /** A controller's button, while its input goes to the computer [h]. False: not a controller's button (Back isn't). */
    fun key(h: Long, e: KeyEvent): Boolean {
        if (!isController(e)) return false
        val bit = BUTTONS[e.keyCode]
        val trigger = e.keyCode == KeyEvent.KEYCODE_BUTTON_L2 || e.keyCode == KeyEvent.KEYCODE_BUTTON_R2
        if (bit == null && !trigger) return false
        if (e.action != KeyEvent.ACTION_DOWN && e.action != KeyEvent.ACTION_UP) return true
        if (e.repeatCount > 0) return true
        val pad = pad(e.deviceId, e.device) ?: return true
        val down = e.action == KeyEvent.ACTION_DOWN
        when {
            e.keyCode == KeyEvent.KEYCODE_BUTTON_L2 -> pad.ltKey = down
            e.keyCode == KeyEvent.KEYCODE_BUTTON_R2 -> pad.rtKey = down
            else -> pad.keys = if (down) pad.keys or bit!! else pad.keys and bit!!.inv()
        }
        send(h, pad)
        return true
    }

    /** A controller's sticks, triggers and hat. False: not a controller's motion. */
    fun motion(h: Long, e: MotionEvent): Boolean {
        if (!e.isFromSource(InputDevice.SOURCE_JOYSTICK) || e.actionMasked != MotionEvent.ACTION_MOVE) return false
        val device = e.device
        val pad = pad(e.deviceId, device) ?: return true
        fun axis(a: Int): Float {
            val v = e.getAxisValue(a)
            // The stick's own dead zone, as the device describes it.
            val flat = device?.getMotionRange(a, e.source)?.flat ?: 0f
            return if (abs(v) <= maxOf(flat, 0.05f)) 0f else v.coerceIn(-1f, 1f)
        }
        fun has(a: Int) = device?.getMotionRange(a, e.source) != null
        pad.lx = axis(MotionEvent.AXIS_X)
        // Android has down positive, the protocol up (0f - v, so that rest is 0, not -0).
        pad.ly = 0f - axis(MotionEvent.AXIS_Y)
        // Most controllers put the right stick on Z and RZ; some on RX and RY.
        if (device == null || has(MotionEvent.AXIS_Z) || !has(MotionEvent.AXIS_RX)) {
            pad.rx = axis(MotionEvent.AXIS_Z)
            pad.ry = 0f - axis(MotionEvent.AXIS_RZ)
        } else {
            pad.rx = axis(MotionEvent.AXIS_RX)
            pad.ry = 0f - axis(MotionEvent.AXIS_RY)
        }
        // Triggers are LTRIGGER and RTRIGGER on some, BRAKE and GAS on others, both on a few.
        pad.lt = maxOf(axis(MotionEvent.AXIS_LTRIGGER), axis(MotionEvent.AXIS_BRAKE)).coerceIn(0f, 1f)
        pad.rt = maxOf(axis(MotionEvent.AXIS_RTRIGGER), axis(MotionEvent.AXIS_GAS)).coerceIn(0f, 1f)
        val hx = e.getAxisValue(MotionEvent.AXIS_HAT_X)
        val hy = e.getAxisValue(MotionEvent.AXIS_HAT_Y)
        pad.hat = (if (hy < -0.5f) DPAD_UP else 0) or (if (hy > 0.5f) DPAD_DOWN else 0) or
            (if (hx < -0.5f) DPAD_LEFT else 0) or (if (hx > 0.5f) DPAD_RIGHT else 0)
        send(h, pad)
        return true
    }

    /**
     * Input stops going to the computer (the bar opened, the app went away, the session
     * ended): every controller that was off neutral goes back to it there.
     */
    fun release() {
        val h = handle
        for (pad in pads.values) {
            if (pad.neutral()) continue
            pad.clear()
            if (h != 0L) send(h, pad)
        }
    }

    /** A controller was unplugged or switched off: the computer's goes too. */
    fun removed(deviceId: Int) {
        val pad = pads.remove(deviceId) ?: return
        Log.i(TAG, "controller ${pad.slot} gone: ${pad.name}")
        if (handle != 0L) Core.gamepad(handle, pad.slot, false, 0, 0f, 0f, 0f, 0f, 0f, 0f)
    }

    /** A new connection: what was sent before was to another. */
    fun session(h: Long) {
        if (h == handle) return
        handle = h
        for (pad in pads.values) { pad.clear(); pad.sent = null }
    }

    private fun pad(id: Int, device: InputDevice?): Pad? {
        pads[id]?.let { return it }
        val used = pads.values.map { it.slot }.toSet()
        val slot = (0 until 4).firstOrNull { it !in used } ?: return null
        val name = device?.name ?: "injected"
        Log.i(TAG, "controller $slot: $name")
        return Pad(slot, name).also { pads[id] = it }
    }

    private fun send(h: Long, pad: Pad) {
        handle = h
        val state = pad.state()
        if (state == pad.sent) return
        pad.sent = state
        Core.gamepad(h, pad.slot, true, pad.buttons(), pad.left(), pad.right(), pad.lx, pad.ly, pad.rx, pad.ry)
        if (!pad.neutral() && !repeating) {
            repeating = true
            main.postDelayed(::repeat, REPEAT_MS)
        }
    }

    /** Every 100 ms while anything is held: the state again, so the computer's view stays fresh. */
    private fun repeat() {
        val h = handle
        val held = pads.values.filter { !it.neutral() }
        if (h == 0L || held.isEmpty()) { repeating = false; return }
        for (pad in held) Core.gamepad(h, pad.slot, true, pad.buttons(), pad.left(), pad.right(), pad.lx, pad.ly, pad.rx, pad.ry)
        main.postDelayed(::repeat, REPEAT_MS)
    }

    companion object {
        const val TAG = "remoteman"
        const val REPEAT_MS = 100L
        /** Start and Select. */
        const val CHORD = 0x0030
        const val DPAD_UP = 0x0001
        const val DPAD_DOWN = 0x0002
        const val DPAD_LEFT = 0x0004
        const val DPAD_RIGHT = 0x0008

        /** Android's keys to XInput's button bits. */
        val BUTTONS = mapOf(
            KeyEvent.KEYCODE_DPAD_UP to DPAD_UP,
            KeyEvent.KEYCODE_DPAD_DOWN to DPAD_DOWN,
            KeyEvent.KEYCODE_DPAD_LEFT to DPAD_LEFT,
            KeyEvent.KEYCODE_DPAD_RIGHT to DPAD_RIGHT,
            KeyEvent.KEYCODE_BUTTON_START to 0x0010,
            KeyEvent.KEYCODE_BUTTON_SELECT to 0x0020,
            KeyEvent.KEYCODE_BUTTON_THUMBL to 0x0040,
            KeyEvent.KEYCODE_BUTTON_THUMBR to 0x0080,
            KeyEvent.KEYCODE_BUTTON_L1 to 0x0100,
            KeyEvent.KEYCODE_BUTTON_R1 to 0x0200,
            KeyEvent.KEYCODE_BUTTON_MODE to 0x0400,
            KeyEvent.KEYCODE_BUTTON_A to 0x1000,
            KeyEvent.KEYCODE_BUTTON_B to 0x2000,
            KeyEvent.KEYCODE_BUTTON_X to 0x4000,
            KeyEvent.KEYCODE_BUTTON_Y to 0x8000,
        )
    }
}
