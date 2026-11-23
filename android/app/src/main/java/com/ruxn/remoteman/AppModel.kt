package com.ruxn.remoteman

import android.os.Handler
import android.os.Looper
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
    /** Computers advertising on this network; looked for while the computers screen is in sight. */
    val nearby: Nearby,
    /** Whether the app may reach the local network now (always, before Android 17). */
    private val localNetworkAllowed: () -> Boolean,
    /** Sends the waking packet for these cards (blocking); how many packets left. */
    private val sendWake: (cards: List<String>, lastAddress: String?) -> Int,
) {
    private val main = Handler(Looper.getMainLooper())
    val stack = mutableStateListOf<Screen>(Screen.Computers)
    val screen: Screen get() = stack.last()

    private var controlsShown by mutableStateOf(false)
    /**
     * The session's controls (disconnect, keyboard, statistics) are showing. Input stays here
     * meanwhile, so the controllers held go back to neutral on the computer.
     */
    var controls: Boolean
        get() = controlsShown
        set(v) {
            controlsShown = v
            if (v) session.gamepads.release()
        }
    /** The on-screen keyboard is wanted over the picture (phones). */
    var keyboard by mutableStateOf(false)
    /** A note for the computers list, like a scanned code that wasn't a remoteman one. */
    var note by mutableStateOf<String?>(null)
    /** A passing word for the computers list, like "Waking blackbox: connecting in ten seconds". */
    var info by mutableStateOf<String?>(null)
    /** Looking nearby is wanted but Android 17 hasn't been allowed the local network yet. */
    var nearbyNeedsPermission by mutableStateOf(false)
        private set

    /** How many screens in sight want Nearby (overlapping while one replaces another). */
    private var nearbyWanted = 0
    private var started = true
    private var wakeThenConnect: Runnable? = null

    init {
        nearby.changed = ::rememberCards
    }

    private var surface: Surface? = null
    private var pending: Computer? = null

    init {
        session.soundWanted = prefs.sound
    }

    /** The computer's sound here or not: kept as the setting, and applied to the session now. */
    fun sound(on: Boolean) {
        prefs.sound(on)
        session.soundWanted = on
    }

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
        cancelWake()
        withLocalNetwork({
            note = null
            info = null
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
        started = false
        updateNearby()
        cancelWake()
        pending = null
        keyboard = false
        if (session.active) session.stop(leftApp = true)
    }

    fun appStarted() {
        started = true
        updateNearby()
    }

    // Nearby: computers advertising on this network.

    /** A computers screen comes into sight (or goes): look nearby while one is. */
    fun lookNearby(on: Boolean) {
        nearbyWanted = maxOf(0, nearbyWanted + if (on) 1 else -1)
        updateNearby()
    }

    /** The person asked to look: ask Android 17 for the local network first. */
    fun allowNearby() {
        withLocalNetwork({ updateNearby() }, {
            note = "remoteman needs to reach computers on this network to find them. Allow it in Settings, Apps, remoteman, Permissions."
        })
    }

    private fun updateNearby() {
        val run = nearbyWanted > 0 && started
        nearbyNeedsPermission = run && !localNetworkAllowed()
        if (run && !nearbyNeedsPermission) nearby.start() else nearby.stop()
    }

    /** This saved computer, if it is advertising on this network now. */
    fun seen(c: Computer): Found? = nearby.found.firstOrNull { it.isKey(c.fingerprint ?: c.expect) }

    /** Computers advertising here that this device doesn't have (by key, or by address). */
    val nearbyNew: List<Found>
        get() = nearby.found.filter { f ->
            store.computers.none { c -> f.isKey(c.fingerprint ?: c.expect) || (f.address != null && c.address.equals(f.address, ignoreCase = true)) }
        }

    /** Pair with a computer found nearby: add it at the address it was found at, and connect. */
    fun pairNearby(f: Found) {
        val address = f.address ?: return
        var c = store.add(f.name, address)
        if (f.cards.isNotEmpty()) c = c.copy(cards = f.cards.joinToString(",")).also(store::update)
        connect(c)
    }

    /** Keep the network cards of saved computers in sight, to wake them another day. */
    private fun rememberCards() {
        for (c in store.computers.toList()) {
            val f = seen(c) ?: continue
            val cards = f.cards.joinToString(",").ifEmpty { null } ?: continue
            if (c.cards != cards) store.update(c.copy(cards = cards))
        }
    }

    /** Whether waking [c] can be tried: its cards are known and it isn't in sight. */
    fun canWake(c: Computer): Boolean = c.cardList.isNotEmpty() && seen(c) == null

    /** Wake [c] with the magic packet, then connect ten seconds later. */
    fun wake(c: Computer) {
        val cards = c.cardList
        if (cards.isEmpty()) return
        withLocalNetwork({
            Thread {
                val sent = sendWake(cards, c.address)
                main.post { woken(c, sent) }
            }.start()
        }, {
            note = "remoteman needs to reach computers on this network. Allow it in Settings, Apps, remoteman, Permissions."
        })
    }

    private fun woken(c: Computer, sent: Int) {
        if (screen == Screen.Session && !session.active) leaveSession()
        if (sent == 0) {
            note = "${c.name} can't be woken from here: the waking packet couldn't be sent."
            return
        }
        cancelWake()
        note = null
        info = "Waking ${c.name}: connecting in ten seconds."
        val r = Runnable {
            wakeThenConnect = null
            info = null
            if (!session.active) store.get(c.id)?.let(::connect)
        }
        wakeThenConnect = r
        main.postDelayed(r, 10_000)
    }

    /** Don't connect after waking after all. */
    fun cancelWake() {
        wakeThenConnect?.let { main.removeCallbacks(it); info = null }
        wakeThenConnect = null
    }
}
