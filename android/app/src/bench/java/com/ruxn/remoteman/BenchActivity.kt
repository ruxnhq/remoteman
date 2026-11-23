package com.ruxn.remoteman

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Color
import android.graphics.ImageFormat
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.util.Log
import android.view.Gravity
import android.view.PixelCopy
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.TextView
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.math.log10

/**
 * The decoder benchmark, for devices nobody here has: the recorded streams in the bench build's
 * assets (android/bench) are played three ways each, and the results go to the screen, to
 * logcat (tag remoteman-bench), to files/bench.json, and, when Firebase Test Lab runs this as a
 * game loop, to the file it gives.
 *
 * 1. timing: through the app's own decoder (Rust, MediaCodec into a Surface) at the stream's
 *    rate, each picture timed from being handed over to its release to the screen;
 * 2. screen: what the screen then shows, copied and compared with FFmpeg's picture of the same
 *    frame (PSNR; the emulators fail this: they show 10-bit pictures as 8-bit);
 * 3. pixels: decoded into memory, each frame's MD5 compared with FFmpeg's (bit-exact or not).
 */
class BenchActivity : Activity() {
    private val main = Handler(Looper.getMainLooper())
    private lateinit var picture: SurfaceView
    private lateinit var text: TextView
    private val log = StringBuilder()
    private val surfaceReady = CountDownLatch(1)
    private lateinit var share: android.widget.Button
    @Volatile private var resultJson = ""

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
        picture = SurfaceView(this)
        root.addView(picture, FrameLayout.LayoutParams(0, 0, Gravity.CENTER))
        picture.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) = surfaceReady.countDown()
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}
            override fun surfaceDestroyed(holder: SurfaceHolder) {}
        })
        text = TextView(this).apply {
            setTextColor(Color.WHITE)
            setShadowLayer(4f, 0f, 0f, Color.BLACK)
            textSize = 11f
            setPadding(16, 16, 16, 16)
        }
        root.addView(text, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
        // For a tester: send the results by mail, chat, anything that takes text.
        share = android.widget.Button(this).apply {
            text = "Share results"
            visibility = android.view.View.GONE
            setOnClickListener {
                val send = android.content.Intent(android.content.Intent.ACTION_SEND).setType("text/plain")
                    .putExtra(android.content.Intent.EXTRA_SUBJECT, "remoteman bench: ${Build.MANUFACTURER} ${Build.MODEL}")
                    .putExtra(android.content.Intent.EXTRA_TEXT, resultJson)
                startActivity(android.content.Intent.createChooser(send, "Share results"))
            }
        }
        root.addView(share, FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM or Gravity.END))
        setContentView(root)
        // The picture: 16:9, as large as fits.
        root.post {
            val s = minOf(root.width / 16f, root.height / 9f)
            picture.layoutParams = FrameLayout.LayoutParams((16 * s).toInt(), (9 * s).toInt(), Gravity.CENTER)
        }
        Thread { run() }.start()
    }

    private fun say(line: String) {
        Log.i(TAG, line)
        log.append(line).append('\n')
        val shown = log.lines().takeLast(40).joinToString("\n")
        main.post { text.text = shown }
    }

    private fun run() {
        val result = JSONObject()
        try {
            surfaceReady.await(10, TimeUnit.SECONDS)
            Thread.sleep(500)
            result.put("device", device())
            result.put("decoders", decoders())
            say("${Build.MANUFACTURER} ${Build.MODEL}, Android ${Build.VERSION.RELEASE}")
            val runs = JSONArray()
            val names = assets.list("recordings").orEmpty().filter { it.endsWith(".rmrec") }.map { it.removeSuffix(".rmrec") }.sorted()
            for (name in names) runs.put(recording(name))
            result.put("runs", runs)
        } catch (e: Throwable) {
            Log.e(TAG, "benchmark", e)
            result.put("error", e.toString())
        }
        val json = result.toString(2)
        File(filesDir, "bench.json").writeText(json)
        json.lines().forEach { Log.i(TAG, "json $it") }
        say("done: files/bench.json")
        main.post { share.visibility = android.view.View.VISIBLE; share.requestFocus() }
        resultJson = json
        intent?.takeIf { it.action == "com.google.intent.action.TEST_LOOP" }?.data?.let { uri ->
            contentResolver.openAssetFileDescriptor(uri, "w")?.createOutputStream()?.use { it.write(json.toByteArray()) }
            main.postDelayed({ finish() }, 2000)
        }
    }

    private fun device() = JSONObject().apply {
        put("manufacturer", Build.MANUFACTURER)
        put("model", Build.MODEL)
        put("device", Build.DEVICE)
        if (Build.VERSION.SDK_INT >= 31) put("soc", "${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}")
        put("android", Build.VERSION.RELEASE)
        put("sdk", Build.VERSION.SDK_INT)
        put("tv", packageManager.hasSystemFeature("android.software.leanback"))
        display?.let { d ->
            put("display", "${d.mode.physicalWidth}x${d.mode.physicalHeight} at ${d.mode.refreshRate} Hz")
            put("hdr", JSONArray(d.hdrCapabilities?.supportedHdrTypes?.toList() ?: emptyList<Int>()))
        }
    }

    /** Every AV1 and HEVC decoder, with what it says it can do. */
    private fun decoders() = JSONArray().apply {
        for (info in MediaCodecList(MediaCodecList.ALL_CODECS).codecInfos) {
            if (info.isEncoder) continue
            for (mime in info.supportedTypes) {
                if (mime != "video/av01" && mime != "video/hevc") continue
                val caps = info.getCapabilitiesForType(mime)
                val v = caps.videoCapabilities ?: continue
                val profiles = caps.profileLevels.map { it.profile }.toSet()
                put(JSONObject().apply {
                    put("name", info.name)
                    put("mime", mime)
                    put("hardware", info.isHardwareAccelerated)
                    put("main10", if (mime == "video/av01") CodecProfileLevel.AV1ProfileMain10 in profiles else CodecProfileLevel.HEVCProfileMain10 in profiles)
                    // (Android lists no HEVC 4:4:4 profile; whether a decoder takes one is only
                    // found by trying, as on Apple's side.)
                    put("profiles", JSONArray(profiles.sorted()))
                    put("max", "${v.supportedWidths.upper}x${v.supportedHeights.upper}")
                    for ((w, h, hz) in listOf(Triple(1920, 1080, 120), Triple(3840, 2160, 60), Triple(3840, 2160, 120))) {
                        put("${w}x${h}@$hz", v.areSizeAndRateSupported(w, h, hz.toDouble()))
                    }
                    put("low_latency", caps.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_LowLatency))
                    put("p010", MediaCodecInfo.CodecCapabilities.COLOR_FormatYUVP010 in caps.colorFormats)
                })
            }
        }
    }

    private fun recording(name: String): JSONObject {
        val out = JSONObject().put("name", name)
        val file = File(cacheDir, "$name.rmrec")
        if (!file.exists()) assets.open("recordings/$name.rmrec").use { i -> file.outputStream().use { i.copyTo(it) } }
        val rec = Recording(file)
        val mime = if (rec.codec == "av1") "video/av01" else "video/hevc"
        val profile = if (rec.codec == "av1") CodecProfileLevel.AV1ProfileMain10 else CodecProfileLevel.HEVCProfileMain10
        // A hardware 10-bit decoder if there is one, else any 10-bit one (the emulator's are software).
        val decoder = Decoders.has(mime, profile, true) ?: Decoders.has(mime, profile, false)
        if (decoder == null) {
            say("$name: no 10-bit $mime decoder")
            return out.put("skipped", "no 10-bit decoder")
        }
        Core.useDecoder(mime, decoder)

        // 1. Timing, through the app's own decoder.
        say("$name: playing ${rec.frames.size} frames at ${rec.refresh} Hz…")
        val timing = JSONObject(Core.benchSurface(file.absolutePath, picture.holder.surface))
        out.put("timing", timing)
        timing.optJSONObject("decode_ms")?.let {
            say("  ${timing.optString("decoder")}: shown ${timing.optInt("shown")}/${rec.frames.size}, dropped ${timing.optInt("dropped")}, decode median ${it.optDouble("median")} ms, p99 ${it.optDouble("p99")} ms")
        } ?: say("  ${timing.optString("error")}")

        // 2. What the screen shows.
        try {
            val psnr = screenPsnr(name)
            out.put("screen_psnr_db", psnr)
            val verdict = when {
                psnr >= 30 -> "right"
                psnr >= 15 -> "close: look at files/screen-$name.png"
                else -> "WRONG: the picture isn't shown as decoded"
            }
            out.put("screen", verdict)
            say("  screen vs FFmpeg: %.1f dB, %s".format(psnr, verdict))
        } catch (e: Throwable) {
            out.put("screen_error", e.toString())
            say("  screen: $e")
        } finally {
            Core.benchRelease()
        }

        // 3. Pixels, bit for bit.
        try {
            val px = pixels(rec, mime, decoder, assets.open("recordings/$name.md5").bufferedReader().readLines(), if (rec.width > 2000) 20 else 60)
            out.put("pixels", px)
            say("  pixels: ${px.optString("summary")}")
        } catch (e: Throwable) {
            out.put("pixels_error", e.toString())
            say("  pixels: $e")
        }
        return out
    }

    /**
     * [src] made [w]×[h] by averaging every pixel that falls in each new one, as FFmpeg's
     * `flags=area` does for the reference. Bilinear filtering from a phone's 2340-pixel-wide copy
     * aliases the test picture's fine text and reads a right picture as 20 to 25 dB.
     */
    private fun shrink(src: Bitmap, w: Int, h: Int): Bitmap {
        val sw = src.width
        val sh = src.height
        val px = IntArray(sw * sh).also { src.getPixels(it, 0, sw, 0, 0, sw, sh) }
        val out = IntArray(w * h)
        for (y in 0 until h) {
            val y0 = y * sh / h
            val y1 = maxOf(y0 + 1, (y + 1) * sh / h)
            for (x in 0 until w) {
                val x0 = x * sw / w
                val x1 = maxOf(x0 + 1, (x + 1) * sw / w)
                var r = 0L; var g = 0L; var b = 0L
                for (yy in y0 until y1) for (xx in x0 until x1) {
                    val c = px[yy * sw + xx]
                    r += (c shr 16) and 255; g += (c shr 8) and 255; b += c and 255
                }
                val n = (y1 - y0) * (x1 - x0)
                out[y * w + x] = (0xFF shl 24) or ((r / n).toInt() shl 16) or ((g / n).toInt() shl 8) or (b / n).toInt()
            }
        }
        return Bitmap.createBitmap(out, w, h, Bitmap.Config.ARGB_8888)
    }

    /** The screen's picture against FFmpeg's last frame, both at 480x270. */
    private fun screenPsnr(name: String): Double {
        val shot = Bitmap.createBitmap(picture.width, picture.height, Bitmap.Config.ARGB_8888)
        val done = CountDownLatch(1)
        var status = -1
        val thread = HandlerThread("copy").apply { start() }
        PixelCopy.request(picture, shot, { status = it; done.countDown() }, Handler(thread.looper))
        done.await(5, TimeUnit.SECONDS)
        thread.quitSafely()
        check(status == PixelCopy.SUCCESS) { "PixelCopy failed ($status)" }
        val ref = assets.open("recordings/$name.png").use { BitmapFactory.decodeStream(it) }
        val small = shrink(shot, ref.width, ref.height)
        // Kept for looking at: files/screen-NAME.png.
        File(filesDir, "screen-$name.png").outputStream().use { small.compress(Bitmap.CompressFormat.PNG, 100, it) }
        var sum = 0.0
        for (y in 0 until ref.height) for (x in 0 until ref.width) {
            val a = small.getPixel(x, y)
            val b = ref.getPixel(x, y)
            for (shift in intArrayOf(16, 8, 0)) {
                val d = ((a shr shift) and 255) - ((b shr shift) and 255)
                sum += d * d
            }
        }
        val mse = sum / (ref.width * ref.height * 3)
        return if (mse == 0.0) 99.0 else 10 * log10(255.0 * 255.0 / mse)
    }

    /**
     * Decode the first [limit] frames into memory and compare each with FFmpeg's MD5 of the
     * same frame as yuv420p10le.
     */
    private fun pixels(rec: Recording, mime: String, decoder: String, md5s: List<String>, limit: Int): JSONObject {
        val codec = MediaCodec.createByCodecName(decoder)
        val caps = codec.codecInfo.getCapabilitiesForType(mime)
        val format = MediaFormat.createVideoFormat(mime, rec.width, rec.height)
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT,
            if (MediaCodecInfo.CodecCapabilities.COLOR_FormatYUVP010 in caps.colorFormats) MediaCodecInfo.CodecCapabilities.COLOR_FormatYUVP010
            else MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible)
        if (mime == "video/hevc") format.setByteBuffer("csd-0", ByteBuffer.wrap(parameterSets(rec.frames[0])))
        codec.configure(format, null, null, 0)
        codec.start()
        val frames = rec.frames.take(limit)
        var fed = 0
        var got = 0
        var same = 0
        var imageFormat = 0
        var eightBit = false
        val info = MediaCodec.BufferInfo()
        val until = System.nanoTime() + 60_000_000_000
        try {
            while (got < frames.size && System.nanoTime() < until) {
                if (fed == frames.size) {
                    // The last few pictures come out only once the decoder knows nothing follows.
                    val i = codec.dequeueInputBuffer(2000)
                    if (i >= 0) { codec.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM); fed++ }
                } else if (fed < frames.size) {
                    val i = codec.dequeueInputBuffer(2000)
                    if (i >= 0) {
                        val f = frames[fed]
                        codec.getInputBuffer(i)!!.apply { clear(); put(f.data) }
                        codec.queueInputBuffer(i, 0, f.data.size, f.captureUs, if (f.key) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
                        fed++
                    }
                }
                val o = codec.dequeueOutputBuffer(info, 2000)
                if (o >= 0 && info.size == 0 && info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) {
                    codec.releaseOutputBuffer(o, false)
                    break
                }
                if (o >= 0) {
                    codec.getOutputImage(o)?.use { image ->
                        imageFormat = image.format
                        val hash = hashImage(image)
                        if (hash == null) eightBit = true
                        else if (hash == md5s.getOrNull(got)) same++
                    }
                    codec.releaseOutputBuffer(o, false)
                    got++
                }
            }
        } finally {
            codec.stop()
            codec.release()
        }
        val summary = when {
            got == 0 -> "nothing decoded"
            eightBit -> "decoded $got, but to 8 bits a sample (format $imageFormat): can't compare"
            same == got -> "all $got frames bit-exact"
            else -> "$same of $got frames bit-exact (format $imageFormat)"
        }
        return JSONObject().put("decoder", decoder).put("frames", got).put("bit_exact", same).put("image_format", imageFormat).put("eight_bit", eightBit).put("summary", summary)
    }

    /** MD5 of the frame as yuv420p10le, or null if its samples are 8-bit. */
    private fun hashImage(image: android.media.Image): String? {
        val w = image.width
        val h = image.height
        val planes = image.planes
        if (planes[0].pixelStride < 2) return null
        // P010 puts the 10 bits at the top of each 16; a decoder that puts them at the bottom
        // is found by its low bits.
        val y0 = planes[0].buffer.duplicate().order(ByteOrder.LITTLE_ENDIAN)
        var low = 0
        for (i in 0 until minOf(w, 512)) low = low or (y0.getShort(i * planes[0].pixelStride).toInt() and 0x3F)
        val shift = if (low == 0) 6 else 0
        val md5 = MessageDigest.getInstance("MD5")
        for ((p, pw, ph) in listOf(Triple(0, w, h), Triple(1, w / 2, h / 2), Triple(2, w / 2, h / 2))) {
            val plane = planes[p]
            val buf = plane.buffer.duplicate().order(ByteOrder.LITTLE_ENDIAN)
            val row = ByteArray(pw * 2)
            for (y in 0 until ph) {
                val base = y * plane.rowStride
                for (x in 0 until pw) {
                    val v = (buf.getShort(base + x * plane.pixelStride).toInt() and 0xFFFF) ushr shift
                    row[2 * x] = v.toByte()
                    row[2 * x + 1] = (v ushr 8).toByte()
                }
                md5.update(row)
            }
        }
        return md5.digest().joinToString("") { "%02x".format(it) }
    }

    /** HEVC's VPS, SPS and PPS, each with a start code, from a keyframe. */
    private fun parameterSets(f: Recording.Frame): ByteArray {
        val d = f.data
        val out = java.io.ByteArrayOutputStream()
        val starts = ArrayList<Int>()
        var i = 0
        while (i + 2 < d.size) {
            if (d[i].toInt() == 0 && d[i + 1].toInt() == 0 && d[i + 2].toInt() == 1) { starts += i + 3; i += 3 } else i++
        }
        for ((k, s) in starts.withIndex()) {
            var end = if (k + 1 < starts.size) starts[k + 1] - 3 else d.size
            while (end > s && d[end - 1].toInt() == 0) end--
            val type = (d[s].toInt() shr 1) and 0x3F
            if (type in 32..34) { out.write(byteArrayOf(0, 0, 0, 1)); out.write(d, s, end - s) }
        }
        return out.toByteArray()
    }

    /** A recorded stream (remoteman-testclient --record): "RMREC1\n", a JSON line, then frames. */
    class Recording(file: File) {
        class Frame(val key: Boolean, val number: Int, val captureUs: Long, val data: ByteArray)

        val codec: String
        val width: Int
        val height: Int
        val refresh: Int
        val frames = ArrayList<Frame>()

        init {
            val bytes = file.readBytes()
            val nl = (7 until bytes.size).first { bytes[it] == '\n'.code.toByte() }
            val desc = JSONObject(String(bytes, 7, nl - 7))
            codec = desc.getString("codec")
            width = desc.getInt("width")
            height = desc.getInt("height")
            refresh = desc.optInt("refresh", 60)
            val b = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
            b.position(nl + 1)
            while (b.remaining() >= 17) {
                val len = b.int
                val key = b.get().toInt() != 0
                val number = b.int
                val capture = b.long
                val data = ByteArray(len).also { b.get(it) }
                frames += Frame(key, number, capture, data)
            }
        }
    }

    companion object {
        const val TAG = "remoteman-bench"
    }
}
