package org.anadolupanteri.uzakel.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
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
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.anadolupanteri.uzakel.R
import org.anadolupanteri.uzakel.discovery.PairingStore
import org.anadolupanteri.uzakel.discovery.SavedDevice
import org.anadolupanteri.uzakel.network.Connection
import org.anadolupanteri.uzakel.network.DiscoveredHost
import org.anadolupanteri.uzakel.network.NetworkClient
import org.anadolupanteri.uzakel.network.ResumeOutcome
import org.anadolupanteri.uzakel.network.ScannedPairingInfo
import org.anadolupanteri.uzakel.ui.theme.BacakColors
import java.net.InetAddress

/**
 * Home screen: "Bilgisayarlarım" (paired computers — one tap reconnects
 * without a PIN, ARCHITECTURE.md §2.3.3) and "Yeni bilgisayar ekle" (QR or
 * PIN pairing). The LAN is re-scanned every few seconds while this screen
 * is up, so each saved computer shows whether it's reachable right now.
 * [repairName], when set, opens straight into re-pairing that computer
 * (the control screen found the pairing was forgotten).
 */
@Composable
fun DeviceListScreen(
    client: NetworkClient,
    store: PairingStore,
    repairName: String?,
    onConnected: (Connection) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var devices by remember { mutableStateOf(store.list()) }
    var discovered by remember { mutableStateOf<List<DiscoveredHost>>(emptyList()) }
    var busyId by remember { mutableStateOf<String?>(null) }
    var message by remember { mutableStateOf<String?>(repairName?.let { "$it artık bu telefonu tanımıyor — yeniden eşleştirin." }) }
    var pairTarget by remember { mutableStateOf<Pair<String, InetAddress>?>(null) }
    var pairError by remember { mutableStateOf<String?>(null) }
    var autoPairing by remember { mutableStateOf(false) }
    var showQr by remember { mutableStateOf(false) }
    var confirmDelete by remember { mutableStateOf<SavedDevice?>(null) }
    var manualIp by remember { mutableStateOf("") }

    LaunchedEffect(Unit) {
        while (true) {
            discovered = runCatching { client.discoverHosts(timeoutMs = 1500) }.getOrDefault(emptyList())
            devices = store.list()
            delay(6_000)
        }
    }

    fun connect(d: SavedDevice) {
        busyId = d.id
        message = null
        scope.launch {
            val (conn, outcome) = Connection.open(client, store, d)
            busyId = null
            devices = store.list()
            when {
                conn != null -> onConnected(conn)
                outcome is ResumeOutcome.Rejected -> message = "${d.name} bu telefonu artık tanımıyor (30 gün kullanılmamış olabilir). Yeniden eşleştirin."
                else -> message = "${d.name} bulunamadı. Bilgisayar açık ve aynı Wi-Fi ağında mı?"
            }
        }
    }

    fun pair(label: String, addr: InetAddress, pin: Int) {
        pairTarget = label to addr
        pairError = null
        scope.launch {
            val result = runCatching { client.pair(addr, pin) }.getOrNull()
            if (result == null) {
                pairError = "PIN yanlış ya da bilgisayar yanıt vermedi"
                return@launch
            }
            val device = store.add(label, addr.hostAddress ?: label, result.clientId, result.resumeKey)
            pairTarget = null
            devices = store.list()
            onConnected(Connection(client, store, device, result.session))
        }
    }

    fun onQr(info: ScannedPairingInfo) {
        showQr = false
        val addr = runCatching { InetAddress.getByName(info.host) }.getOrNull()
        if (addr == null) {
            message = "QR'daki adres çözümlenemedi: ${info.host}"
            return
        }
        autoPairing = true
        pair(info.name ?: info.host, addr, info.pin)
    }

    if (showQr) {
        QrScanScreen(onScanned = ::onQr, onCancel = { showQr = false })
        return
    }

    val onlineNames = discovered.map { it.response.daemonName }.toSet()
    val newHosts = discovered.filterNot { h -> devices.any { it.name == h.response.daemonName } }

    LazyColumn(
        modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item {
            Spacer(Modifier.height(24.dp))
            Text("Uzakel", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Bold)
            Text("Telefonunla BacakOS'u kontrol et", color = MaterialTheme.colorScheme.onSurfaceVariant)
            message?.let {
                Spacer(Modifier.height(12.dp))
                Text(it, color = MaterialTheme.colorScheme.error)
            }
            Spacer(Modifier.height(12.dp))
            SectionTitle("Bilgisayarlarım")
        }
        if (devices.isEmpty()) {
            item { Text("Henüz eşleşmiş bilgisayar yok.", color = MaterialTheme.colorScheme.onSurfaceVariant) }
        }
        items(devices, key = { it.id }) { d ->
            DeviceCard(
                device = d,
                online = d.name in onlineNames,
                busy = busyId == d.id,
                onClick = { if (busyId == null) connect(d) },
                onDelete = { confirmDelete = d },
            )
        }
        item {
            Text(
                "30 gün kullanılmayan bilgisayarlar listeden otomatik silinir.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(16.dp))
            SectionTitle("Yeni bilgisayar ekle")
            Button(onClick = { showQr = true }, modifier = Modifier.fillMaxWidth().height(52.dp)) {
                Icon(painterResource(R.drawable.ic_qr), contentDescription = null)
                Spacer(Modifier.width(8.dp))
                Text("QR kodu ile eşleştir")
            }
            Text(
                "BacakOS'ta Kontrol Merkezi → \"Uzakel'e Bağlan\" ekranındaki QR'ı okutun.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 6.dp),
            )
        }
        if (newHosts.isNotEmpty()) {
            item { Text("Ağda bulunanlar", style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(top = 8.dp)) }
        }
        items(newHosts, key = { it.address.hostAddress ?: it.response.daemonName }) { h ->
            Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer)) {
                Row(Modifier.fillMaxWidth().padding(12.dp), verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(h.response.daemonName, fontWeight = FontWeight.SemiBold)
                        Text(h.address.hostAddress ?: "", style = MaterialTheme.typography.bodySmall)
                    }
                    OutlinedButton(onClick = {
                        autoPairing = false
                        pairTarget = h.response.daemonName to h.address
                        pairError = null
                    }) { Text("PIN ile eşleştir") }
                }
            }
        }
        item {
            Row(Modifier.fillMaxWidth().padding(vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = manualIp,
                    onValueChange = { manualIp = it.trim() },
                    label = { Text("Elle IP (ör. 192.168.1.20)") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(8.dp))
                OutlinedButton(onClick = {
                    val addr = runCatching { InetAddress.getByName(manualIp) }.getOrNull()
                    if (addr != null) {
                        autoPairing = false
                        pairTarget = manualIp to addr
                        pairError = null
                    } else {
                        message = "Geçersiz adres: $manualIp"
                    }
                }) { Text("Eşleştir") }
            }
            Spacer(Modifier.height(24.dp))
        }
    }

    pairTarget?.let { (label, addr) ->
        PairingDialog(
            hostLabel = label,
            error = pairError,
            auto = autoPairing && pairError == null,
            onDismiss = { pairTarget = null; pairError = null },
            onSubmit = { pin -> pair(label, addr, pin) },
        )
    }

    confirmDelete?.let { d ->
        AlertDialog(
            onDismissRequest = { confirmDelete = null },
            title = { Text("${d.name} silinsin mi?") },
            text = { Text("Yeniden bağlanmak için PIN ya da QR ile tekrar eşleştirmeniz gerekir.") },
            confirmButton = {
                TextButton(onClick = {
                    store.remove(d.id)
                    devices = store.list()
                    confirmDelete = null
                }) { Text("Sil", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { confirmDelete = null }) { Text("Vazgeç") } },
        )
    }
}

