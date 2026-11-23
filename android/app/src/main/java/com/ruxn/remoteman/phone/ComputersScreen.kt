package com.ruxn.remoteman.phone

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LargeTopAppBar
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.OutlinedButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Computer
import com.ruxn.remoteman.Found
import com.ruxn.remoteman.Screen

/** The computers this device knows: tap one to connect; add or pair another. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ComputersScreen(model: AppModel) {
    val scroll = TopAppBarDefaults.exitUntilCollapsedScrollBehavior()
    var forgetting by remember { mutableStateOf<Computer?>(null) }
    // Look for computers on this network while this screen is in sight.
    DisposableEffect(Unit) {
        model.lookNearby(true)
        onDispose { model.lookNearby(false) }
    }
    Scaffold(
        modifier = Modifier.nestedScroll(scroll.nestedScrollConnection),
        topBar = {
            LargeTopAppBar(
                title = { Text("Computers") },
                actions = {
                    IconButton(onClick = { model.push(Screen.Settings) }) { Icon(Icons.Default.Settings, contentDescription = "Settings") }
                },
                scrollBehavior = scroll,
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = { model.push(Screen.Add) },
                icon = { Icon(Icons.Default.Add, contentDescription = null) },
                text = { Text("Add computer") },
            )
        },
    ) { padding ->
        val computers = model.store.computers.sortedByDescending { it.lastUsed ?: it.added }
        Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.TopCenter) {
            LazyColumn(
                modifier = Modifier.widthIn(max = 720.dp).fillMaxSize(),
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = 8.dp, bottom = 96.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                model.note?.let { note ->
                    item {
                        Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer)) {
                            Text(note, Modifier.padding(16.dp), color = MaterialTheme.colorScheme.onErrorContainer)
                        }
                    }
                }
                model.info?.let { info ->
                    item {
                        Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer)) {
                            ListItem(
                                colors = ListItemDefaults.colors(containerColor = Color.Transparent),
                                headlineContent = { Text(info) },
                                trailingContent = { TextButton(onClick = { model.cancelWake() }) { Text("Cancel") } },
                            )
                        }
                    }
                }
                if (computers.isEmpty()) item { Empty(model) }
                items(computers, key = { it.id }) { c ->
                    ComputerRow(
                        c, online = model.seen(c) != null, canWake = model.canWake(c),
                        onConnect = { model.connect(c) }, onSettings = { model.push(Screen.Computer(c.id)) },
                        onWake = { model.wake(c) }, onForget = { forgetting = c },
                    )
                }
                // Nearby: computers advertising on this network that this device doesn't have.
                val nearby = model.nearbyNew
                item(key = "nearby") {
                    Column(Modifier.padding(top = 16.dp, start = 4.dp, end = 4.dp)) {
                        Text("Nearby", style = MaterialTheme.typography.titleSmall, color = MaterialTheme.colorScheme.primary)
                        Text(
                            when {
                                model.nearbyNeedsPermission -> "remoteman can find computers on this network once it may reach them."
                                nearby.isNotEmpty() -> "Running remoteman on this network, not paired with this ${model.session.deviceKind}."
                                model.nearby.looking -> "Looking for computers running remoteman on this network…"
                                else -> "Not looking: this ${model.session.deviceKind} may not be on a network."
                            },
                            style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        if (model.nearbyNeedsPermission) {
                            OutlinedButton(onClick = { model.allowNearby() }, modifier = Modifier.padding(top = 8.dp)) { Text("Find computers nearby") }
                        }
                    }
                }
                items(nearby, key = { "nearby:" + it.name }) { f -> NearbyRow(f, onPair = { model.pairNearby(f) }) }
            }
        }
    }
    forgetting?.let { c -> ForgetDialog(model, c, onDone = { forgetting = null }) }
}

@Composable
private fun Empty(model: AppModel) {
    Column(Modifier.fillMaxWidth().padding(top = 48.dp, start = 16.dp, end = 16.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Text("No computers yet", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(8.dp))
        Text(
            "Start remoteman on your computer with pairing on, then add it here by its code or its address.",
            style = MaterialTheme.typography.bodyMedium, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))
        Button(onClick = { model.push(Screen.Add) }) { Text("Add a computer") }
    }
}

@Composable
private fun ComputerRow(c: Computer, online: Boolean, canWake: Boolean, onConnect: () -> Unit, onSettings: () -> Unit, onWake: () -> Unit, onForget: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    Card(onClick = onConnect, modifier = Modifier.fillMaxWidth()) {
        ListItem(
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            leadingContent = { Monogram(c.name, online) },
            headlineContent = { Text(c.name, style = MaterialTheme.typography.titleMedium) },
            supportingContent = {
                val parts = listOfNotNull(if (online) "On this network" else null, c.osLine.takeIf { it != c.address }, c.address, c.usedLine() ?: if (!c.paired) "Not paired yet" else null)
                Text(parts.joinToString(" · "))
            },
            trailingContent = {
                Box {
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, contentDescription = "More for ${c.name}") }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Connect") }, onClick = { menu = false; onConnect() })
                        if (canWake) DropdownMenuItem(text = { Text("Wake and connect") }, onClick = { menu = false; onWake() })
                        DropdownMenuItem(text = { Text("Computer settings") }, onClick = { menu = false; onSettings() })
                        DropdownMenuItem(text = { Text("Forget…") }, onClick = { menu = false; onForget() })
                    }
                }
            },
        )
    }
}

/** A computer seen on this network that this device isn't paired with: Pair goes through the usual pairing. */
@Composable
private fun NearbyRow(f: Found, onPair: () -> Unit) {
    Card(
        onClick = onPair, enabled = f.address != null, modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
    ) {
        ListItem(
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            leadingContent = { Monogram(f.name, online = true) },
            headlineContent = { Text(f.name, style = MaterialTheme.typography.titleMedium) },
            supportingContent = { Text(listOfNotNull(f.osLine.ifEmpty { null }, f.address ?: "Finding its address…").joinToString(" · ")) },
            trailingContent = { OutlinedButton(onClick = onPair, enabled = f.address != null) { Text("Pair") } },
        )
    }
}

/** The computer's initial; [online]: a dot for "seen on this network now". */
@Composable
fun Monogram(name: String, online: Boolean = false) {
    Box(Modifier.size(44.dp)) {
        Surface(shape = CircleShape, color = MaterialTheme.colorScheme.primaryContainer, modifier = Modifier.size(44.dp)) {
            Box(contentAlignment = Alignment.Center) {
                Text(name.trim().take(1).uppercase(), style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
            }
        }
        if (online) {
            Box(
                Modifier.align(Alignment.BottomEnd).size(14.dp)
                    .border(2.dp, MaterialTheme.colorScheme.surface, CircleShape)
                    .background(OnlineGreen, CircleShape),
            )
        }
    }
}

/** The dot of a computer seen on this network. */
val OnlineGreen = Color(0xFF34A853)

@Composable
fun ForgetDialog(model: AppModel, c: Computer, onDone: () -> Unit, after: () -> Unit = {}) {
    AlertDialog(
        onDismissRequest = onDone,
        title = { Text("Forget ${c.name}?") },
        text = { Text("Its key is removed from this ${model.session.deviceKind}, so connecting again needs pairing again. Revoke this ${model.session.deviceKind} on the computer too.") },
        confirmButton = {
            TextButton(onClick = { model.store.forget(c); onDone(); after() }) { Text("Forget", color = MaterialTheme.colorScheme.error) }
        },
        dismissButton = { TextButton(onClick = onDone) { Text("Cancel") } },
    )
}
