package org.anadolupanteri.uzakel.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import org.anadolupanteri.uzakel.discovery.SavedHost
import org.anadolupanteri.uzakel.discovery.SavedHostsStore
import org.anadolupanteri.uzakel.network.DiscoveredHost
import org.anadolupanteri.uzakel.network.NetworkClient
import java.net.InetAddress

/**
 * Device list + discovery + pairing (ARCHITECTURE.md §4's `discovery/`
 * and §2.3's PIN flow). Tapping a discovered host that isn't already
 * paired prompts for the PIN the daemon showed as a desktop notification;
 * a successful pair saves the host and hands control to [onConnected].
 */
@Composable
fun DeviceListScreen(
    client: NetworkClient,
    savedHosts: SavedHostsStore,
    onConnected: (name: String, address: InetAddress) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var scanning by remember { mutableStateOf(false) }
    var discovered by remember { mutableStateOf<List<DiscoveredHost>>(emptyList()) }
    var savedList by remember { mutableStateOf(savedHosts.list()) }
    var manualAddress by remember { mutableStateOf("") }
    var pairingTarget by remember { mutableStateOf<Pair<String, InetAddress>?>(null) }
    var pairingError by remember { mutableStateOf<String?>(null) }

    fun scan() {
        scanning = true
        scope.launch {
            discovered = try {
                client.discoverHosts()
            } catch (_: Exception) {
                emptyList()
            }
            scanning = false
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
        Text("Uzakel — Cihaz Bul", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(12.dp))

        Row(verticalAlignment = Alignment.CenterVertically) {
            Button(onClick = ::scan, enabled = !scanning) { Text("Tara") }
            Spacer(Modifier.width(12.dp))
            if (scanning) CircularProgressIndicator(modifier = Modifier.size(20.dp))
        }

        Spacer(Modifier.height(16.dp))
        Text("Bulunan cihazlar", style = MaterialTheme.typography.titleMedium)
        LazyColumn(modifier = Modifier.weight(1f, fill = false)) {
            val savedAddresses = savedList.map { it.lastKnownAddress }.toSet()
            items(discovered.filterNot { savedAddresses.contains(it.address.hostAddress ?: "") }) { host ->
                ListItem(
                    headlineContent = { Text(host.response.daemonName) },
                    supportingContent = { Text(host.address.hostAddress ?: "") },
                    trailingContent = {
                        Button(onClick = { pairingTarget = host.response.daemonName to host.address }) {
                            Text("Eşleştir")
                        }
                    },
                )
            }
            items(savedList) { saved ->
                ListItem(
                    headlineContent = { Text(saved.name) },
                    supportingContent = { Text(saved.lastKnownAddress) },
                    trailingContent = {
                        Button(onClick = {
                            val addr = runCatching { InetAddress.getByName(saved.lastKnownAddress) }.getOrNull()
                            if (addr != null) onConnected(saved.name, addr)
                        }) { Text("Bağlan") }
                    },
                )
            }
        }

        Spacer(Modifier.height(16.dp))
        Text("Manuel IP", style = MaterialTheme.typography.titleMedium)
        Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = manualAddress,
                onValueChange = { manualAddress = it },
                label = { Text("192.168.1.x") },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            Button(onClick = {
                val addr = runCatching { InetAddress.getByName(manualAddress) }.getOrNull()
                if (addr != null) pairingTarget = manualAddress to addr
            }) { Text("Eşleştir") }
        }
    }

    val target = pairingTarget
    if (target != null) {
        PairingDialog(
            hostLabel = target.first,
            error = pairingError,
            onDismiss = { pairingTarget = null; pairingError = null },
            onSubmit = { pin ->
                scope.launch {
                    val ok = try {
                        client.pair(target.second, pin)
                    } catch (_: Exception) {
                        false
                    }
                    if (ok) {
                        savedHosts.upsert(SavedHost(target.first, target.second.hostAddress ?: target.first))
                        savedList = savedHosts.list()
                        pairingTarget = null
                        onConnected(target.first, target.second)
                    } else {
                        pairingError = "PIN yanlış veya cihaz yanıt vermedi"
                    }
                }
            },
        )
    }
}

@Composable
private fun PairingDialog(
    hostLabel: String,
    error: String?,
    onDismiss: () -> Unit,
    onSubmit: (Int) -> Unit,
) {
    var pinText by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("$hostLabel ile eşleştir") },
        text = {
            Column {
                Text("BacakOS masaüstünde gösterilen 6 haneli PIN'i gir.")
                OutlinedTextField(
                    value = pinText,
                    onValueChange = { pinText = it.filter(Char::isDigit).take(6) },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                )
                if (error != null) {
                    Text(error, color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { pinText.toIntOrNull()?.let(onSubmit) },
                enabled = pinText.length == 6,
            ) { Text("Eşleştir") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("İptal") } },
    )
}
