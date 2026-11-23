package com.ruxn.remoteman

import android.content.Context
import android.net.ConnectivityManager
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.net.Inet4Address
import java.net.InetAddress
import java.net.InetSocketAddress

/**
 * Computers on this network that say they run remoteman (`_remoteman._udp`, docs/PROTOCOL.md,
 * "Finding a host"), through the system's NsdManager. Only while asked to ([start] / [stop]):
 * the app looks while the computers screen is in sight. What is found is "seen on this network
 * now"; a paired computer that isn't found may be reached another way, so that says nothing.
 *
 * Its state is Compose state, changed on the main thread only.
 */
class Nearby(context: Context) {
    private val app = context.applicationContext
    private val nsd = app.getSystemService(NsdManager::class.java)
    private val main = Handler(Looper.getMainLooper())

    /** One entry per advertised name, in name order. */
    val found = mutableStateListOf<Found>()
    var looking by mutableStateOf(false)
        private set
    /** Called (main thread) when [found] has changed. */
    var changed: () -> Unit = {}

    private var lock: WifiManager.MulticastLock? = null
    private var listener: NsdManager.DiscoveryListener? = null
    /** Bumped on every start and stop: callbacks of an earlier look are dropped. */
    private var generation = 0
    /** Android 14+: one updating resolver per service name. */
    private val watching = mutableMapOf<String, NsdManager.ServiceInfoCallback>()
    /** Older: one resolve at a time (two at once fail with FAILURE_ALREADY_ACTIVE). */
    private val queue = ArrayDeque<NsdServiceInfo>()
    private var resolving = false

    fun start() {
        if (listener != null) return
        val gen = ++generation
        // Wi-Fi drops multicast to save power unless an app holds this; mDNS is multicast.
        lock = runCatching {
            app.getSystemService(WifiManager::class.java)?.createMulticastLock("remoteman nearby")?.apply {
                setReferenceCounted(false)
                acquire()
            }
        }.getOrNull()
        val l = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(type: String) = post(gen) { looking = true }
            override fun onDiscoveryStopped(type: String) = post(gen) { looking = false }
            override fun onStartDiscoveryFailed(type: String, error: Int) = post(gen) {
                Log.w(TAG, "nearby: not looking (error $error)")
                looking = false
                listener = null
            }
            override fun onStopDiscoveryFailed(type: String, error: Int) {}
            override fun onServiceFound(info: NsdServiceInfo) = post(gen) { seen(gen, info) }
            override fun onServiceLost(info: NsdServiceInfo) = post(gen) { lost(Advert.unescape(info.serviceName)) }
        }
        listener = l
        runCatching { nsd.discoverServices(Advert.TYPE, NsdManager.PROTOCOL_DNS_SD, l) }.onFailure {
            Log.w(TAG, "nearby: not looking ($it)")
            listener = null
        }
    }

    fun stop() {
        generation++
        listener?.let { l -> runCatching { nsd.stopServiceDiscovery(l) } }
        listener = null
        if (Build.VERSION.SDK_INT >= 34) watching.values.forEach { cb -> runCatching { nsd.unregisterServiceInfoCallback(cb) } }
        watching.clear()
        queue.clear()
        resolving = false
        runCatching { lock?.release() }
        lock = null
        looking = false
        if (found.isNotEmpty()) {
            found.clear()
            changed()
        }
    }

    private fun post(gen: Int, f: () -> Unit) {
        main.post { if (gen == generation) f() }
    }

    private fun seen(gen: Int, info: NsdServiceInfo) {
        val name = Advert.unescape(info.serviceName)
        if (found.none { it.name == name }) put(Found(name))
        if (Build.VERSION.SDK_INT >= 34) {
            if (name in watching) return
            val cb = object : NsdManager.ServiceInfoCallback {
                override fun onServiceInfoCallbackRegistrationFailed(error: Int) = post(gen) { watching.remove(name) }
                override fun onServiceUpdated(info: NsdServiceInfo) = post(gen) { resolved(info, info.hostAddresses) }
                // Gone for now: the callback stays, so it comes back if it does.
                override fun onServiceLost() = post(gen) { if (found.removeAll { it.name == name }) changed() }
                override fun onServiceInfoCallbackUnregistered() {}
            }
            watching[name] = cb
            runCatching { nsd.registerServiceInfoCallback(info, { it.run() }, cb) }.onFailure { watching.remove(name) }
        } else {
            queue.addLast(info)
            resolveNext(gen)
        }
    }

    @Suppress("DEPRECATION")
    private fun resolveNext(gen: Int) {
        if (resolving) return
        val info = queue.removeFirstOrNull() ?: return
        resolving = true
        val done = { main.post { if (gen == generation) { resolving = false; resolveNext(gen) } } }
        runCatching {
            nsd.resolveService(info, object : NsdManager.ResolveListener {
                override fun onResolveFailed(info: NsdServiceInfo, error: Int) { done() }
                override fun onServiceResolved(info: NsdServiceInfo) {
                    post(gen) { resolved(info, listOfNotNull(info.host)) }
                    done()
                }
            })
        }.onFailure { resolving = false }
    }

    private fun resolved(info: NsdServiceInfo, addresses: List<InetAddress>) {
        val name = Advert.unescape(info.serviceName)
        val (key, os, cards) = Advert.read(info.attributes ?: emptyMap())
        put(Found(name, Advert.bestAddress(addresses, info.port), key, os, cards))
    }

    private fun put(f: Found) {
        val i = found.indexOfFirst { it.name == f.name }
        if (i >= 0) {
            if (found[i] == f) return
            found[i] = f
        } else {
            val at = found.indexOfFirst { it.name.compareTo(f.name, ignoreCase = true) > 0 }
            if (at < 0) found += f else found.add(at, f)
        }
        changed()
    }

    private fun lost(name: String) {
        if (Build.VERSION.SDK_INT >= 34) watching.remove(name)?.let { cb -> runCatching { nsd.unregisterServiceInfoCallback(cb) } }
        if (found.removeAll { it.name == name }) changed()
    }

    companion object {
        const val TAG = "remoteman"

        /**
         * Wake the computer with these network cards, if it sleeps on the network this device is
         * on: the packet goes to the network's broadcast address (and 255.255.255.255) on ports 9
         * and 7, and to [lastAddress] if that's an IPv4 number. Blocking: not on the main thread.
         * Returns how many packets left.
         */
        fun wake(context: Context, cards: List<String>, lastAddress: String?): Int {
            val targets = linkedSetOf<InetAddress>(InetAddress.getByAddress(byteArrayOf(-1, -1, -1, -1)))
            runCatching {
                val cm = context.getSystemService(ConnectivityManager::class.java)
                cm.getLinkProperties(cm.activeNetwork)?.linkAddresses?.forEach { la ->
                    val a = la.address
                    if (a is Inet4Address && !a.isLoopbackAddress) targets += InetAddress.getByAddress(Advert.broadcast(a.address, la.prefixLength))
                }
            }
            lastAddress?.substringBeforeLast(':')?.takeIf { h -> h.count { it == '.' } == 3 && h.all { it.isDigit() || it == '.' } }?.let { h ->
                runCatching { targets += InetAddress.getByName(h) }
            }
            Log.i(TAG, "waking ${cards.joinToString()} at ${targets.joinToString { it.hostAddress ?: "" }}")
            return Advert.wakeAt(cards, targets.flatMap { listOf(InetSocketAddress(it, 9), InetSocketAddress(it, 7)) })
        }
    }
}
