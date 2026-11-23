package com.ruxn.remoteman

import android.content.Context
import android.net.Uri
import androidx.compose.runtime.mutableStateListOf
import org.json.JSONArray
import org.json.JSONObject
import java.util.Calendar
import java.util.UUID

/**
 * A computer this device knows, kept on the device: what it's called, where it is, how this
 * device's display sits on it, and what it said about itself on the last connection.
 */
data class Computer(
    val id: String = UUID.randomUUID().toString(),
    val name: String,
    /** "name" or "name:port", as typed; the port is 7791 when left out. */
    val address: String,
    /** "linux", "macos", "windows", once the computer has said. */
    val os: String? = null,
    /** How this device's display sits on the computer; null follows the setting. */
    val placement: Placement? = null,
    val added: Long = System.currentTimeMillis(),
    val lastUsed: Long? = null,
    /** False until the first connection has gone through (which is when pairing happens). */
    val paired: Boolean = false,
    val fingerprint: String? = null,
    /** From a scanned pairing code: the key this computer must show on its first connection. */
    val expect: String? = null,
    /** Its network cards' hardware addresses (comma-separated), as last seen advertised: for waking it. */
    val cards: String? = null,
) {
    val cardList: List<String> get() = cards?.split(',')?.filter { it.isNotBlank() }.orEmpty()

    val osLine: String
        get() = when (os) {
            "linux" -> "Linux"
            "macos" -> "macOS"
            "windows" -> "Windows"
            null, "" -> address
            else -> os
        }

    /** "Used today, 09:12", "Used yesterday", "Used Friday", "Used 2 Nov"; null if never used. */
    fun usedLine(): String? {
        val used = lastUsed ?: return null
        val then = Calendar.getInstance().apply { timeInMillis = used }
        val now = Calendar.getInstance()
        val days = ((startOfDay(now) - startOfDay(then)) / 86_400_000L).toInt()
        val text = when {
            days == 0 -> "today, " + android.text.format.DateFormat.format("HH:mm", then)
            days == 1 -> "yesterday"
            days < 7 -> android.text.format.DateFormat.format("EEEE", then).toString()
            else -> android.text.format.DateFormat.format("d MMM", then).toString()
        }
        return "Used $text"
    }

    private fun startOfDay(c: Calendar): Long = (c.clone() as Calendar).apply {
        set(Calendar.HOUR_OF_DAY, 0); set(Calendar.MINUTE, 0); set(Calendar.SECOND, 0); set(Calendar.MILLISECOND, 0)
    }.timeInMillis

    fun toJson(): JSONObject = JSONObject().apply {
        put("id", id); put("name", name); put("address", address)
        os?.let { put("os", it) }
        placement?.let { put("placement", it.key) }
        put("added", added)
        lastUsed?.let { put("lastUsed", it) }
        put("paired", paired)
        fingerprint?.let { put("fingerprint", it) }
        expect?.let { put("expect", it) }
        cards?.let { put("cards", it) }
    }

    companion object {
        fun fromJson(o: JSONObject) = Computer(
            id = o.getString("id"),
            name = o.getString("name"),
            address = o.getString("address"),
            os = o.optString("os").ifEmpty { null },
            placement = Placement.fromKey(o.optString("placement")),
            added = o.optLong("added", System.currentTimeMillis()),
            lastUsed = if (o.has("lastUsed")) o.getLong("lastUsed") else null,
            paired = o.optBoolean("paired"),
            fingerprint = o.optString("fingerprint").ifEmpty { null },
            expect = o.optString("expect").ifEmpty { null },
            cards = o.optString("cards").ifEmpty { null },
        )
    }
}

/** How this device's display sits on the computer; [core] is what `Core.connect` takes. */
enum class Placement(val key: String, val core: Int, val title: String) {
    Mirror("mirror", 3, "Mirror its screen"),
    Extend("extend", 0, "An extra display"),
    Main("main", 1, "The main display"),
    Only("only", 2, "The only display");

    /** What the connecting screen says this device becomes. */
    fun becomes(device: String) = when (this) {
        Extend -> "This $device joins as an extra display"
        Main -> "This $device becomes the main display"
        Only -> "This $device becomes the only display"
        Mirror -> "This $device shows the computer's main monitor"
    }

    val detail: String
        get() = when (this) {
            Mirror -> "Shows what the computer's own monitor shows."
            Extend -> "A display of its own next to the computer's monitors."
            Main -> "The computer's monitors stay on; this one has the menu bar and new windows."
            Only -> "The computer's monitors go dark while you're connected."
        }

    companion object {
        fun fromKey(k: String?) = entries.firstOrNull { it.key == k }
    }
}

/** The computers this device knows, in SharedPreferences, as JSON. */
class ComputerStore(context: Context) {
    private val prefs = context.getSharedPreferences("computers", Context.MODE_PRIVATE)
    private val stateDir = context.filesDir.absolutePath
    val computers = mutableStateListOf<Computer>()

    init {
        runCatching {
            val a = JSONArray(prefs.getString("list", "[]"))
            for (i in 0 until a.length()) computers += Computer.fromJson(a.getJSONObject(i))
        }
    }

    private fun save() {
        val a = JSONArray()
        computers.forEach { a.put(it.toJson()) }
        prefs.edit().putString("list", a.toString()).apply()
    }

    fun add(name: String, address: String, expect: String? = null): Computer {
        // The same address again is the same computer: use the entry there is.
        computers.firstOrNull { it.address.equals(address, ignoreCase = true) }?.let { old ->
            val named = if (name.isNotBlank() && (old.name == old.address || expect == null)) name else old.name
            val c = old.copy(name = named, expect = expect ?: old.expect)
            update(c)
            return c
        }
        val c = Computer(name = name.ifBlank { address }, address = address, expect = expect)
        computers += c
        save()
        return c
    }

    fun update(c: Computer) {
        val i = computers.indexOfFirst { it.id == c.id }
        if (i < 0) return
        computers[i] = c
        save()
    }

    /** Gone from the list, and its key from this device, so it must be paired again. */
    fun forget(c: Computer) {
        computers.removeAll { it.id == c.id }
        if (computers.none { it.address == c.address }) Core.forgetHost(stateDir, c.address)
        save()
    }

    fun get(id: String?): Computer? = computers.firstOrNull { it.id == id }
}

/**
 * What a computer's pairing code (the QR code in "Pair a Device") says, as a link:
 *   remoteman://pair?v=0&name=NAME&addr=ADDRESS[:PORT][,ADDRESS[:PORT]…]&fp=FINGERPRINT
 * The device adds the computer at the first address and connects with pairing on; the
 * fingerprint turns away any other computer answering there.
 */
data class PairLink(val name: String, val addresses: List<String>, val fingerprint: String) {
    companion object {
        fun parse(text: String): PairLink? {
            val uri = runCatching { Uri.parse(text.trim()) }.getOrNull() ?: return null
            if (uri.scheme != "remoteman" || uri.host != "pair") return null
            val addresses = uri.getQueryParameter("addr").orEmpty().split(',').map { it.trim() }.filter { it.isNotEmpty() }
            val fp = uri.getQueryParameter("fp").orEmpty().lowercase()
            if (addresses.isEmpty() || fp.length < 32 || !fp.all { it in '0'..'9' || it in 'a'..'f' }) return null
            return PairLink(uri.getQueryParameter("name").orEmpty(), addresses, fp)
        }
    }
}
