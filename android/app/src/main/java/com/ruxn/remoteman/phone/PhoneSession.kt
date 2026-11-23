package com.ruxn.remoteman.phone

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.isImeVisible
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Phase
import com.ruxn.remoteman.Placement

/** Connecting, pairing, the second factor, the picture with its controls, and the end. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun PhoneSession(model: AppModel) {
    val session = model.session
    // The keyboard put away by the system (its own Back) is put away here too.
    val ime = WindowInsets.isImeVisible
    var imeSeen by remember { mutableStateOf(false) }
    LaunchedEffect(ime) {
        if (ime) imeSeen = true else if (imeSeen) { imeSeen = false; model.keyboard = false }
    }
    Box(Modifier.fillMaxSize().background(Color.Black)) {
        com.ruxn.remoteman.Picture(model)
        when (val p = session.phase) {
            is Phase.Connecting -> Backdrop { Connecting(model, p) }
            is Phase.SecondFactor -> Backdrop { SecondFactor(model, p.left) }
            is Phase.Ended -> Backdrop { Ended(model, p) }
            Phase.Streaming -> Controls(model)
            Phase.Idle -> Backdrop { Box(Modifier.size(48.dp)) { CircularProgressIndicator() } }
        }
    }
}

/** The dark ground the getting-in states sit on, over the (not yet shown) picture. */
@Composable
private fun Backdrop(content: @Composable () -> Unit) {
    Box(
        Modifier.fillMaxSize().background(Brush.verticalGradient(listOf(Color(0xFF1B2430), Color(0xFF0B0D12)))).safeDrawingPadding().imePadding(),
        contentAlignment = Alignment.Center,
    ) {
        Surface(
            color = Color.White.copy(alpha = 0.06f), shape = RoundedCornerShape(32.dp),
            modifier = Modifier.padding(20.dp).widthIn(max = 560.dp).fillMaxWidth(),
        ) {
            Column(
                Modifier.verticalScroll(rememberScrollState()).padding(24.dp),
                verticalArrangement = Arrangement.spacedBy(20.dp),
            ) { content() }
        }
    }
}

@Composable
private fun Connecting(model: AppModel, p: Phase.Connecting) {
    val c = model.session.computer
    // Until the computer has said its name, it's "the computer", not its address twice.
    val name = c?.name?.takeIf { it != c.address } ?: "the computer"
    val device = model.session.deviceKind
    val placement = c?.placement ?: model.prefs.placement
    Text("Connecting to $name", style = MaterialTheme.typography.headlineSmall, color = Color.White)
    Text(
        if (c?.paired == false) "Pairing this $device with the computer." else "Reaching your session.",
        style = MaterialTheme.typography.bodyMedium, color = Color.White.copy(alpha = 0.7f),
    )
    Column(verticalArrangement = Arrangement.spacedBy(14.dp)) {
        Step(if (p.stage >= 2) StepState.Done else StepState.Current, "Reaching $name", "At ${c?.address ?: "its address"}.")
        Step(
            when { p.stage >= 2 -> StepState.Done; p.stage == 1 -> StepState.Current; else -> StepState.Pending },
            "Recognising it by its key", if (c?.paired == false) "This first connection pairs the two." else null,
        )
        Step(if (p.stage >= 2) StepState.Current else StepState.Pending, placement.becomes(device), null)
    }
    p.code?.let { code -> PairingCode(code, name) }
    OutlinedButton(onClick = { model.leaveSession() }, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
}

@Composable
private fun PairingCode(code: String, name: String) {
    Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(
            code, color = Color.White, fontFamily = FontFamily.Monospace, fontWeight = FontWeight.SemiBold, fontSize = 40.sp, letterSpacing = 2.sp,
            modifier = Modifier.semantics { contentDescription = "Pairing code $code" },
        )
        Text(
            "${name.replaceFirstChar { it.uppercase() }} is waiting for its owner. Check that it shows the same code, then confirm the pairing there.",
            style = MaterialTheme.typography.bodyMedium, color = Color.White.copy(alpha = 0.7f), textAlign = TextAlign.Center,
        )
    }
}

private enum class StepState { Done, Current, Pending }

@Composable
private fun Step(state: StepState, title: String, sub: String?) {
    Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Box(Modifier.size(24.dp), contentAlignment = Alignment.Center) {
            when (state) {
                StepState.Done -> Icon(Icons.Default.CheckCircle, contentDescription = "Done", tint = Color(0xFF45D166))
                StepState.Current -> CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = Color.White)
                StepState.Pending -> Box(Modifier.size(18.dp).background(Color.White.copy(alpha = 0.15f), RoundedCornerShape(9.dp)))
            }
        }
        Column {
            Text(title, style = MaterialTheme.typography.titleMedium, color = if (state == StepState.Pending) Color.White.copy(alpha = 0.6f) else Color.White)
            sub?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = Color.White.copy(alpha = 0.6f)) }
        }
    }
}

