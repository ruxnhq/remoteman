package com.ruxn.remoteman

import android.content.Context
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** The display this device asks for: its own screen's size, or one of these. */
enum class Resolution(val key: String, val title: String, val width: Int, val height: Int) {
    Screen("screen", "This screen's own", 0, 0),
    P720("720p", "1280 × 720", 1280, 720),
    P1080("1080p", "1920 × 1080", 1920, 1080),
    P1440("1440p", "2560 × 1440", 2560, 1440),
    P2160("2160p", "3840 × 2160", 3840, 2160);

    companion object {
        fun fromKey(k: String?) = entries.firstOrNull { it.key == k } ?: Screen
    }
}

/** 0 is the screen's own rate. */
val refreshChoices = listOf(0, 30, 60, 90, 120)

fun refreshTitle(hz: Int) = if (hz == 0) "This screen's own" else "$hz Hz"

/** The viewer's settings, kept on the device. */
class Prefs(context: Context) {
    private val p = context.getSharedPreferences("viewer", Context.MODE_PRIVATE)

    var placement by mutableStateOf(Placement.fromKey(p.getString("placement", null)) ?: Placement.Mirror)
        private set
    var resolution by mutableStateOf(Resolution.fromKey(p.getString("resolution", null)))
        private set
    var refresh by mutableStateOf(p.getInt("refresh", 0))
        private set
    var showStats by mutableStateOf(p.getBoolean("stats", false))
        private set
    /**
     * The computer's sound plays here. Off until chosen, as on the other viewers: turning it on
     * moves the computer's sound away from its own speakers.
     */
    var sound by mutableStateOf(p.getBoolean("sound", false))
        private set

    fun placement(v: Placement) { placement = v; p.edit().putString("placement", v.key).apply() }
    fun resolution(v: Resolution) { resolution = v; p.edit().putString("resolution", v.key).apply() }
    fun refresh(v: Int) { refresh = v; p.edit().putInt("refresh", v).apply() }
    fun showStats(v: Boolean) { showStats = v; p.edit().putBoolean("stats", v).apply() }
    fun sound(v: Boolean) { sound = v; p.edit().putBoolean("sound", v).apply() }
}

/** "0.1.0", from the package. */
@androidx.compose.runtime.Composable
fun appVersion(): String {
    val ctx = androidx.compose.ui.platform.LocalContext.current
    return runCatching { ctx.packageManager.getPackageInfo(ctx.packageName, 0).versionName }.getOrNull() ?: ""
}
