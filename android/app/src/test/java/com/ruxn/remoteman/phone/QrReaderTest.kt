package com.ruxn.remoteman.phone

import com.google.zxing.BarcodeFormat
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.qrcode.QRCodeWriter
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** The scanner's reader on pictures made here: a QR code as a camera's brightness plane would hold it. */
class QrReaderTest {
    private val link = "remoteman://pair?host=192.168.1.20:7791&fp=4d7ef94db67297f9b31daecec8b4adcd5dfeffdf86418cf5a4d6f9123ea02027&name=BLACKBOX"

    /** [text] as a QR code in the middle of a [w]×[h] grey picture whose rows are [stride] bytes. */
    private fun picture(text: String, w: Int, h: Int, stride: Int, lightOnDark: Boolean = false): ByteArray {
        val code = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, 400, 400)
        val (dark, light) = if (lightOnDark) 230.toByte() to 25.toByte() else 25.toByte() to 230.toByte()
        val out = ByteArray(stride * h) { 128.toByte() }
        val x0 = (w - code.width) / 2
        val y0 = (h - code.height) / 2
        for (y in 0 until code.height) for (x in 0 until code.width) {
            out[(y0 + y) * stride + x0 + x] = if (code[x, y]) dark else light
        }
        return out
    }

    @Test fun readsACodeFromABrightnessPlaneWithPaddedRows() {
        val (w, h, stride) = Triple(640, 480, 704)
        val source = PlanarYUVLuminanceSource(picture(link, w, h, stride), stride, h, 0, 0, w, h, false)
        assertEquals(link, QrReader().read(source))
    }

    @Test fun readsALightOnDarkCode() {
        val (w, h) = 640 to 480
        val source = PlanarYUVLuminanceSource(picture(link, w, h, w, lightOnDark = true), w, h, 0, 0, w, h, false)
        assertEquals(link, QrReader().read(source))
    }

    @Test fun nothingWhereThereIsNoCode() {
        val (w, h) = 320 to 240
        val source = PlanarYUVLuminanceSource(ByteArray(w * h) { 128.toByte() }, w, h, 0, 0, w, h, false)
        assertNull(QrReader().read(source))
    }
}