@Composable
private fun SecondFactor(model: AppModel, left: Int) {
    val name = model.session.computer?.name ?: "The computer"
    var code by remember { mutableStateOf("") }
    var remember by remember { mutableStateOf(true) }
    val send = { if (code.isNotBlank()) model.session.secondFactor(code, remember) }
    Text("Code from your authenticator app", style = MaterialTheme.typography.headlineSmall, color = Color.White)
    Text(
        if (left >= 0) "That code wasn't right. $left ${if (left == 1) "try" else "tries"} left." else "$name asks for the six-digit code its owner's authenticator app shows for it.",
        style = MaterialTheme.typography.bodyMedium, color = if (left >= 0) MaterialTheme.colorScheme.error else Color.White.copy(alpha = 0.7f),
    )
    OutlinedTextField(
        value = code, onValueChange = { code = it.filter(Char::isDigit).take(8) }, label = { Text("Code") }, singleLine = true,
        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Go),
        keyboardActions = KeyboardActions(onGo = { send() }), modifier = Modifier.fillMaxWidth(),
    )
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(checked = remember, onCheckedChange = { remember = it })
        Text("Don't ask on this ${model.session.deviceKind} for a while", color = Color.White)
    }
    Button(onClick = send, enabled = code.isNotBlank(), modifier = Modifier.fillMaxWidth()) { Text("Continue") }
    OutlinedButton(onClick = { model.leaveSession() }, modifier = Modifier.fillMaxWidth()) { Text("Cancel") }
}

@Composable
private fun Ended(model: AppModel, p: Phase.Ended) {
    val c = model.session.computer
    Text(p.headline, style = MaterialTheme.typography.headlineSmall, color = Color.White)
    Text(p.reason, style = MaterialTheme.typography.bodyMedium, color = Color.White.copy(alpha = 0.7f))
    if (c != null && p.retry) {
        Button(onClick = { if (p.keyChanged) model.pairAgain(c) else model.connect(c) }, modifier = Modifier.fillMaxWidth()) {
            Text(when { p.keyChanged -> "Forget the key and pair again"; p.mustPairAgain -> "Pair again"; p.leftApp || p.byHost -> "Connect again"; else -> "Try again" })
        }
    }
    OutlinedButton(onClick = { model.leaveSession() }, modifier = Modifier.fillMaxWidth()) { Text("Back to computers") }
}

/** Over the picture: a small handle at the top; tapped (or Back), the session's controls. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Controls(model: AppModel) {
    val session = model.session
    Box(Modifier.fillMaxSize().safeDrawingPadding()) {
        if (model.prefs.showStats) {
            Text(
                listOf(session.stream, session.stats).filter { it.isNotEmpty() }.joinToString("\n"),
                color = Color.White, style = MaterialTheme.typography.labelSmall,
                modifier = Modifier.align(Alignment.TopStart).padding(8.dp).background(Color.Black.copy(alpha = 0.5f), RoundedCornerShape(8.dp)).padding(8.dp),
            )
        }
        AnimatedVisibility(!model.controls, enter = fadeIn(), exit = fadeOut(), modifier = Modifier.align(Alignment.TopCenter)) {
            Surface(
                onClick = { model.controls = true }, shape = RoundedCornerShape(50), color = Color.White.copy(alpha = 0.25f),
                modifier = Modifier.padding(top = 6.dp).semantics { contentDescription = "Session controls" },
            ) { Box(Modifier.width(56.dp).height(8.dp)) }
        }
        AnimatedVisibility(model.controls, enter = fadeIn(), exit = fadeOut(), modifier = Modifier.align(Alignment.TopCenter)) {
            Surface(shape = RoundedCornerShape(28.dp), color = MaterialTheme.colorScheme.surfaceContainerHigh, tonalElevation = 6.dp, modifier = Modifier.padding(12.dp)) {
                Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f, fill = false)) {
                            Text(session.computer?.name ?: "", style = MaterialTheme.typography.titleMedium)
                            if (session.stream.isNotEmpty()) Text(session.stream, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        IconButton(onClick = { model.controls = false }) { Icon(Icons.Default.Close, contentDescription = "Hide the controls") }
                    }
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilledTonalButton(onClick = { model.keyboard = !model.keyboard; model.controls = false }) { Text(if (model.keyboard) "Hide keyboard" else "Keyboard") }
                        if (model.session.soundOffered) FilledTonalButton(onClick = { model.sound(!model.prefs.sound) }) { Text(if (model.prefs.sound) "Sound off" else "Sound on") }
                        FilledTonalButton(onClick = { model.prefs.showStats(!model.prefs.showStats) }) { Text(if (model.prefs.showStats) "Hide statistics" else "Statistics") }
                        Button(
                            onClick = { model.leaveSession() },
                            colors = androidx.compose.material3.ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error, contentColor = MaterialTheme.colorScheme.onError),
                        ) { Text("Disconnect") }
                    }
                    // The microphone, the camera, the clipboard and files go here when they come.
                }
            }
        }
    }
}
