package org.anadolupanteri.uzakel.ui

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import org.anadolupanteri.uzakel.network.PairedSession
import org.anadolupanteri.uzakel.transfer.FileTransferManager
import org.anadolupanteri.uzakel.transfer.Transfer
import org.anadolupanteri.uzakel.transfer.TransferState

/**
 * File-transfer panel (ARCHITECTURE.md §4's `transfer/` bullet): a
 * Storage-Access-Framework file picker plus a live list of in-flight and
 * finished transfers, each backed by [FileTransferManager.transfers].
 *
 * Only sending is wired up here — the daemon only implements the receive
 * direction so far (see `../../ARCHITECTURE.md` §5), so there's no
 * "incoming file" list to show yet.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun TransferScreen(
    session: PairedSession,
    manager: FileTransferManager,
    onBack: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    val transfers by manager.transfers.collectAsState()

    val pickFile = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) {
            scope.launch { manager.send(uri, session) }
        }
    }

    Column(modifier = Modifier.fillMaxSize()) {
        TopAppBar(
            title = { Text("Dosya Transferi") },
            navigationIcon = { OutlinedButton(onClick = onBack) { Text("Geri") } },
        )

        Button(
            onClick = { pickFile.launch(arrayOf("*/*")) },
            modifier = Modifier.fillMaxWidth().padding(16.dp),
        ) { Text("Dosya Gönder") }

        LazyColumn(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            items(transfers, key = { it.id }) { transfer -> TransferRow(transfer) }
        }
    }
}

@Composable
private fun TransferRow(transfer: Transfer) {
    Column(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Text(transfer.name, style = MaterialTheme.typography.bodyLarge)
        Text(statusLabel(transfer.state), style = MaterialTheme.typography.bodySmall)
        if (transfer.state is TransferState.Sending && transfer.totalBytes > 0) {
            LinearProgressIndicator(
                progress = { (transfer.sentBytes.toFloat() / transfer.totalBytes.toFloat()).coerceIn(0f, 1f) },
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

private fun statusLabel(state: TransferState): String = when (state) {
    is TransferState.Hashing -> "SHA-256 hesaplanıyor…"
    is TransferState.Sending -> "Gönderiliyor…"
    is TransferState.Done -> "Tamamlandı"
    is TransferState.Rejected -> "Reddedildi: ${state.reason}"
    is TransferState.Corrupt -> "Bozuk — karşı taraf sildi"
    is TransferState.Failed -> "Hata: ${state.message}"
}
