package com.ruxn.remoteman.tv

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.OutlinedTextField
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.foundation.background
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.activity.compose.BackHandler
import androidx.tv.material3.Button
import androidx.tv.material3.Card
import androidx.tv.material3.Icon
import androidx.tv.material3.ListItem
import androidx.tv.material3.MaterialTheme
import androidx.tv.material3.OutlinedButton
import androidx.tv.material3.RadioButton
import androidx.tv.material3.Switch
import androidx.tv.material3.Text
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Computer
import com.ruxn.remoteman.Decoders
import com.ruxn.remoteman.Found
import com.ruxn.remoteman.phone.OnlineGreen
import com.ruxn.remoteman.Placement
import com.ruxn.remoteman.Resolution
import com.ruxn.remoteman.Screen
import com.ruxn.remoteman.appVersion
import com.ruxn.remoteman.phone.defaultAddress
import com.ruxn.remoteman.refreshChoices
import com.ruxn.remoteman.refreshTitle

@Composable
private fun Title(text: String, sub: String? = null) {
    Column(Modifier.padding(bottom = 24.dp)) {
        Text(text, style = MaterialTheme.typography.displaySmall)
        sub?.let { Text(it, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(top = 8.dp)) }
    }
}

/** The computers, as a row of cards: OK connects, holding OK opens the computer's settings. */
@Composable
fun TvComputers(model: AppModel) {
    val computers = model.store.computers.sortedByDescending { it.lastUsed ?: it.added }
    val first = rememberInitialFocus()
    // Look for computers on this network while this screen is in sight.
    DisposableEffect(Unit) {
        model.lookNearby(true)
        onDispose { model.lookNearby(false) }
    }
    val nearby = model.nearbyNew
    // Scrolls when the Nearby row is pushed below the screen; focus brings it into view.
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState())) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.Top) {
            Box(Modifier.weight(1f)) {
                Title("remoteman", if (computers.isEmpty()) "Start remoteman on your computer with pairing on, then add it here." else "Choose a computer. Hold OK for its settings.")
            }
            OutlinedButton(onClick = { model.push(Screen.Settings) }) {
                Icon(Icons.Default.Settings, contentDescription = null, modifier = Modifier.size(20.dp))
                Spacer(Modifier.width(8.dp))
                Text("Settings")
            }
        }
        model.note?.let { Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(bottom = 16.dp)) }
        model.info?.let { info ->
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp), modifier = Modifier.padding(bottom = 8.dp)) {
                Text(info, style = MaterialTheme.typography.bodyLarge)
                OutlinedButton(onClick = { model.cancelWake() }) { Text("Cancel") }
            }
        }
        LazyRow(horizontalArrangement = Arrangement.spacedBy(24.dp), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 16.dp)) {
            items(computers, key = { it.id }) { c ->
                ComputerCard(
                    c, model.seen(c) != null, Modifier.then(if (c == computers.first()) Modifier.focusRequester(first) else Modifier),
                    onClick = { model.connect(c) }, onLongClick = { model.push(Screen.Computer(c.id)) },
                )
            }
            item {
                Card(
                    onClick = { model.push(Screen.Add) },
                    modifier = Modifier.size(280.dp, 158.dp).then(if (computers.isEmpty()) Modifier.focusRequester(first) else Modifier),
                ) {
                    Column(Modifier.fillMaxSize(), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(Icons.Default.Add, contentDescription = null, modifier = Modifier.size(40.dp))
                        Text("Add a computer", style = MaterialTheme.typography.titleMedium)
                    }
                }
            }
        }
        // Nearby: computers advertising on this network that this TV doesn't have.
        Text("Nearby", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(top = 16.dp))
        Text(
            when {
                model.nearbyNeedsPermission -> "remoteman can find computers on this network once it may reach them."
                nearby.isNotEmpty() -> "Running remoteman on this network, not paired with this TV. OK pairs."
                model.nearby.looking -> "Looking for computers running remoteman on this network…"
                else -> "Not looking: this TV may not be on a network."
            },
            style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (model.nearbyNeedsPermission) {
            OutlinedButton(onClick = { model.allowNearby() }, modifier = Modifier.padding(top = 12.dp, start = 16.dp)) { Text("Find computers nearby") }
        }
        LazyRow(horizontalArrangement = Arrangement.spacedBy(24.dp), contentPadding = PaddingValues(horizontal = 16.dp, vertical = 16.dp)) {
            items(nearby, key = { it.name }) { f -> NearbyCard(f, onClick = { model.pairNearby(f) }) }
        }
    }
}

