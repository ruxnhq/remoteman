package com.xnixy.remoteman

import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** What the session screen shows over (or instead of) the picture. */
sealed interface Phase {
    data object Idle : Phase
    /** [stage] as `Core.Listener.onStage`; [code] while the computer's owner confirms a new device. */
    data class Connecting(val stage: Int, val code: String? = null) : Phase
    /** The host wants a code from an authenticator app; [left] tries after a wrong one, or -1. */
    data class SecondFactor(val left: Int) : Phase
    data object Streaming : Phase
    /**
     * Over, or it never started. [mustPairAgain]: the computer no longer trusts this device.
     * [byHost]: the computer ended it. [leftApp]: it ended because this app was left.
     */
    data class Ended(
        val headline: String,
        val reason: String,
        val mustPairAgain: Boolean = false,
        val byHost: Boolean = false,
        val leftApp: Boolean = false,
        /** Another key answered than the one this device paired with: forget it to pair afresh. */
        val keyChanged: Boolean = false,
        /** Connecting again can work (not after a scanned code named another computer). */
        val retry: Boolean = true,
    ) : Phase
}

/** The display asked for: pixels, scale and refresh rate. */
data class DisplayRequest(val width: Int, val height: Int, val scale: Float, val refresh: Int)

/**
 * One connection at a time: connecting (with pairing and the second factor), the picture, and
 * the end. Its state is Compose state, changed on the main thread only; the core's callbacks
 * are posted there, and dropped when they belong to a connection that's been left.
 */
class Session(private val store: ComputerStore, private val stateDir: String, private val device: String) {
    private val main = Handler(Looper.getMainLooper())

    var computer by mutableStateOf<Computer?>(null)
        private set
    var phase by mutableStateOf<Phase>(Phase.Idle)
        private set
    /** "AV1 4:2:0 10-bit, 1280×720 at 60 Hz". */
    var stream by mutableStateOf("")
        private set
    var streamSize by mutableStateOf(0 to 0)
        private set
    var stats by mutableStateOf("")
        private set

    /** The live connection's handle (0 none); main thread only. */
    var handle = 0L
        private set
    /** Bumped whenever a connection is started or left: callbacks of older ones are ignored. */
    private var generation = 0

    val active: Boolean get() = phase !is Phase.Idle && phase !is Phase.Ended

    /** Start connecting to [c], the picture going to [surface]. */
    fun start(c: Computer, surface: Surface, display: DisplayRequest, placement: Placement) {
        stop()
        val gen = ++generation
        computer = c
        phase = Phase.Connecting(0)
        stream = ""
        streamSize = 0 to 0
        stats = ""
        val decoders = Decoders.list()
        Decoders.choose()
        val listener = Conn(gen)
        Thread {
            Log.i(TAG, "connecting to ${c.address} as ${display.width}x${display.height} at ${display.refresh} Hz, scale ${display.scale}, $placement, decoders $decoders")
            val h = Core.connect(c.address, stateDir, device, true, display.width, display.height, display.scale, display.refresh, decoders, placement.core, surface, listener)
            main.post { connected(gen, h) }
        }.start()
    }

    private fun connected(gen: Int, h: Long) {
        if (h == 0L) return // onClosed has said why
        if (gen != generation || phase is Phase.Ended) {
            // Left (or ended) while it was connecting.
            Thread { Core.close(h) }.start()
            return
        }
        handle = h
        val c = computer ?: return
        val (name, os, fingerprint) = (Core.host(h) ?: "").split('\t').let { Triple(it.getOrElse(0) { "" }, it.getOrElse(1) { "" }, it.getOrElse(2) { "" }) }
        // Added from a scanned code: any other computer answering at that address is turned away.
        if (!c.expect.isNullOrEmpty() && fingerprint.lowercase() != c.expect) {
            stop()
            // A key kept from before is the computer's own: only a key from this try goes.
            if (!c.paired) Core.forgetHost(stateDir, c.address)
            val kept = c.copy(expect = null)
            store.update(kept)
            computer = kept
            phase = Phase.Ended("This isn't the computer you scanned", "The computer at ${c.address} is not the one whose code you scanned. Nothing was paired.", retry = false)
            return
        }
        val updated = c.copy(
            paired = true,
            lastUsed = System.currentTimeMillis(),
            os = os.ifEmpty { c.os },
            fingerprint = fingerprint.ifEmpty { c.fingerprint },
            name = if (c.name == c.address && name.isNotEmpty()) name else c.name,
            expect = null,
        )
        store.update(updated)
        computer = updated
        if (phase is Phase.Connecting) phase = Phase.Streaming
        tick(gen)
    }

