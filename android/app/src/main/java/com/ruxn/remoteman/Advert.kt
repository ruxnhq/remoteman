package com.ruxn.remoteman

import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress

// What a host's advertisement says, and the packet that wakes it (docs/PROTOCOL.md, "Finding a
// host"). No Android here, so the unit tests run on the computer.

/**
 * A computer seen on this network now: its advertised name, `address:port` as the core takes
 * it (null until resolved), the start of its key's fingerprint, what it runs, and the hardware
 * addresses of its network cards.
 */
data class Found(
    val name: String,
    val address: String? = null,
    val key: String = "",
    val os: String = "",
    val cards: List<String> = emptyList(),
) {
    /** Whether this is the computer with [fingerprint] (the whole one this device keeps). */
    fun isKey(fingerprint: String?): Boolean = Advert.sameKey(key, fingerprint)

    val osLine: String
        get() = when (os) {
            "linux" -> "Linux"
            "macos" -> "macOS"
            "windows" -> "Windows"
            else -> os
        }
}

object Advert {
    const val TYPE = "_remoteman._udp"

    /** The text records `fp`, `os` and `mac` (as NsdServiceInfo gives them): key start, system, cards. */
    fun read(records: Map<String, ByteArray?>): Triple<String, String, List<String>> {
        fun text(k: String) = records.entries.firstOrNull { it.key.equals(k, ignoreCase = true) }?.value?.toString(Charsets.UTF_8)?.trim().orEmpty()
        val key = text("fp").lowercase().takeIf { k -> k.all { it in '0'..'9' || it in 'a'..'f' } }.orEmpty()
        val cards = text("mac").split(',').map { it.trim().lowercase() }.filter { cardBytes(it) != null }
        return Triple(key, text("os"), cards)
    }

    /**
     * Whether an advertised key start belongs to [fingerprint]. A start shorter than 16 hex
     * digits (or none) matches nothing: never "online" on a guess.
     */
    fun sameKey(start: String, fingerprint: String?): Boolean {
        val f = fingerprint?.lowercase() ?: return false
        return start.length >= 16 && f.startsWith(start.lowercase())
    }

    /** DNS-SD names arrive with some bytes escaped as `\DDD` (a space as `\032`): undo that. */
    fun unescape(name: String): String {
        if ('\\' !in name) return name
        val bytes = java.io.ByteArrayOutputStream()
        var i = 0
        while (i < name.length) {
            val ch = name[i]
            if (ch == '\\' && i + 4 <= name.length && name.substring(i + 1, i + 4).all { it in '0'..'9' }) {
                bytes.write(name.substring(i + 1, i + 4).toInt())
                i += 4
            } else if (ch == '\\' && i + 1 < name.length) {
                bytes.write(name.substring(i + 1, i + 2).toByteArray())
                i += 2
            } else {
                bytes.write(ch.toString().toByteArray())
                i += 1
            }
        }
        return bytes.toString(Charsets.UTF_8.name())
    }

    /**
     * Which of a service's addresses to connect to, as `address:port`: IPv4 first, then IPv6
     * that isn't link-local (which would need an interface named with it), in brackets.
     */
    fun bestAddress(addresses: List<InetAddress>, port: Int): String? {
        addresses.firstOrNull { it is Inet4Address }?.let { return "${it.hostAddress}:$port" }
        addresses.firstOrNull { it is Inet6Address && !it.isLinkLocalAddress && !it.isLoopbackAddress }?.let {
            return "[${it.hostAddress?.substringBefore('%')}]:$port"
        }
        return null
    }

    /** A hardware address (`aa:bb:cc:dd:ee:ff`) as its six bytes. */
    fun cardBytes(card: String): ByteArray? {
        val parts = card.split(':')
        if (parts.size != 6 || parts.any { it.length != 2 }) return null
        return ByteArray(6) { (parts[it].toIntOrNull(16) ?: return null).toByte() }
    }

    /** The packet that wakes a sleeping computer: six bytes of 0xFF, then the card's address sixteen times. */
    fun magic(card: ByteArray): ByteArray {
        require(card.size == 6)
        return ByteArray(6) { 0xFF.toByte() } + ByteArray(96) { card[it % 6] }
    }

    /** The broadcast address of the IPv4 network [address]/[prefix] (192.168.1.5/24 → 192.168.1.255). */
    fun broadcast(address: ByteArray, prefix: Int): ByteArray {
        require(address.size == 4 && prefix in 0..32)
        val ip = address.fold(0L) { acc, b -> (acc shl 8) or (b.toLong() and 0xFF) }
        val host = if (prefix == 32) 0L else (1L shl (32 - prefix)) - 1
        val b = ip or host
        return ByteArray(4) { ((b shr (24 - 8 * it)) and 0xFF).toByte() }
    }

    /**
     * Send the waking packet for each card to each target (on its own port). Blocking: not on
     * the main thread. Returns how many packets left.
     */
    fun wakeAt(cards: List<String>, targets: List<InetSocketAddress>): Int {
        val packets = cards.mapNotNull { cardBytes(it) }.map { magic(it) }
        if (packets.isEmpty() || targets.isEmpty()) return 0
        var sent = 0
        runCatching {
            DatagramSocket().use { s ->
                s.broadcast = true
                for (to in targets) for (p in packets) {
                    if (runCatching { s.send(DatagramPacket(p, p.size, to)) }.isSuccess) sent++
                }
            }
        }
        return sent
    }
}