@Composable
private fun SectionTitle(text: String) {
    Text(text, style = MaterialTheme.typography.titleMedium, modifier = Modifier.padding(bottom = 6.dp))
}

@Composable
private fun DeviceCard(device: SavedDevice, online: Boolean, busy: Boolean, onClick: () -> Unit, onDelete: () -> Unit) {
    Card(
        shape = RoundedCornerShape(18.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    ) {
        Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
            Box(
                Modifier.size(44.dp).clip(CircleShape).background(BacakColors.AegeanMid),
                contentAlignment = Alignment.Center,
            ) {
                Icon(painterResource(R.drawable.ic_computer), contentDescription = null, tint = BacakColors.AegeanFoam)
            }
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Text(device.name, fontWeight = FontWeight.SemiBold)
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Box(Modifier.size(8.dp).clip(CircleShape).background(if (online) BacakColors.Online else MaterialTheme.colorScheme.outline))
                    Spacer(Modifier.width(6.dp))
                    Text(
                        if (online) "Ağda görünüyor" else "Son bağlantı: ${ago(device.lastSeen)}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (busy) {
                CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
            } else {
                IconButton(onClick = onDelete) { Icon(Icons.Default.Delete, contentDescription = "Sil") }
            }
        }
    }
}

private fun ago(t: Long): String {
    val d = (System.currentTimeMillis() - t) / 1000
    return when {
        d < 60 -> "az önce"
        d < 3600 -> "${d / 60} dk önce"
        d < 86_400 -> "${d / 3600} sa önce"
        else -> "${d / 86_400} gün önce"
    }
}

/**
 * PIN entry, or — when [auto] (a QR already carried the PIN) — just a
 * "Bağlanıyor…" status. [onDismiss] cancels a stuck attempt.
 */
@Composable
private fun PairingDialog(hostLabel: String, error: String?, auto: Boolean, onDismiss: () -> Unit, onSubmit: (Int) -> Unit) {
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
                        Text("Bağlanılıyor…")
                    }
                } else {
                    Text("BacakOS'ta Kontrol Merkezi → \"Uzakel'e Bağlan\" ekranındaki 6 haneli PIN'i girin.")
                    Spacer(Modifier.height(8.dp))
                    OutlinedTextField(
                        value = pinText,
                        onValueChange = { pinText = it.filter(Char::isDigit).take(6) },
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
                        singleLine = true,
                    )
                }
                error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            }
        },
        confirmButton = {
            if (!auto) {
                TextButton(onClick = { pinText.toIntOrNull()?.let(onSubmit) }, enabled = pinText.length == 6) { Text("Eşleştir") }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("İptal") } },
    )
}
