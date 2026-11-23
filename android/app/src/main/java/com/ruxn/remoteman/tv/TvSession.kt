package com.ruxn.remoteman.tv

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedTextField
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.tv.material3.Button
import androidx.tv.material3.Icon
import androidx.tv.material3.MaterialTheme
import androidx.tv.material3.OutlinedButton
import androidx.tv.material3.Surface
import androidx.tv.material3.SurfaceDefaults
import androidx.tv.material3.Text
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Phase
import com.ruxn.remoteman.Picture

/**
 * The session on a TV: the picture full screen; the remote's Back opens a bar with Disconnect
 * and the statistics (Back again closes it), instead of leaving.
 */
@Composable
fun TvSession(model: AppModel) {
    val session = model.session
    Box(Modifier.fillMaxSize().background(Color.Black)) {
        Picture(model)
        when (val p = session.phase) {
            is Phase.Connecting -> Panel { Connecting(model, p) }
            is Phase.SecondFactor -> Panel { SecondFactor(model, p.left) }
            is Phase.Ended -> Panel { Ended(model, p) }
            Phase.Streaming -> Overlay(model)
            Phase.Idle -> {}
        }
    }
}

@Composable
private fun Panel(content: @Composable () -> Unit) {
    Box(
        Modifier.fillMaxSize().background(Brush.verticalGradient(listOf(Color(0xFF1B2430), Color(0xFF0B0D12)))).padding(horizontal = 48.dp, vertical = 27.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(Modifier.width(720.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) { content() }
    }
}

@Composable
private fun Connecting(model: AppModel, p: Phase.Connecting) {
    val c = model.session.computer
    // Until the computer has said its name, it's "the computer", not its address twice.
    val name = c?.name?.takeIf { it != c.address } ?: "the computer"
    val placement = c?.placement ?: model.prefs.placement
    val focus = rememberInitialFocus()
    Text("Connecting to $name", style = MaterialTheme.typography.headlineMedium, color = Color.White)
    Step(p.stage >= 2, p.stage < 2, "Reaching $name at ${c?.address ?: "its address"}")
    Step(p.stage >= 2, p.stage == 1, if (c?.paired == false) "Recognising it by its key: this first connection pairs the two" else "Recognising it by its key")
    Step(false, p.stage >= 2, placement.becomes("TV"))
    p.code?.let { code ->
        Text(code, color = Color.White, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.SemiBold, fontSize = 48.sp, letterSpacing = 3.sp)
        Text("${name.replaceFirstChar { it.uppercase() }} is waiting for its owner. Check that it shows the same code, then confirm the pairing there.", color = Color.White.copy(alpha = 0.75f), style = MaterialTheme.typography.bodyLarge)
    }
    OutlinedButton(onClick = { model.leaveSession() }, modifier = Modifier.focusRequester(focus)) { Text("Cancel") }
}

@Composable
private fun Step(done: Boolean, current: Boolean, title: String) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Box(Modifier.size(28.dp), contentAlignment = Alignment.Center) {
            when {
                done -> Icon(Icons.Default.CheckCircle, contentDescription = "Done", tint = Color(0xFF45D166))
                current -> CircularProgressIndicator(Modifier.size(22.dp), strokeWidth = 2.dp, color = Color.White)
                else -> Box(Modifier.size(20.dp).background(Color.White.copy(alpha = 0.15f), RoundedCornerShape(10.dp)))
            }
        }
        Text(title, style = MaterialTheme.typography.titleMedium, color = if (done || current) Color.White else Color.White.copy(alpha = 0.6f))
    }
}

@Composable
private fun SecondFactor(model: AppModel, left: Int) {
    var code by remember { mutableStateOf("") }
    val send = { if (code.isNotBlank()) model.session.secondFactor(code, true) }
    val focus = rememberInitialFocus()
    Text("Code from your authenticator app", style = MaterialTheme.typography.headlineMedium, color = Color.White)
    Text(
        if (left >= 0) "That code wasn't right. $left ${if (left == 1) "try" else "tries"} left." else "${model.session.computer?.name ?: "The computer"} asks for the code its owner's authenticator app shows for it.",
        color = if (left >= 0) MaterialTheme.colorScheme.error else Color.White.copy(alpha = 0.75f),
    )
    TvTextField(
        value = code, onValueChange = { code = it.filter(Char::isDigit).take(8) }, label = "Code",
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Go), onDone = { send() },
        modifier = Modifier.fillMaxWidth().focusRequester(focus),
    )
    Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        Button(onClick = send, enabled = code.isNotBlank()) { Text("Continue") }
        OutlinedButton(onClick = { model.leaveSession() }) { Text("Cancel") }
    }
}

@Composable
private fun Ended(model: AppModel, p: Phase.Ended) {
    val c = model.session.computer
    val focus = rememberInitialFocus(p)
    Text(p.headline, style = MaterialTheme.typography.headlineMedium, color = Color.White)
    Text(p.reason, style = MaterialTheme.typography.bodyLarge, color = Color.White.copy(alpha = 0.75f))
    Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
        if (c != null && p.retry) {
            Button(onClick = { if (p.keyChanged) model.pairAgain(c) else model.connect(c) }, modifier = Modifier.focusRequester(focus)) {
                Text(when { p.keyChanged -> "Forget the key and pair again"; p.mustPairAgain -> "Pair again"; p.leftApp || p.byHost -> "Connect again"; else -> "Try again" })
            }
        }
        // It didn't answer: it may be asleep, and its cards are known.
        if (c != null && p.noAnswer && model.canWake(c)) {
            OutlinedButton(onClick = { model.wake(c) }) { Text("Wake it and connect") }
        }
        OutlinedButton(onClick = { model.leaveSession() }, modifier = if (c == null || !p.retry) Modifier.focusRequester(focus) else Modifier) { Text("Back to computers") }
    }
}

/** The statistics line, and the bar Back opens. */
@Composable
private fun Overlay(model: AppModel) {
    val session = model.session
    Box(Modifier.fillMaxSize().padding(horizontal = 48.dp, vertical = 27.dp)) {
        if (model.prefs.showStats) {
            Text(
                listOf(session.stream, session.stats).filter { it.isNotEmpty() }.joinToString("\n"),
                color = Color.White, style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.align(Alignment.TopStart).background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(8.dp)).padding(10.dp),
            )
        }
        if (model.controls) {
            val focus = rememberInitialFocus()
            Surface(
                modifier = Modifier.align(Alignment.BottomCenter),
                shape = RoundedCornerShape(24.dp),
                colors = SurfaceDefaults.colors(containerColor = MaterialTheme.colorScheme.surface.copy(alpha = 0.92f)),
            ) {
                Column(Modifier.padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
                    Text(listOfNotNull(session.computer?.name, session.stream.ifEmpty { null }).joinToString(" · "), style = MaterialTheme.typography.titleMedium)
                    Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                        Button(onClick = { model.controls = false }, modifier = Modifier.focusRequester(focus)) { Text("Back to the picture") }
                        if (model.session.soundOffered) OutlinedButton(onClick = { model.sound(!model.prefs.sound) }) { Text(if (model.prefs.sound) "Sound off" else "Sound on") }
                        OutlinedButton(onClick = { model.prefs.showStats(!model.prefs.showStats) }) { Text(if (model.prefs.showStats) "Hide statistics" else "Show statistics") }
                        OutlinedButton(onClick = { model.leaveSession() }) { Text("Disconnect") }
                    }
                    // The microphone, the camera, the clipboard and files go here when they come.
                }
            }
        }
    }
}
