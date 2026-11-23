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
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.ruxn.remoteman.AppModel
import com.ruxn.remoteman.Computer
import com.ruxn.remoteman.Screen

/** The computers this device knows: tap one to connect; add or pair another. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ComputersScreen(model: AppModel) {
    val scroll = TopAppBarDefaults.exitUntilCollapsedScrollBehavior()
    var forgetting by remember { mutableStateOf<Computer?>(null) }
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
                if (computers.isEmpty()) item { Empty(model) }
                items(computers, key = { it.id }) { c ->
                    ComputerRow(c, onConnect = { model.connect(c) }, onSettings = { model.push(Screen.Computer(c.id)) }, onForget = { forgetting = c })
                }
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
private fun ComputerRow(c: Computer, onConnect: () -> Unit, onSettings: () -> Unit, onForget: () -> Unit) {
    var menu by remember { mutableStateOf(false) }
    Card(onClick = onConnect, modifier = Modifier.fillMaxWidth()) {
        ListItem(
            colors = ListItemDefaults.colors(containerColor = androidx.compose.ui.graphics.Color.Transparent),
            leadingContent = { Monogram(c.name) },
            headlineContent = { Text(c.name, style = MaterialTheme.typography.titleMedium) },
            supportingContent = {
                val parts = listOfNotNull(c.osLine.takeIf { it != c.address }, c.address, c.usedLine() ?: if (!c.paired) "Not paired yet" else null)
                Text(parts.joinToString(" · "))
            },
            trailingContent = {
                Box {
                    IconButton(onClick = { menu = true }) { Icon(Icons.Default.MoreVert, contentDescription = "More for ${c.name}") }
                    DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                        DropdownMenuItem(text = { Text("Connect") }, onClick = { menu = false; onConnect() })
                        DropdownMenuItem(text = { Text("Computer settings") }, onClick = { menu = false; onSettings() })
                        DropdownMenuItem(text = { Text("Forget…") }, onClick = { menu = false; onForget() })
                    }
                }
            },
        )
    }
}

@Composable
fun Monogram(name: String) {
    Surface(shape = CircleShape, color = MaterialTheme.colorScheme.primaryContainer, modifier = Modifier.size(44.dp)) {
        Box(contentAlignment = Alignment.Center) {
            Text(name.trim().take(1).uppercase(), style = MaterialTheme.typography.titleMedium, color = MaterialTheme.colorScheme.onPrimaryContainer)
        }
    }
}

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
