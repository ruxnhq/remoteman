package com.xnixy.remoteman

import android.view.Surface

/** The Rust side (crates/remoteman-android): the connection, and the decoder drawing into [Surface]. */
object Core {
    init {
        System.loadLibrary("remoteman_android")
    }

    /** Called on the core's threads, never the main one. */
    interface Listener {
        /** How far connecting has got: 0 looking the host up, 1 reaching it, 2 hello, 3 the display. */
        fun onStage(stage: Int)
        /** This device is new to the host: its owner confirms there, comparing [code]. */
        fun onPairing(code: String, seconds: Int)
        /** The picture is (re)starting in this format. */
        fun onStream(description: String, width: Int, height: Int)
        /** The connection is over, or never started. */
        fun onClosed(reason: String)
    }

    /** Blocks until the host answers: call it off the main thread. 0 when it failed (after onClosed). */
    external fun connect(
        host: String, stateDir: String, name: String, pair: Boolean,
        width: Int, height: Int, scale: Float, refresh: Int,
        decoders: String,
        /** 0 an extra display, 1 the main display, 2 the only display, 3 mirror the computer's screen. */
        placement: Int,
        surface: Surface, listener: Listener,
    ): Long

    external fun close(handle: Long)

    /** [lost, keyframes asked for, round trip µs, decoded, dropped, age at the screen µs or -1]. */
    external fun stats(handle: Long): IntArray?

    /** "name\tos\tfingerprint". */
    external fun host(handle: Long): String?

    /** phase 0 down, 1 move, 2 up; x and y fractions of the display. */
    external fun touch(handle: Long, phase: Int, slot: Int, x: Float, y: Float)
    external fun pointerTo(handle: Long, x: Float, y: Float)
    /** 0 left, 1 right, 2 middle. */
    external fun button(handle: Long, button: Int, down: Boolean)
    /** A USB HID usage, page 7. */
    external fun key(handle: Long, usage: Int, down: Boolean)
    /** A hardware key by its Linux code (KeyEvent.getScanCode()); false if it has no USB usage. */
    external fun keyLinux(handle: Long, code: Int, down: Boolean): Boolean
    /** The benchmark: play a recording (RMREC1) through the decoder into [surface]; results as JSON. */
    external fun benchSurface(path: String, surface: Surface): String
    /** Let the benchmark's last decoder (and its picture on the surface) go. */
    external fun benchRelease()

    /** Decode [mime] with the decoder called [name] (see [Decoders.choose]). */
    external fun useDecoder(mime: String, name: String)

    /** Display pixels; positive dy moves towards the end of a document. */
    external fun scroll(handle: Long, dx: Float, dy: Float)
}
