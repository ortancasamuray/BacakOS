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
import org.anadolupanteri.uzakel.network.PairedSession
import org.anadolupanteri.uzakel.network.ScannedPairingInfo
import java.net.InetAddress

/**
 * Device list + discovery + pairing (ARCHITECTURE.md §4's `discovery/`
 * and §2.3's PIN flow). Tapping a discovered host that isn't already
 * paired prompts for the PIN the daemon showed as a desktop notification;
 * a successful pair saves the host and hands control (with the session
 * keys the pairing derived) to [onConnected].
 */
@Composable
fun DeviceListScreen(
    client: NetworkClient,
    savedHosts: SavedHostsStore,
    onConnected: (name: String, session: PairedSession) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var scanning by remember { mutableStateOf(false) }
    var discovered by remember { mutableStateOf<List<DiscoveredHost>>(emptyList()) }
    var savedList by remember { mutableStateOf(savedHosts.list()) }
    var manualAddress by remember { mutableStateOf("") }
    var pairingTarget by remember { mutableStateOf<Pair<String, InetAddress>?>(null) }
    var pairingError by remember { mutableStateOf<String?>(null) }
    var showQrScan by remember { mutableStateOf(false) }
    // true while the in-flight pair() came from a scanned QR (has its PIN
    // already) rather than the manual-entry dialog (still waiting on one).
    var autoPairing by remember { mutableStateOf(false) }
    var scanResolveError by remember { mutableStateOf<String?>(null) }

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

    // Shared by both the manual-PIN dialog and a successful QR scan (which
    // just skips straight to this with the PIN the code carried).
    fun submitPair(hostLabel: String, address: InetAddress, pin: Int) {
        pairingTarget = hostLabel to address
        pairingError = null
        scope.launch {
            val session = try {
                client.pair(address, pin)
            } catch (_: Exception) {
                null
            }
            if (session != null) {
                savedHosts.upsert(SavedHost(hostLabel, address.hostAddress ?: hostLabel))
                savedList = savedHosts.list()
                pairingTarget = null
                onConnected(hostLabel, session)
            } else {
                pairingError = "PIN yanlış veya cihaz yanıt vermedi"
            }
        }
    }

    fun onQrScanned(info: ScannedPairingInfo) {
        showQrScan = false
        val addr = runCatching { InetAddress.getByName(info.host) }.getOrNull()
        if (addr != null) {
            scanResolveError = null
            autoPairing = true
            submitPair(info.name ?: info.host, addr, info.pin)
        } else {
            scanResolveError = "QR'daki adres çözümlenemedi: ${info.host}"
        }
    }

    if (showQrScan) {
        QrScanScreen(onScanned = ::onQrScanned, onCancel = { showQrScan = false })
        return
    }

    Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
        Text("Uzakel — Cihaz Bul", style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(12.dp))

        Row(verticalAlignment = Alignment.CenterVertically) {
            Button(onClick = ::scan, enabled = !scanning) { Text("Tara") }
            Spacer(Modifier.width(12.dp))
            Button(onClick = { showQrScan = true }) { Text("QR ile Eşleştir") }
            Spacer(Modifier.width(12.dp))
            if (scanning) CircularProgressIndicator(modifier = Modifier.size(20.dp))
        }
        if (scanResolveError != null) {
            Spacer(Modifier.height(8.dp))
            Text(scanResolveError!!, color = MaterialTheme.colorScheme.error)
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
                        Button(onClick = {
                            autoPairing = false
                            pairingTarget = host.response.daemonName to host.address
                            pairingError = null
                        }) {
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
                        // Always re-pairs, never connects directly: the
                        // daemon's pairing trust is in-memory only (see
                        // uzakel-daemon/src/trust.rs) and resets on every
                        // daemon restart, so a "saved" host from a previous
                        // session can't be assumed still trusted — skipping
                        // the PIN here would silently connect to a session
                        // whose input/file traffic the daemon then drops.
                        Button(onClick = {
                            val addr = runCatching { InetAddress.getByName(saved.lastKnownAddress) }.getOrNull()
                            if (addr != null) {
                                autoPairing = false
                                pairingTarget = saved.name to addr
                                pairingError = null
                            }
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
                if (addr != null) {
                    autoPairing = false
                    pairingTarget = manualAddress to addr
                    pairingError = null
                }
            }) { Text("Eşleştir") }
        }
    }

    val target = pairingTarget
    if (target != null) {
        PairingDialog(
            hostLabel = target.first,
            error = pairingError,
            auto = autoPairing,
            onDismiss = { pairingTarget = null; pairingError = null },
            onSubmit = { pin -> submitPair(target.first, target.second, pin) },
        )
    }
}

/**
 * PIN entry, or — when [auto] (a QR scan already carried a PIN) — just a
 * "Bağlanıyor…" status with no field, since [DeviceListScreen] has already
 * called `submitPair` by the time this shows. Either way `onDismiss` cancels
 * out of a pairing attempt that's stuck (wrong PIN, unreachable daemon).
 */
@Composable
private fun PairingDialog(
    hostLabel: String,
    error: String?,
    auto: Boolean,
    onDismiss: () -> Unit,
    onSubmit: (Int) -> Unit,
) {
    var pinText by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("$hostLabel ile eşleştir") },
        text = {
            Column {
                if (auto) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        CircularProgressIndicator(modifier = Modifier.size(20.dp))
                        Spacer(Modifier.width(12.dp))
                        Text("Taranan PIN ile bağlanılıyor…")
                    }
                } else {
                    Text("BacakOS masaüstünde gösterilen 6 haneli PIN'i gir.")
                    OutlinedTextField(
                        value = pinText,
                        onValueChange = { pinText = it.filter(Char::isDigit).take(6) },
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    )
                }
                if (error != null) {
                    Text(error, color = MaterialTheme.colorScheme.error)
                }
            }
        },
        confirmButton = {
            if (!auto) {
                TextButton(
                    onClick = { pinText.toIntOrNull()?.let(onSubmit) },
                    enabled = pinText.length == 6,
                ) { Text("Eşleştir") }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("İptal") } },
    )
}
