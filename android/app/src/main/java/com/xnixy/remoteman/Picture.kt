package com.xnixy.remoteman

import android.content.Context
import android.text.InputType
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView

/**
 * The computer's picture: a SurfaceView the Rust decoder draws into, the largest box of the
 * stream's proportions that fits. Touches go to the computer as touches, a mouse as a mouse.
 * The connection starts once its surface is there and ends when it goes.
 */
@Composable
fun Picture(model: AppModel) {
    val (w, h) = model.session.streamSize
    BoxWithConstraints(Modifier.fillMaxSize().background(Color.Black), contentAlignment = Alignment.Center) {
        val box = if (w > 0 && h > 0) {
            val s = minOf(maxWidth.value / w, maxHeight.value / h)
            Modifier.size((w * s).dp, (h * s).dp)
        } else Modifier.fillMaxSize()
        AndroidView(
            modifier = box,
            factory = { ctx ->
                SurfaceView(ctx).apply {
                    holder.addCallback(object : SurfaceHolder.Callback {
                        override fun surfaceCreated(holder: SurfaceHolder) = model.surfaceReady(holder.surface)
                        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
                        override fun surfaceDestroyed(holder: SurfaceHolder) = model.surfaceGone()
                    })
                    setOnTouchListener { v, e ->
                        if (e.isFromSource(InputDevice.SOURCE_MOUSE)) mouse(model, v, e) else touch(model, v, e)
                        true
                    }
                    setOnGenericMotionListener { v, e -> e.isFromSource(InputDevice.SOURCE_MOUSE) && mouse(model, v, e) }
                }
            },
        )
        KeyCatcher(model)
    }
}

private fun touch(model: AppModel, v: View, e: MotionEvent) {
    val h = model.session.handle
    if (h == 0L || v.width == 0 || v.height == 0) return
    // A touch on the picture puts the controls away.
    if (e.actionMasked == MotionEvent.ACTION_DOWN && model.controls) model.controls = false
    fun send(phase: Int, i: Int) =
        Core.touch(h, phase, e.getPointerId(i), (e.getX(i) / v.width).coerceIn(0f, 1f), (e.getY(i) / v.height).coerceIn(0f, 1f))
    when (e.actionMasked) {
        MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> send(0, e.actionIndex)
        MotionEvent.ACTION_MOVE -> for (i in 0 until e.pointerCount) send(1, i)
        MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP, MotionEvent.ACTION_CANCEL -> send(2, e.actionIndex)
    }
}

/** A mouse over the picture: the pointer follows, buttons and the wheel go to the computer. */
private fun mouse(model: AppModel, v: View, e: MotionEvent): Boolean {
    val h = model.session.handle
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
        // One notch is about three lines.
        MotionEvent.ACTION_SCROLL ->
            Core.scroll(h, e.getAxisValue(MotionEvent.AXIS_HSCROLL) * 48f, -e.getAxisValue(MotionEvent.AXIS_VSCROLL) * 48f)
    }
    return true
}

/**
 * Where the on-screen keyboard types while [AppModel.keyboard] is on: an invisible editor
 * whose text goes straight to the computer (as text; Enter, Tab and Backspace as keys).
 */
@Composable
private fun KeyCatcher(model: AppModel) {
    val on = model.keyboard
    Box(Modifier.size(1.dp)) {
        AndroidView(
            factory = { ctx -> KeyCatcherView(ctx, model) },
            update = { v ->
                val imm = v.context.getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
                if (on) {
                    v.isFocusableInTouchMode = true
                    v.requestFocus()
                    v.post { imm.showSoftInput(v, 0) }
                } else {
                    imm.hideSoftInputFromWindow(v.windowToken, 0)
                    v.clearFocus()
                }
            },
        )
    }
}

private class KeyCatcherView(context: Context, val model: AppModel) : View(context) {
    init { isFocusable = true }

    override fun onCheckIsTextEditor() = true

    private fun tap(usage: Int) {
        val h = model.session.handle
        if (h == 0L) return
        Core.key(h, usage, true)
        Core.key(h, usage, false)
    }

    private fun type(text: CharSequence) {
        val h = model.session.handle
        if (h == 0L) return
        val run = StringBuilder()
        fun flush() { if (run.isNotEmpty()) Core.text(h, run.toString()); run.clear() }
        for (ch in text) when (ch) {
            '\n', '\r' -> { flush(); tap(USAGE_ENTER) }
            '\t' -> { flush(); tap(USAGE_TAB) }
            else -> run.append(ch)
        }
        flush()
    }

    /** Keys without a scan code (an input method's own, or injected ones) arrive here. */
    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        when (keyCode) {
            KeyEvent.KEYCODE_DEL -> tap(USAGE_BACKSPACE)
            KeyEvent.KEYCODE_FORWARD_DEL -> tap(USAGE_DELETE)
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> tap(USAGE_ENTER)
            KeyEvent.KEYCODE_TAB -> tap(USAGE_TAB)
            else -> {
                val ch = event.unicodeChar
                if (ch <= 0) return super.onKeyDown(keyCode, event)
                type(ch.toChar().toString())
            }
        }
        return true
    }

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        // No suggestions and no composing: what's typed is committed, and sent, at once.
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean { type(text); return true }
            override fun setComposingText(text: CharSequence, newCursorPosition: Int) = true
            override fun finishComposingText() = true
            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(beforeLength.coerceIn(0, 64)) { tap(USAGE_BACKSPACE) }
                repeat(afterLength.coerceIn(0, 64)) { tap(USAGE_DELETE) }
                return true
            }
            override fun sendKeyEvent(event: KeyEvent): Boolean {
                if (event.action != KeyEvent.ACTION_DOWN) return true
                when (event.keyCode) {
                    KeyEvent.KEYCODE_DEL -> tap(USAGE_BACKSPACE)
                    KeyEvent.KEYCODE_FORWARD_DEL -> tap(USAGE_DELETE)
                    KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> tap(USAGE_ENTER)
                    KeyEvent.KEYCODE_TAB -> tap(USAGE_TAB)
                    else -> event.unicodeChar.takeIf { it > 0 }?.let { type(it.toChar().toString()) }
                }
                return true
            }
            override fun performEditorAction(editorAction: Int): Boolean { tap(USAGE_ENTER); return true }
        }
    }

    companion object {
        const val USAGE_ENTER = 0x28
        const val USAGE_BACKSPACE = 0x2A
        const val USAGE_TAB = 0x2B
        const val USAGE_DELETE = 0x4C
    }
}
