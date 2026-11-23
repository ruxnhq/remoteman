package com.xnixy.remoteman

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.xnixy.remoteman.phone.PhoneApp
import com.xnixy.remoteman.tv.TvApp

/**
 * The viewer. One activity for phones, tablets and TVs: the TV interface (Compose for TV, D-pad
 * focus) on devices with `android.software.leanback`, Material 3 elsewhere. Both drive the same
 * [AppModel].
 */
class MainActivity : ComponentActivity() {
    private lateinit var model: AppModel
    private var afterPermission: Pair<() -> Unit, () -> Unit>? = null

    private val localNetwork = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        val (then, refused) = afterPermission ?: return@registerForActivityResult
        afterPermission = null
        if (granted) then() else refused()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val isTv = packageManager.hasSystemFeature(PackageManager.FEATURE_LEANBACK)
        val store = ComputerStore(this)
        val session = Session(store, filesDir.absolutePath, deviceName).apply {
            deviceKind = when {
                isTv -> "TV"
                resources.configuration.smallestScreenWidthDp >= 600 -> "tablet"
                else -> "phone"
            }
        }
        model = AppModel(store, Prefs(this), session, isTv, ::withLocalNetwork, ::displayRequest)
        intent?.let(::openLink)
        setContent {
            if (isTv) TvApp(model) else PhoneApp(model)
            SessionWindow(model.screen == Screen.Session)
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openLink(intent)
    }

    /** remoteman://pair?… from a link or another app's scanner. */
    private fun openLink(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val text = intent.dataString ?: return
        val link = PairLink.parse(text)
        if (link == null) { model.note = "That link isn't a remoteman pairing code."; return }
        model.pairLink(link)
    }

    /** Android 17 asks before an app reaches the local network, where most computers are. */
    private fun withLocalNetwork(then: () -> Unit, refused: () -> Unit) {
        if (Build.VERSION.SDK_INT < 37 || checkSelfPermission(LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED) {
            then()
            return
        }
        afterPermission = then to refused
        localNetwork.launch(LOCAL_NETWORK)
    }

    /**
     * The display to ask the computer for. The emulator's decoders are software, so it asks for
     * 1280×720 at 60 unless told otherwise; devices ask for their own screen.
     */
    private fun displayRequest(): DisplayRequest {
        val prefs = model.prefs
        val b = windowManager.currentWindowMetrics.bounds
        val screenHz = display?.refreshRate?.toInt() ?: 60
        val (w, h) = when {
            prefs.resolution != Resolution.Screen -> prefs.resolution.width to prefs.resolution.height
            Decoders.isEmulator -> 1280 to 720
            else -> maxOf(b.width(), b.height()) to minOf(b.width(), b.height())
        }
        val hz = when {
            prefs.refresh != 0 -> prefs.refresh
            Decoders.isEmulator -> 60
            else -> screenHz
        }
        val scale = if (Decoders.isEmulator || prefs.resolution != Resolution.Screen) 1f else resources.displayMetrics.density
        return DisplayRequest(w, h, scale, hz)
    }

    /** While the session screen is up: full screen (immersive) and the screen kept on. */
    @Composable
    private fun SessionWindow(on: Boolean) {
        LaunchedEffect(on) {
            val c = WindowCompat.getInsetsController(window, window.decorView)
            if (on) {
                c.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                c.hide(WindowInsetsCompat.Type.systemBars())
                window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            } else {
                c.show(WindowInsetsCompat.Type.systemBars())
                window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            }
        }
    }

    /**
     * While the picture is up and its controls aren't, hardware keys go to the computer by
     * position (their Linux scan code). Back stays this app's: it opens the controls.
     */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val h = model.session.handle
        val toComputer = h != 0L && model.screen == Screen.Session && model.session.phase == Phase.Streaming && !model.controls
        if (toComputer && event.keyCode !in localKeys && event.scanCode != 0) {
            if (event.repeatCount > 0) return true // the computer repeats keys itself
            if (event.action == KeyEvent.ACTION_DOWN || event.action == KeyEvent.ACTION_UP) {
                if (Core.keyLinux(h, event.scanCode, event.action == KeyEvent.ACTION_DOWN)) return true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    override fun onStop() {
        super.onStop()
        model.appStopped()
    }

    companion object {
        const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
        /** Keys that stay with this device and its system. */
        val localKeys = setOf(
            KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_HOME, KeyEvent.KEYCODE_APP_SWITCH, KeyEvent.KEYCODE_POWER,
            KeyEvent.KEYCODE_VOLUME_UP, KeyEvent.KEYCODE_VOLUME_DOWN, KeyEvent.KEYCODE_VOLUME_MUTE,
            KeyEvent.KEYCODE_SETTINGS, KeyEvent.KEYCODE_GUIDE, KeyEvent.KEYCODE_TV_INPUT, KeyEvent.KEYCODE_ASSIST,
        )
    }
}
