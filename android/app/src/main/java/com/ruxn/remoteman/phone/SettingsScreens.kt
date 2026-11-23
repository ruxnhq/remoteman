package com.ruxn.remoteman.phone

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.ui.graphics.Color
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.appVersion
import com.ruxn.remoteman.Decoders
import com.ruxn.remoteman.Placement
import com.ruxn.remoteman.Resolution
import com.ruxn.remoteman.refreshChoices
import com.ruxn.remoteman.refreshTitle

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SettingsScaffold(model: AppModel, title: String, content: @Composable () -> Unit) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(title) },
                navigationIcon = { IconButton(onClick = { model.pop() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back") } },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding).imePadding(), contentAlignment = Alignment.TopCenter) {
            Column(Modifier.widthIn(max = 720.dp).fillMaxWidth().verticalScroll(rememberScrollState())) { content() }
        }
    }
}

@Composable
private fun Header(text: String) {
    Text(text, style = MaterialTheme.typography.labelLarge, color = MaterialTheme.colorScheme.primary, modifier = Modifier.padding(start = 16.dp, top = 24.dp, bottom = 4.dp))
}

/** A setting with a few choices: its value under its name; tapping opens the choices. */
@Composable
private fun <T> Choice(title: String, value: T, choices: List<T>, label: (T) -> String, detail: (T) -> String? = { null }, onChoose: (T) -> Unit) {
    var open by remember { mutableStateOf(false) }
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(label(value)) },
        modifier = Modifier.clickable { open = true },
    )
    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text(title) },
            text = {
                Column {
                    for (c in choices) {
                        ListItem(
                            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                            leadingContent = { RadioButton(selected = c == value, onClick = null) },
                            headlineContent = { Text(label(c)) },
                            supportingContent = detail(c)?.let { d -> { Text(d) } },
                            modifier = Modifier.selectable(selected = c == value, role = Role.RadioButton) { onChoose(c); open = false },
                        )
                    }
                }
            },
            confirmButton = { TextButton(onClick = { open = false }) { Text("Cancel") } },
        )
    }
}

/** The viewer's settings. */
@Composable
fun SettingsScreen(model: AppModel) {
    val p = model.prefs
    SettingsScaffold(model, "Settings") {
        Header("Display")
        Choice("On the computer, this ${model.session.deviceKind} is", p.placement, Placement.entries, { it.title }, { it.detail }) { p.placement(it) }
        Choice("Resolution", p.resolution, Resolution.entries, { it.title }) { p.resolution(it) }
        Choice("Refresh rate", p.refresh, refreshChoices, ::refreshTitle) { p.refresh(it) }
        Text(
            "Each computer can have its own placement, in its settings.",
            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(horizontal = 16.dp),
        )
        Header("Session")
        ListItem(
            headlineContent = { Text("Show statistics") },
            supportingContent = { Text("Frames, losses and delay over the picture") },
            trailingContent = { Switch(checked = p.showStats, onCheckedChange = null) },
            modifier = Modifier.clickable { p.showStats(!p.showStats) },
        )
        Header("This ${model.session.deviceKind}")
        ListItem(headlineContent = { Text("Decoders") }, supportingContent = { Text(Decoders.describe()) })
        ListItem(headlineContent = { Text("Version") }, supportingContent = { Text(appVersion()) })
    }
}

/** One computer's settings: its name, how this device's display sits on it, forgetting it. */
@Composable
fun ComputerScreen(model: AppModel, id: String) {
    val c = model.store.get(id) ?: run { model.pop(); return }
    var forgetting by remember { mutableStateOf(false) }
    var name by remember(id) { mutableStateOf(c.name) }
    SettingsScaffold(model, c.name) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it; if (it.isNotBlank()) model.store.update(c.copy(name = it.trim())) },
            label = { Text("Name") }, singleLine = true,
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Done),
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        )
        ListItem(headlineContent = { Text("Address") }, supportingContent = { Text(c.address) })
        c.os?.let { ListItem(headlineContent = { Text("System") }, supportingContent = { Text(c.osLine) }) }
        ListItem(headlineContent = { Text("Paired") }, supportingContent = { Text(if (c.paired) "Yes" + (c.usedLine()?.let { " · $it" } ?: "") else "Not yet: the next connection pairs") })
        Header("On ${c.name}, this ${model.session.deviceKind} is")
        val choices: List<Placement?> = listOf(null) + Placement.entries
        for (pl in choices) {
            ListItem(
                leadingContent = { RadioButton(selected = c.placement == pl, onClick = null) },
                headlineContent = { Text(pl?.title ?: "As in Settings (${model.prefs.placement.title.lowercase()})") },
                supportingContent = pl?.let { { Text(it.detail) } },
                modifier = Modifier.selectable(selected = c.placement == pl, role = Role.RadioButton) { model.store.update(c.copy(placement = pl)) },
            )
        }
        HorizontalDivider(Modifier.padding(vertical = 8.dp))
        ListItem(
            headlineContent = { Text("Forget this computer…", color = MaterialTheme.colorScheme.error) },
            modifier = Modifier.clickable { forgetting = true },
        )
    }
    if (forgetting) ForgetDialog(model, c, onDone = { forgetting = false }, after = { model.pop() })
}