/** The dot of a computer seen on this network now. */
@Composable
private fun OnlineDot() {
    Box(Modifier.size(12.dp).background(OnlineGreen, CircleShape))
}

@Composable
private fun NearbyCard(f: Found, onClick: () -> Unit) {
    Card(onClick = { if (f.address != null) onClick() }, modifier = Modifier.size(280.dp, 158.dp)) {
        Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.SpaceBetween) {
            Text(f.name, style = MaterialTheme.typography.headlineSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Column {
                if (f.os.isNotEmpty()) Text(f.osLine, style = MaterialTheme.typography.bodyMedium)
                Text(if (f.address != null) "Pair with this TV" else "Finding its address…", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
private fun ComputerCard(c: Computer, online: Boolean, modifier: Modifier, onClick: () -> Unit, onLongClick: () -> Unit) {
    Card(onClick = onClick, onLongClick = onLongClick, modifier = modifier.size(280.dp, 158.dp)) {
        Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.SpaceBetween) {
            Text(c.name, style = MaterialTheme.typography.headlineSmall, maxLines = 2, overflow = TextOverflow.Ellipsis)
            Column {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (online) OnlineDot()
                    Text(if (online) "${c.osLine} · on this network" else c.osLine, style = MaterialTheme.typography.bodyMedium)
                }
                Text(c.usedLine() ?: if (c.paired) c.address else "Not paired yet", style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

/**
 * Adding a computer by its address (a TV has no camera for the code). With an address there
 * already, focus starts on the button, so OK connects instead of opening the keyboard.
 */
@Composable
fun TvAdd(model: AppModel) {
    var address by rememberSaveable { mutableStateOf(defaultAddress()) }
    var name by rememberSaveable { mutableStateOf("") }
    val add = { if (address.isNotBlank()) model.add(name, address) }
    val focus = rememberInitialFocus()
    Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(48.dp)) {
        Column(Modifier.weight(1f)) {
            Title("Add a computer")
            Text(
                "Start remoteman on the computer with pairing on. The first connection from this TV pairs the two: " +
                    "the computer shows a code, the TV shows the same, and the computer's owner confirms there.\n\n" +
                    "The address is the computer's name or number on your network, for example my-pc or 192.168.1.20. Add :port if it isn't 7791.",
                style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            TvTextField(
                value = address, onValueChange = { address = it }, label = "Address",
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false, imeAction = ImeAction.Go),
                onDone = { add() },
                modifier = Modifier.fillMaxWidth().then(if (address.isBlank()) Modifier.focusRequester(focus) else Modifier),
            )
            TvTextField(
                value = name, onValueChange = { name = it }, label = "Name (optional)",
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done), onDone = {},
                modifier = Modifier.fillMaxWidth(),
            )
            Button(onClick = add, enabled = address.isNotBlank(), modifier = if (address.isNotBlank()) Modifier.focusRequester(focus) else Modifier) {
                Text("Pair and connect")
            }
        }
    }
}

/** A list of choices (one setting's), replacing the settings list until one is picked or Back. */
@Composable
private fun <T> ChoiceList(title: String, value: T, choices: List<T>, label: (T) -> String, detail: (T) -> String? = { null }, onChoose: (T) -> Unit, onClose: () -> Unit) {
    BackHandler(onBack = onClose)
    val focus = rememberInitialFocus(title)
    Column(Modifier.fillMaxSize()) {
        Title(title)
        LazyColumn(Modifier.width(640.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(choices) { c ->
                ListItem(
                    selected = c == value,
                    onClick = { onChoose(c); onClose() },
                    leadingContent = { RadioButton(selected = c == value, onClick = null) },
                    headlineContent = { Text(label(c)) },
                    supportingContent = detail(c)?.let { d -> { Text(d) } },
                    modifier = if (c == value) Modifier.focusRequester(focus) else Modifier,
                )
            }
        }
    }
}

@Composable
fun TvSettings(model: AppModel) {
    val p = model.prefs
    var choosing by remember { mutableStateOf<String?>(null) }
    when (choosing) {
        "placement" -> return ChoiceList("On the computer, this TV is", p.placement, Placement.entries, { it.title }, { it.detail }, { p.placement(it) }) { choosing = null }
        "resolution" -> return ChoiceList("Resolution", p.resolution, Resolution.entries, { it.title }, onChoose = { p.resolution(it) }) { choosing = null }
        "refresh" -> return ChoiceList("Refresh rate", p.refresh, refreshChoices, ::refreshTitle, onChoose = { p.refresh(it) }) { choosing = null }
    }
    val focus = rememberInitialFocus()
    Column(Modifier.fillMaxSize()) {
        Title("Settings")
        LazyColumn(Modifier.width(640.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            item {
                ListItem(selected = false, onClick = { choosing = "placement" }, headlineContent = { Text("On the computer, this TV is") },
                    supportingContent = { Text(p.placement.title) }, modifier = Modifier.focusRequester(focus))
            }
            item { ListItem(selected = false, onClick = { choosing = "resolution" }, headlineContent = { Text("Resolution") }, supportingContent = { Text(p.resolution.title) }) }
            item { ListItem(selected = false, onClick = { choosing = "refresh" }, headlineContent = { Text("Refresh rate") }, supportingContent = { Text(refreshTitle(p.refresh)) }) }
            item {
                ListItem(selected = false, onClick = { p.showStats(!p.showStats) }, headlineContent = { Text("Show statistics") },
                    supportingContent = { Text("Frames, losses and delay over the picture") },
                    trailingContent = { Switch(checked = p.showStats, onCheckedChange = null) })
            }
            item {
                ListItem(selected = false, onClick = { model.sound(!p.sound) }, headlineContent = { Text("Play the computer's sound") },
                    supportingContent = { Text("On this TV, as soon as a session starts") },
                    trailingContent = { Switch(checked = p.sound, onCheckedChange = null) })
            }
            item { ListItem(selected = false, onClick = {}, headlineContent = { Text("Decoders") }, supportingContent = { Text(Decoders.describe()) }) }
            item { ListItem(selected = false, onClick = {}, headlineContent = { Text("Version") }, supportingContent = { Text(appVersion()) }) }
            item {
                val uri = androidx.compose.ui.platform.LocalUriHandler.current
                ListItem(selected = false, onClick = { com.ruxn.remoteman.phone.openPolicy(uri) }, headlineContent = { Text("Privacy policy") },
                    supportingContent = { Text(com.ruxn.remoteman.phone.PRIVACY_URL.removePrefix("https://")) })
            }
        }
    }
}

/** One computer: connect, where this TV's display goes on it, forget it. */
@Composable
fun TvComputer(model: AppModel, id: String) {
    val c = model.store.get(id) ?: run { model.pop(); return }
    var confirm by remember { mutableStateOf(false) }
    // Still looking here, so Wake is offered only for a computer not in sight.
    DisposableEffect(Unit) {
        model.lookNearby(true)
        onDispose { model.lookNearby(false) }
    }
    val focus = rememberInitialFocus()
    Row(Modifier.fillMaxSize(), horizontalArrangement = Arrangement.spacedBy(48.dp)) {
        Column(Modifier.weight(1f).fillMaxHeight(), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Title(c.name, listOfNotNull(c.address, c.os?.let { c.osLine }, c.usedLine() ?: if (!c.paired) "Not paired yet" else null).joinToString(" · "))
            Button(onClick = { model.connect(c) }, modifier = Modifier.focusRequester(focus)) { Text("Connect") }
            if (model.canWake(c)) OutlinedButton(onClick = { model.pop(); model.wake(c) }) { Text("Wake and connect") }
            if (!confirm) {
                OutlinedButton(onClick = { confirm = true }) { Text("Forget this computer") }
            } else {
                Text("Its key is removed from this TV, so connecting again needs pairing again. Revoke this TV on the computer too.", color = MaterialTheme.colorScheme.onSurfaceVariant)
                Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                    val f = rememberInitialFocus("confirm")
                    OutlinedButton(onClick = { confirm = false }, modifier = Modifier.focusRequester(f)) { Text("Keep it") }
                    Button(onClick = { model.store.forget(c); model.pop() }) { Text("Forget") }
                }
            }
        }
        Column(Modifier.weight(1f)) {
            Text("On ${c.name}, this TV is", style = MaterialTheme.typography.titleLarge, modifier = Modifier.padding(bottom = 16.dp))
            val choices: List<Placement?> = listOf(null) + Placement.entries
            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(choices) { pl ->
                    ListItem(
                        selected = c.placement == pl,
                        onClick = { model.store.update(c.copy(placement = pl)) },
                        leadingContent = { RadioButton(selected = c.placement == pl, onClick = null) },
                        headlineContent = { Text(pl?.title ?: "As in Settings") },
                        supportingContent = { Text(pl?.detail ?: model.prefs.placement.title) },
                    )
                }
            }
        }
    }
}
