package com.xnixy.remoteman.phone

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedCard
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.xnixy.remoteman.AppModel
import com.xnixy.remoteman.Decoders
import com.xnixy.remoteman.Screen

/**
 * "Add a computer": scan its pairing code, or type its address. The first connection pairs,
 * while the computer is in pairing mode and its owner confirms there.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddScreen(model: AppModel) {
    var address by rememberSaveable { mutableStateOfDefault() }
    var name by rememberSaveable { androidx.compose.runtime.mutableStateOf("") }
    val ok = address.isNotBlank()
    val add = { if (ok) model.add(name, address) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Add a computer") },
                navigationIcon = { IconButton(onClick = { model.pop() }) { Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back") } },
            )
        },
    ) { padding ->
        Box(Modifier.fillMaxSize().padding(padding).imePadding(), contentAlignment = Alignment.TopCenter) {
            Column(
                Modifier.widthIn(max = 600.dp).fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 24.dp, vertical = 8.dp),
                verticalArrangement = Arrangement.spacedBy(16.dp),
            ) {
                OutlinedCard(Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text("Scan its code", style = MaterialTheme.typography.titleMedium)
                        Text(
                            "On the computer, open remoteman's \"Pair a Device\". Its QR code has the computer's address and key.",
                            style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        FilledTonalButton(onClick = { model.push(Screen.Scan) }) { Text("Scan the code") }
                    }
                }
                Text("Or add it by its address", style = MaterialTheme.typography.titleMedium)
                Text(
                    "Start remoteman on the computer with pairing on. The first connection from this ${model.session.deviceKind} pairs the two, and no code is needed after that.",
                    style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = address, onValueChange = { address = it },
                    label = { Text("Address") }, placeholder = { Text("blackbox or 10.0.0.76") },
                    supportingText = { Text("The computer's name or number on your network. Add :port if it isn't 7791.") },
                    singleLine = true, modifier = Modifier.fillMaxWidth(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, autoCorrectEnabled = false, capitalization = KeyboardCapitalization.None, imeAction = ImeAction.Next),
                )
                OutlinedTextField(
                    value = name, onValueChange = { name = it },
                    label = { Text("Name (optional)") }, singleLine = true, modifier = Modifier.fillMaxWidth(),
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Words, imeAction = ImeAction.Go),
                    keyboardActions = KeyboardActions(onGo = { add() }),
                )
                Button(onClick = add, enabled = ok, modifier = Modifier.fillMaxWidth()) { Text("Pair and connect") }
            }
        }
    }
}

/** The emulator's way to this PC's throwaway host; empty on devices. */
fun defaultAddress() = if (Decoders.isEmulator) "10.0.2.2:7799" else ""

private fun mutableStateOfDefault() = androidx.compose.runtime.mutableStateOf(defaultAddress())
