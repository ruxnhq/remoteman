package com.ruxn.remoteman

import android.view.Surface
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** Where the app is. A stack: Back pops it. */
sealed interface Screen {
    data object Computers : Screen
    /** Adding a computer by its address (or, on a phone, its code). */
    data object Add : Screen
    /** One computer's settings: name, display placement, forget. */
    data class Computer(val id: String) : Screen
    data object Settings : Screen
    /** The camera, reading a computer's pairing code (phones). */
    data object Scan : Screen
    /** Connecting, pairing and the picture: one screen, because it owns the picture's surface. */
    data object Session : Screen
}

/**
 * The app's state, shared by the phone and TV interfaces: the screen stack, the computers,
 * the settings and the session. The activity supplies what needs it (permissions, the display).
 */
class AppModel(
    val store: ComputerStore,
    val prefs: Prefs,
    val session: Session,
    val isTv: Boolean,
    /** Asks for ACCESS_LOCAL_NETWORK if it's needed, then runs the block (or not, if refused). */
    private val withLocalNetwork: (then: () -> Unit, refused: () -> Unit) -> Unit,
    private val displayRequest: () -> DisplayRequest,
) {
    val stack = mutableStateListOf<Screen>(Screen.Computers)
    val screen: Screen get() = stack.last()

    /** The session's controls (disconnect, keyboard, statistics) are showing. */
    var controls by mutableStateOf(false)
    /** The on-screen keyboard is wanted over the picture (phones). */
    var keyboard by mutableStateOf(false)
    /** A note for the computers list, like a scanned code that wasn't a remoteman one. */
    var note by mutableStateOf<String?>(null)

    private var surface: Surface? = null
    private var pending: Computer? = null

    fun push(s: Screen) { stack += s }

    fun pop() {
        if (stack.size > 1) stack.removeAt(stack.lastIndex)
    }

    /** Back from wherever: true if it was handled here (the app stays open). */
    fun back(): Boolean {
        if (screen == Screen.Session) {
            if (session.active && session.phase == Phase.Streaming) {
                controls = !controls
                return true
            }
            leaveSession()
            return true
        }
        if (stack.size > 1) { pop(); return true }
        return false
    }

    fun add(name: String, address: String, expect: String? = null) {
        val c = store.add(name.trim(), address.trim(), expect)
        stack.removeAll { it == Screen.Add || it == Screen.Scan }
        connect(c)
    }

    /** A pairing link, scanned or opened: add the computer and connect. */
    fun pairLink(link: PairLink) {
        add(link.name, link.addresses.first(), link.fingerprint)
    }

    fun connect(c: Computer) {
        withLocalNetwork({
            note = null
            controls = false
            keyboard = false
            pending = c
            if (screen != Screen.Session) push(Screen.Session)
            startIfReady()
        }, {
            note = "remoteman needs to reach computers on this network. Allow it in Settings, Apps, remoteman, Permissions."
        })
    }

    /** After a changed key: forget the old one and pair afresh. */
    fun pairAgain(c: Computer) {
        session.forgetKey(c)
        connect(store.get(c.id) ?: c)
    }

    /** The picture's surface is there (the session screen is showing). */
    fun surfaceReady(s: Surface) {
        surface = s
        startIfReady()
    }

    /**
     * The surface goes when the app is left (often before the activity hears of it); leaving
     * the session on purpose has already stopped it.
     */
    fun surfaceGone() {
        surface = null
        if (session.active) session.stop(leftApp = true)
    }

    private fun startIfReady() {
        val c = pending ?: return
        val s = surface ?: return
        pending = null
        session.start(c, s, displayRequest(), c.placement ?: prefs.placement)
    }

    /** Disconnect (or stop connecting) and go back to the computers. */
    fun leaveSession() {
        pending = null
        controls = false
        keyboard = false
        session.reset()
        stack.removeAll { it == Screen.Session }
    }

    /** The app is out of sight: the session ends cleanly, and coming back offers to reconnect. */
    fun appStopped() {
        pending = null
        keyboard = false
        if (session.active) session.stop(leftApp = true)
    }
}
