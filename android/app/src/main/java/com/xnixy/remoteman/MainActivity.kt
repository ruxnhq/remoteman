package com.xnixy.remoteman

import android.app.Activity
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Gravity
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView

/**
 * The first spike: an address, Connect, and the computer's picture filling the screen, with a
 * line of statistics. Touches go to the computer as touches.
 */
class MainActivity : Activity(), Core.Listener {
    private val main = Handler(Looper.getMainLooper())
    private lateinit var picture: SurfaceView
    private lateinit var form: LinearLayout
    private lateinit var address: EditText
    private lateinit var mirror: CheckBox
    private lateinit var connectButton: Button
    private lateinit var status: TextView
    private var surfaceReady = false
    @Volatile private var handle = 0L
    private var streamSize = 0 to 0
    private var pendingHost: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }

        picture = SurfaceView(this)
        root.addView(picture, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT, Gravity.CENTER))
        picture.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) { surfaceReady = true }
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
            override fun surfaceDestroyed(holder: SurfaceHolder) {
                surfaceReady = false
                disconnect()
            }
        })
        picture.setOnTouchListener { v, e -> if (e.isFromSource(InputDevice.SOURCE_MOUSE)) mouse(v, e) else touch(v, e); true }
        picture.setOnGenericMotionListener { v, e -> e.isFromSource(InputDevice.SOURCE_MOUSE) && mouse(v, e) }

        val prefs = getSharedPreferences("viewer", MODE_PRIVATE)
        form = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 48, 48, 48)
            setBackgroundColor(0xCC202020.toInt())
        }
        address = EditText(this).apply {
            hint = "computer:port"
            setSingleLine()
            setText(prefs.getString("address", if (Decoders.isEmulator) "10.0.2.2:7799" else ""))
            // The keyboard's own key connects: on a TV it covers the Connect button.
            imeOptions = android.view.inputmethod.EditorInfo.IME_ACTION_GO
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_URI
            setOnEditorActionListener { _, _, _ -> start(); true }
        }
        connectButton = Button(this).apply {
            text = "Connect"
            setOnClickListener { start() }
        }
        form.addView(TextView(this).apply { text = "remoteman\n\n" + Decoders.describe(); setTextColor(Color.WHITE) })
        form.addView(address)
        mirror = CheckBox(this).apply {
            text = "Mirror the computer's screen (instead of an extra display)"
            setTextColor(Color.WHITE)
            isChecked = prefs.getBoolean("mirror", true)
            setOnCheckedChangeListener { _, on -> prefs.edit().putBoolean("mirror", on).apply() }
        }
        form.addView(mirror)
        form.addView(connectButton)
        root.addView(form, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.CENTER))

        status = TextView(this).apply {
            setTextColor(Color.WHITE)
            setShadowLayer(4f, 0f, 0f, Color.BLACK)
            setPadding(24, 24, 24, 24)
        }
        root.addView(status, FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.TOP or Gravity.START))
        setContentView(root)
        // With an address already there, start on Connect, so a TV remote's OK connects at once
        // instead of opening the on-screen keyboard.
        if (address.text.isNotBlank()) connectButton.requestFocus()
    }

    private fun start() {
        val host = address.text.toString().trim()
        getSharedPreferences("viewer", MODE_PRIVATE).edit().putString("address", host).apply()
        (getSystemService(INPUT_METHOD_SERVICE) as android.view.inputmethod.InputMethodManager).hideSoftInputFromWindow(address.windowToken, 0)
        connect(host)
    }

    private fun connect(host: String) {
        if (host.isEmpty() || handle != 0L) return
        // Android 17 asks before an app reaches the local network, where most computers are.
        if (Build.VERSION.SDK_INT >= 37 && checkSelfPermission(LOCAL_NETWORK) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            pendingHost = host
            requestPermissions(arrayOf(LOCAL_NETWORK), 1)
            return
        }
        if (!surfaceReady) { status.text = "The picture isn't ready yet"; return }
        form.visibility = View.GONE
        status.text = "Connecting to $host…"
        // A small display on the emulator, whose decoders are software; the screen's own size elsewhere.
        val (w, h, hz) = if (Decoders.isEmulator) Triple(1280, 720, 60) else {
            val b = windowManager.currentWindowMetrics.bounds
            Triple(maxOf(b.width(), b.height()), minOf(b.width(), b.height()), display?.refreshRate?.toInt() ?: 60)
        }
        val scale = if (Decoders.isEmulator) 1f else resources.displayMetrics.density
        val decoders = Decoders.list()
        val placement = if (mirror.isChecked) 3 else 0
        val surface = picture.holder.surface
        val state = filesDir.absolutePath
        Thread {
            Log.i(TAG, "connecting to $host as ${w}x$h at $hz Hz, scale $scale, decoders $decoders")
            val h2 = Core.connect(host, state, Build.MODEL, true, w, h, scale, hz, decoders, placement, surface, this)
            handle = h2
            if (h2 != 0L) main.post { status.text = Core.host(h2)?.split('\t')?.first() ?: ""; tick() }
        }.start()
    }

    private fun disconnect() {
        val h = handle
        handle = 0L
        if (h != 0L) Thread { Core.close(h) }.start()
    }

    private fun tick() {
        val h = handle
        if (h == 0L) return
        Core.stats(h)?.let { s ->
            val age = if (s[5] >= 0) "%.1f ms".format(s[5] / 1000.0) else "–"
            val text = "shown ${s[3]}  dropped ${s[4]}  lost ${s[0]}  keyframes asked ${s[1]}  round trip %.1f ms  age $age".format(s[2] / 1000.0)
            status.text = text
        }
        main.postDelayed({ tick() }, 1000)
    }

    private fun touch(v: View, e: MotionEvent) {
        val h = handle
        if (h == 0L || v.width == 0 || v.height == 0) return
        fun send(phase: Int, i: Int) =
            Core.touch(h, phase, e.getPointerId(i), (e.getX(i) / v.width).coerceIn(0f, 1f), (e.getY(i) / v.height).coerceIn(0f, 1f))
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> send(0, e.actionIndex)
            MotionEvent.ACTION_MOVE -> for (i in 0 until e.pointerCount) send(1, i)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP, MotionEvent.ACTION_CANCEL -> send(2, e.actionIndex)
        }
    }

    /** A mouse over the picture: the pointer follows, buttons and the wheel go to the computer. */
    private fun mouse(v: View, e: MotionEvent): Boolean {
        val h = handle
        if (h == 0L || v.width == 0 || v.height == 0) return false
        Core.pointerTo(h, (e.x / v.width).coerceIn(0f, 1f), (e.y / v.height).coerceIn(0f, 1f))
        when (e.actionMasked) {
            MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_BUTTON_RELEASE -> {
                val b = when (e.actionButton) {
                    MotionEvent.BUTTON_SECONDARY -> 1
                    MotionEvent.BUTTON_TERTIARY -> 2
                    else -> 0
                }
                Core.button(h, b, e.actionMasked == MotionEvent.ACTION_BUTTON_PRESS)
            }
            MotionEvent.ACTION_SCROLL -> {
                // One notch is about three lines.
                Core.scroll(h, e.getAxisValue(MotionEvent.AXIS_HSCROLL) * 48f, -e.getAxisValue(MotionEvent.AXIS_VSCROLL) * 48f)
            }
        }
        return true
    }

    /** While connected, hardware keys go to the computer by position. */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val h = handle
        if (h != 0L && form.visibility != View.VISIBLE && event.scanCode != 0 && event.repeatCount == 0 &&
            (event.action == KeyEvent.ACTION_DOWN || event.action == KeyEvent.ACTION_UP)) {
            if (Core.keyLinux(h, event.scanCode, event.action == KeyEvent.ACTION_DOWN)) return true
        }
        if (h != 0L && event.scanCode != 0 && event.repeatCount > 0) return true
        return super.dispatchKeyEvent(event)
    }

    /** Keep the picture's shape: the largest box of the stream's proportions that fits. */
    private fun fit() {
        val (w, h) = streamSize
        val root = picture.parent as View
        if (w == 0 || h == 0 || root.width == 0) return
        val s = minOf(root.width.toFloat() / w, root.height.toFloat() / h)
        picture.layoutParams = FrameLayout.LayoutParams((w * s).toInt(), (h * s).toInt(), Gravity.CENTER)
    }

    override fun onStage(stage: Int) {
        val text = arrayOf("Looking the computer up…", "Reaching it…", "Saying hello…", "The computer is making a display…").getOrElse(stage) { "" }
        main.post { status.text = text }
    }

    override fun onPairing(code: String, seconds: Int) {
        main.post { status.text = "Confirm this device on the computer.\nCode: $code ($seconds s)" }
    }

    override fun onStream(description: String, width: Int, height: Int) {
        Log.i(TAG, "stream: $description")
        main.post { streamSize = width to height; fit() }
    }

    override fun onClosed(reason: String) {
        Log.i(TAG, "closed: $reason")
        main.post {
            handle = 0L
            status.text = reason
            form.visibility = View.VISIBLE
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, results: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, results)
        val host = pendingHost ?: return
        pendingHost = null
        if (results.firstOrNull() == android.content.pm.PackageManager.PERMISSION_GRANTED) connect(host)
        else status.text = "remoteman needs to reach computers on this network: allow it in Settings, Apps, remoteman, Permissions"
    }

    override fun onStop() {
        super.onStop()
        disconnect()
    }

    companion object {
        const val TAG = "remoteman"
        const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
    }
}
