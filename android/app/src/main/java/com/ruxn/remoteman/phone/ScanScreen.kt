package com.ruxn.remoteman.phone

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.view.LifecycleCameraController
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.LuminanceSource
import com.google.zxing.MultiFormatReader
import com.google.zxing.NotFoundException
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.PairLink

/** The camera, reading the computer's pairing code (its QR code in "Pair a Device"). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ScanScreen(model: AppModel) {
    val ctx = LocalContext.current
    var allowed by remember { mutableStateOf(ContextCompat.checkSelfPermission(ctx, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) }
    var refused by remember { mutableStateOf(false) }
    var note by remember { mutableStateOf<String?>(null) }
    val ask = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok -> allowed = ok; refused = !ok }
    // The camera is asked for here, when the person wants to scan.
    LaunchedEffect(Unit) { if (!allowed) ask.launch(Manifest.permission.CAMERA) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Scan the computer's code") },
                navigationIcon = { IconButton(onClick = { model.pop() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back") } },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
            when {
                allowed -> {
                    Camera { text ->
                        val link = PairLink.parse(text)
                        if (link != null) model.pairLink(link) else note = "That isn't a remoteman pairing code. On the computer, open remoteman's \"Pair a Device\"."
                    }
                    Box(Modifier.size(240.dp).border(3.dp, Color.White.copy(alpha = 0.85f), RoundedCornerShape(24.dp)))
                    Surface(
                        color = Color.Black.copy(alpha = 0.6f), shape = RoundedCornerShape(16.dp),
                        modifier = Modifier.align(Alignment.BottomCenter).padding(24.dp),
                    ) {
                        Text(note ?: "Point the camera at the code on the computer's screen.", color = Color.White, textAlign = TextAlign.Center, modifier = Modifier.padding(16.dp))
                    }
                }
                else -> Column(Modifier.padding(32.dp), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text(
                        if (refused) "remoteman isn't allowed to use the camera. Allow it in Settings, Apps, remoteman, Permissions, or add the computer by its address."
                        else "remoteman uses the camera only to read the code.",
                        style = MaterialTheme.typography.bodyLarge, textAlign = TextAlign.Center,
                    )
                    Button(onClick = { ask.launch(Manifest.permission.CAMERA) }) { Text("Allow the camera") }
                }
            }
        }
    }
}

/** The camera's picture, with ZXing reading QR codes in it; [onCode] gets each new one's text. */
@Composable
private fun Camera(onCode: (String) -> Unit) {
    val ctx = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val controller = remember { LifecycleCameraController(ctx) }
    var last by remember { mutableStateOf<String?>(null) }
    DisposableEffect(owner) {
        val main = ContextCompat.getMainExecutor(ctx)
        val work = java.util.concurrent.Executors.newSingleThreadExecutor()
        val reader = QrReader()
        // Only the newest picture is looked at; the camera drops the ones that come meanwhile.
        controller.imageAnalysisBackpressureStrategy = ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST
        controller.setImageAnalysisAnalyzer(work) { image ->
            val text = image.use { reader.read(it) }
            if (text != null) main.execute { if (text != last) { last = text; onCode(text) } }
        }
        controller.bindToLifecycle(owner)
        onDispose {
            controller.clearImageAnalysisAnalyzer()
            controller.unbind()
            work.shutdown()
        }
    }
    AndroidView(
        modifier = Modifier.fillMaxSize().background(Color.Black),
        factory = { c -> PreviewView(c).apply { this.controller = controller } },
    )
}

/**
 * QR codes from the camera's pictures with ZXing: the picture's brightness plane is all a QR
 * reader needs, and a QR code reads the same at any rotation. A code shown light on dark (a
 * dark theme) is tried inverted too.
 */
internal class QrReader {
    private val reader = MultiFormatReader().apply {
        setHints(mapOf(DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE), DecodeHintType.TRY_HARDER to true))
    }
    private var buffer = ByteArray(0)

    fun read(image: ImageProxy): String? {
        val plane = image.planes[0]
        val rowStride = plane.rowStride
        val height = image.height
        val size = rowStride * height
        if (buffer.size != size) buffer = ByteArray(size)
        val bytes = plane.buffer.duplicate()
        bytes.rewind()
        // The last row can be shorter than the stride: what's missing stays as it was.
        bytes.get(buffer, 0, minOf(size, bytes.remaining()))
        return read(PlanarYUVLuminanceSource(buffer, rowStride, height, 0, 0, image.width, height, false))
    }

    fun read(source: LuminanceSource): String? {
        for (s in listOf(source, source.invert())) {
            try {
                return reader.decodeWithState(BinaryBitmap(HybridBinarizer(s))).text
            } catch (_: NotFoundException) {
            } catch (_: ReaderException) {
            } finally {
                reader.reset()
            }
        }
        return null
    }
}
