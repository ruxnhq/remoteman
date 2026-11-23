package com.ruxn.remoteman

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress

/** The same cases as the Linux viewer's (crates/remoteman-viewer/src/nearby.rs). */
class AdvertTest {
    private fun b(vararg v: Int) = ByteArray(v.size) { v[it].toByte() }

    @Test
    fun theWakingPacketIsAsCardsExpectIt() {
        val card = Advert.cardBytes("00:1a:2b:3c:4d:5e")!!
        assertArrayEquals(b(0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e), card)
        val packet = Advert.magic(card)
        assertEquals(102, packet.size)
        assertArrayEquals(ByteArray(6) { 0xFF.toByte() }, packet.copyOfRange(0, 6))
        for (i in 0 until 16) assertArrayEquals(card, packet.copyOfRange(6 + 6 * i, 12 + 6 * i))
        assertNull(Advert.cardBytes("00:1a:2b:3c:4d"))
        assertNull(Advert.cardBytes("00:1a:2b:3c:4d:5e:6f"))
        assertNull(Advert.cardBytes("zz:1a:2b:3c:4d:5e"))
        assertNull(Advert.cardBytes("0:1a:2b:3c:4d:5e"))
    }

    @Test
    fun theWakingPacketArrivesWhole() {
        // Sent, and received whole, on this computer alone.
        DatagramSocket(0, InetAddress.getLoopbackAddress()).use { listener ->
            listener.soTimeout = 2000
            val to = InetSocketAddress(InetAddress.getLoopbackAddress(), listener.localPort)
            assertEquals(1, Advert.wakeAt(listOf("00:1a:2b:3c:4d:5e", "not a card"), listOf(to)))
            val got = DatagramPacket(ByteArray(200), 200)
            listener.receive(got)
            assertArrayEquals(Advert.magic(Advert.cardBytes("00:1a:2b:3c:4d:5e")!!), got.data.copyOf(got.length))
        }
        assertEquals(0, Advert.wakeAt(listOf("junk"), listOf(InetSocketAddress(InetAddress.getLoopbackAddress(), 9))))
    }

    @Test
    fun records() {
        val (key, os, cards) = Advert.read(
            mapOf(
                "v" to "0".toByteArray(),
                "fp" to "841C275C001DB54D".toByteArray(),
                "os" to "linux".toByteArray(),
                "mac" to "AA:BB:CC:DD:EE:FF,junk,00:11:22:33:44:55".toByteArray(),
                "empty" to null,
            ),
        )
        assertEquals("841c275c001db54d", key)
        assertEquals("linux", os)
        assertEquals(listOf("aa:bb:cc:dd:ee:ff", "00:11:22:33:44:55"), cards)
        // Nothing there, or nonsense in fp.
        assertEquals(Triple("", "", emptyList<String>()), Advert.read(emptyMap()))
        assertEquals("", Advert.read(mapOf("fp" to "not hex!".toByteArray())).first)
    }

    @Test
    fun aKeyStartMatchesOnlyItsOwnKey() {
        val whole = "841c275c001db54d" + "0".repeat(48)
        assertTrue(Advert.sameKey("841C275C001DB54D", whole))
        assertTrue(Found("desk", key = "841c275c001db54d").isKey(whole.uppercase()))
        assertFalse(Advert.sameKey("841c275c001db54e", whole))
        // Too short, or none, is never a match.
        assertFalse(Advert.sameKey("", whole))
        assertFalse(Advert.sameKey("841c275c", whole))
        assertFalse(Advert.sameKey("841c275c001db54d", null))
    }

    @Test
    fun theBestAddress() {
        val v4 = InetAddress.getByName("192.168.1.5")
        val ula = InetAddress.getByName("fd00::2")
        val link = InetAddress.getByName("fe80::1")
        assertEquals("192.168.1.5:7791", Advert.bestAddress(listOf(link, ula, v4), 7791))
        assertEquals("[fd00:0:0:0:0:0:0:2]:7791", Advert.bestAddress(listOf(link, ula), 7791))
        assertNull(Advert.bestAddress(listOf(link), 7791))
        assertNull(Advert.bestAddress(emptyList(), 7791))
    }

    @Test
    fun broadcastAddresses() {
        assertArrayEquals(b(192, 168, 1, 255), Advert.broadcast(b(192, 168, 1, 5), 24))
        assertArrayEquals(b(10, 0, 3, 255), Advert.broadcast(b(10, 0, 0, 76), 22))
        assertArrayEquals(b(10, 0, 2, 15), Advert.broadcast(b(10, 0, 2, 15), 32))
        assertArrayEquals(b(255, 255, 255, 255), Advert.broadcast(b(10, 0, 2, 15), 0))
    }

    @Test
    fun escapedNames() {
        assertEquals("Anna's desk", Advert.unescape("Anna's\\032desk"))
        assertEquals("a.b", Advert.unescape("a\\.b"))
        assertEquals("blackbox", Advert.unescape("blackbox"))
        assertEquals("Café", Advert.unescape("Caf\\195\\169"))
    }
}