    /** Leave the connection (or stop connecting). [leftApp]: say so, and offer to connect again. */
    fun stop(leftApp: Boolean = false) {
        generation++
        val h = handle
        handle = 0L
        if (h != 0L) Thread { Core.close(h) }.start()
        phase = when {
            phase is Phase.Ended -> phase
            leftApp && computer != null && phase !is Phase.Idle ->
                Phase.Ended("Disconnected", "The session ended when you left remoteman.", leftApp = true)
            else -> Phase.Idle
        }
    }

    /** Back to nothing: the session screen is gone. */
    fun reset() {
        stop()
        phase = Phase.Idle
    }

    /** Throw the computer's old key away (it answered with another one), so the next connection pairs. */
    fun forgetKey(c: Computer) {
        Core.forgetHost(stateDir, c.address)
        store.update(c.copy(paired = false))
    }

    fun secondFactor(code: String, remember: Boolean) {
        if (handle != 0L) Core.secondFactor(handle, code, remember)
        phase = Phase.Connecting(3)
    }

    private fun tick(gen: Int) {
        if (gen != generation || handle == 0L) return
        Core.stats(handle)?.let { s ->
            val age = if (s[5] >= 0) "%.1f ms".format(s[5] / 1000.0) else "–"
            stats = "shown ${s[3]} · dropped ${s[4]} · lost ${s[0]} · keyframes ${s[1]} · round trip %.1f ms · age $age".format(s[2] / 1000.0)
        }
        main.postDelayed({ tick(gen) }, 1000)
    }

    private inner class Conn(val gen: Int) : Core.Listener {
        private fun post(f: () -> Unit) { main.post { if (gen == generation) f() } }

        override fun onStage(stage: Int) = post {
            val p = phase
            phase = Phase.Connecting(stage, (p as? Phase.Connecting)?.code)
        }

        override fun onPairing(code: String, seconds: Int) = post {
            phase = Phase.Connecting((phase as? Phase.Connecting)?.stage ?: 1, code)
        }

        override fun onStream(description: String, width: Int, height: Int) = post {
            Log.i(TAG, "stream: $description")
            stream = description
            streamSize = width to height
            if (phase !is Phase.Ended) phase = Phase.Streaming
        }

        override fun onSecondFactor(left: Int) = post { phase = Phase.SecondFactor(left) }

        override fun onClosed(reason: String) = post {
            Log.i(TAG, "closed: $reason")
            val h = handle
            val code = if (h != 0L) Core.closeCode(h) else -1
            handle = 0L
            generation++
            if (h != 0L) Thread { Core.close(h) }.start()
            val c = computer
            val name = c?.name ?: "the computer"
            phase = when {
                code == CLOSE_REVOKED -> {
                    c?.let { store.update(it.copy(paired = false)); computer = it.copy(paired = false) }
                    Phase.Ended("$name no longer trusts this $deviceKind", "Turn pairing on at the computer, then pair again.", mustPairAgain = true, byHost = true)
                }
                code >= 0 -> Phase.Ended("$name ended the session", plainWords(reason), byHost = true)
                "isn't the host that was paired" in reason -> Phase.Ended(
                    if (c == null || c.name == c.address) "This isn't the computer this $deviceKind paired with" else "This isn't the $name this $deviceKind paired with",
                    "A different computer, or the same one with a new key, answers at ${c?.address}. If you trust it, forget the old key and pair again.",
                    keyChanged = true,
                )
                else -> Phase.Ended(if (h != 0L) "The connection to $name was lost" else "Can't connect to $name", plainWords(reason))
            }
        }
    }

    /** "phone", "tablet" or "TV", for sentences. */
    var deviceKind = "device"

    companion object {
        const val TAG = "remoteman"
        /** `Client::close_code`: the host revoked this device. */
        const val CLOSE_REVOKED = 3

        /** The core's reasons are for logs; this says the common ones for people. */
        fun plainWords(reason: String): String {
            val r = reason.lowercase()
            return when {
                "operation not permitted" in r || "eperm" in r ->
                    "remoteman isn't allowed to reach computers on this network. Allow it in Settings, Apps, remoteman, Permissions."
                "look up" in r || "no such host" in r ->
                    "That name wasn't found on this network. Check the address, or use the computer's number (like 10.0.0.76)."
                "timed out" in r || "timeout" in r ->
                    "It didn't answer. Check that it's on, that remoteman is running there, and that this device is on the same network."
                "refused" in r && "the host refused" !in r ->
                    "Nothing answered at that address. Check that remoteman is running on the computer, and the port."
                "isn't paired" in r -> "This device isn't paired with it yet. Turn pairing on at the computer and connect again."
                "the host refused" in r -> reason.substringAfter("the host refused: ").substringAfter(": ").ifEmpty { reason }
                else -> reason.replaceFirstChar { it.uppercase() }
            }
        }
    }
}

/** The model name, which the computer shows for this device. */
val deviceName: String get() = Build.MODEL
