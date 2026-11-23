package com.ruxn.remoteman

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
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
        /** It never answered: it may be asleep, so waking it is offered. */
        val noAnswer: Boolean = false,
    ) : Phase
}

/** The display asked for: pixels, scale and refresh rate. */
data class DisplayRequest(val width: Int, val height: Int, val scale: Float, val refresh: Int)

/**
 * One connection at a time: connecting (with pairing and the second factor), the picture, and
 * the end. Its state is Compose state, changed on the main thread only; the core's callbacks
 * are posted there, and dropped when they belong to a connection that's been left.
 */
class Session(private val store: ComputerStore, private val stateDir: String, private val device: String, context: Context) {
    private val main = Handler(Looper.getMainLooper())
    private val audio = context.getSystemService(AudioManager::class.java)

    /** Whether the computer's sound is wanted (the setting); applied to the live connection. */
    var soundWanted = true
        set(v) { field = v; applySound() }
    /** The computer's sound plays now. */
    var soundOn by mutableStateOf(false)
        private set
    /** The computer connected to can send its sound (the switch shows only then). */
    var soundOffered by mutableStateOf(false)
        private set
    /** The controllers held here, as the computer's. */
    val gamepads = Gamepads()

    /**
     * Sound focus, as a game asks for it: another app's sound (a call, an alarm, a voice
     * assistant) takes it for a while, and the computer's is silent meanwhile. Short sounds
     * that only duck others (a notification) leave it playing; the system ducks it itself.
     */
    private val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
        .setAudioAttributes(AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_GAME).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build())
        .setWillPauseWhenDucked(false)
        .setOnAudioFocusChangeListener({ change ->
            val h = handle
            if (h == 0L) return@setOnAudioFocusChangeListener
            when (change) {
                AudioManager.AUDIOFOCUS_GAIN -> Core.soundMuted(h, false)
                AudioManager.AUDIOFOCUS_LOSS, AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> Core.soundMuted(h, true)
            }
            Log.i(TAG, "sound focus: $change")
        }, main)
        .build()

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
        gamepads.session(h)
        applySound()
        tick(gen)
    }

    /** The live connection's sound as [soundWanted] says, with the sound focus to match. */
    private fun applySound() {
        val h = handle
        soundOffered = h != 0L && Core.hasSound(h)
        val on = soundWanted && soundOffered
        if (h != 0L && on != soundOn) Core.sound(h, on)
        if (on && !soundOn) {
            val got = audio.requestAudioFocus(focus)
            Core.soundMuted(h, got != AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
        } else if (!on && soundOn) {
            audio.abandonAudioFocusRequest(focus)
        }
        soundOn = on
    }

    /** This app is in front again: if another app took the sound for good, take it back. */
    fun regainSound() {
        if (!soundOn || handle == 0L) return
        val got = audio.requestAudioFocus(focus)
        Core.soundMuted(handle, got != AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
    }

    /** Leave the connection (or stop connecting). [leftApp]: say so, and offer to connect again. */
    fun stop(leftApp: Boolean = false) {
        generation++
        val h = handle
        gamepads.release()
        handle = 0L
        applySound()
        gamepads.session(0L)
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

    private var soundTicks = 0

    private fun tick(gen: Int) {
        if (gen != generation || handle == 0L) return
        Core.stats(handle)?.let { s ->
            val age = if (s[5] >= 0) "%.1f ms".format(s[5] / 1000.0) else "–"
            stats = "shown ${s[3]} · dropped ${s[4]} · lost ${s[0]} · keyframes ${s[1]} · round trip %.1f ms · age $age".format(s[2] / 1000.0)
        }
        Core.soundStats(handle)?.let { s ->
            val out = if (s[5] >= 0) "%.0f ms".format(s[5] / 1000.0) else "–"
            val line = "sound: buffer %.0f ms · out $out · concealed ${s[1]} of ${s[0] + s[1]} · late ${s[2]} · underruns ${s[6]}".format(s[4] / 1000.0)
            stats += "\n" + line
            if (++soundTicks % 5 == 0) Log.i(TAG, "$line · depth ${s[11]} · peak %.2f · AAudio ${s[7]} frames (burst ${s[8]}), ${if (s[9] == 1L) "exclusive low-latency" else "shared or not low-latency"}".format(s[10] / 1000.0))
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
            gamepads.release()
            val h = handle
            val code = if (h != 0L) Core.closeCode(h) else -1
            handle = 0L
            applySound()
            gamepads.session(0L)
            generation++
            if (h != 0L) Thread { Core.close(h) }.start()
            val c = computer
            val name = c?.name ?: "the computer"
            phase = when {
                code == CLOSE_REVOKED -> {
                    c?.let { store.update(it.copy(paired = false)); computer = it.copy(paired = false) }
                    Phase.Ended("$name no longer trusts this $deviceKind", "Turn pairing on at the computer, then pair again.", mustPairAgain = true, byHost = true)
                }
                code >= 0 -> Phase.Ended("$name ended the session", when (code) {
                    CLOSE_DROPPED -> "Someone at the computer ended it."
                    0 -> "The computer stopped sharing its screen, or another device took over."
                    else -> "Something went wrong on the computer. " + plainWords(reason)
                }, byHost = true)
                "isn't the host that was paired" in reason -> Phase.Ended(
                    if (c == null || c.name == c.address) "This isn't the computer this $deviceKind paired with" else "This isn't the $name this $deviceKind paired with",
                    "A different computer, or the same one with a new key, answers at ${c?.address}. If you trust it, forget the old key and pair again.",
                    keyChanged = true,
                )
                else -> Phase.Ended(if (h != 0L) "The connection to $name was lost" else "Can't connect to $name", plainWords(reason), noAnswer = h == 0L && silent(reason))
            }
        }
    }

    /** "phone", "tablet" or "TV", for sentences. */
    var deviceKind = "device"

    companion object {
        const val TAG = "remoteman"
        /** `Client::close_code`: the host revoked this device. */
        const val CLOSE_REVOKED = 3
        /** `Client::close_code`: someone at the host dropped this device. */
        const val CLOSE_DROPPED = 2

        /** Nothing answered at all (it may be asleep), as opposed to an answer that said no. */
        fun silent(reason: String): Boolean = reason.lowercase().let { r ->
            "timed out" in r || "timeout" in r || "unreachable" in r || "no route" in r
        }

        /** The core's reasons are for logs; this says the common ones for people. */
        fun plainWords(reason: String): String {
            val r = reason.lowercase()
            return when {
                "operation not permitted" in r || "eperm" in r ->
                    "remoteman isn't allowed to reach computers on this network. Allow it in Settings, Apps, remoteman, Permissions."
                "look up" in r || "no such host" in r ->
                    "That name wasn't found on this network. Check the address, or use the computer's number (like 192.168.1.20)."
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
