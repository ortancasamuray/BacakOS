package org.anadolupanteri.uzakel.transfer

import android.content.Context
import android.net.Uri
import android.provider.OpenableColumns
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import org.anadolupanteri.uzakel.network.FileSendResult
import org.anadolupanteri.uzakel.network.NetworkClient
import java.net.InetAddress
import java.util.UUID

/** One entry in the transfer list the UI (`ui/TrackpadScreen.kt`'s transfer
 * panel) renders — see ARCHITECTURE.md §4's `transfer/` bullet. */
data class Transfer(
    val id: String,
    val name: String,
    val totalBytes: Long,
    val sentBytes: Long = 0,
    val state: TransferState = TransferState.Hashing,
)

sealed class TransferState {
    object Hashing : TransferState()
    object Sending : TransferState()
    object Done : TransferState()
    data class Rejected(val reason: String) : TransferState()
    object Corrupt : TransferState()
    data class Failed(val message: String) : TransferState()
}

/**
 * Picks a file via Android's Storage Access Framework (never a raw file
 * path — the app never requests broad storage permissions, matching the
 * "sandboxed by design" instinct `altay`'s `security::Sandbox` follows on
 * the desktop side, see ARCHITECTURE.md §4) and streams it to the daemon
 * over [NetworkClient.sendFile].
 *
 * SHA-256 is computed in a first full pass over the SAF stream before
 * sending, since the protocol's handshake (`FILE_META`) needs the hash
 * up front — see ARCHITECTURE.md §2.2. For very large files this means
 * reading the content twice (hash, then send); an incremental digest
 * computed alongside the send would remove that cost but was left for
 * later, see the open questions list.
 */
class FileTransferManager(private val context: Context) {
    private val client = NetworkClient()
    private val _transfers = MutableStateFlow<List<Transfer>>(emptyList())
    val transfers: StateFlow<List<Transfer>> = _transfers

    suspend fun send(uri: Uri, host: InetAddress) {
        val id = UUID.randomUUID().toString()
        val name = queryDisplayName(uri) ?: uri.lastPathSegment ?: "dosya"
        val size = querySize(uri) ?: -1L

        _transfers.update { it + Transfer(id, name, size, state = TransferState.Hashing) }

        val digest = try {
            hashContent(uri)
        } catch (e: Exception) {
            update(id) { it.copy(state = TransferState.Failed(e.message ?: "hash failed")) }
            return
        }

        update(id) { it.copy(state = TransferState.Sending) }

        val resolver = context.contentResolver
        val result = resolver.openInputStream(uri)?.use { input ->
            client.sendFile(
                host = host,
                name = name,
                size = size,
                sha256 = digest,
                input = input,
                onProgress = { sent, total -> update(id) { it.copy(sentBytes = sent, totalBytes = total) } },
            )
        } ?: FileSendResult.Failed(IllegalStateException("could not open $uri"))

        val newState = when (result) {
            is FileSendResult.Success -> TransferState.Done
            is FileSendResult.Rejected -> TransferState.Rejected(result.reason)
            is FileSendResult.Corrupt -> TransferState.Corrupt
            is FileSendResult.Failed -> TransferState.Failed(result.cause.message ?: "transfer failed")
        }
        update(id) { it.copy(state = newState) }
    }

    private fun hashContent(uri: Uri): ByteArray {
        val digest = java.security.MessageDigest.getInstance("SHA-256")
        context.contentResolver.openInputStream(uri)?.use { input ->
            val buf = ByteArray(64 * 1024)
            while (true) {
                val read = input.read(buf)
                if (read <= 0) break
                digest.update(buf, 0, read)
            }
        } ?: throw IllegalStateException("could not open $uri for hashing")
        return digest.digest()
    }

    private fun queryDisplayName(uri: Uri): String? =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(0) else null
        }

    private fun querySize(uri: Uri): Long? =
        context.contentResolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst() && !cursor.isNull(0)) cursor.getLong(0) else null
        }

    private fun update(id: String, transform: (Transfer) -> Transfer) {
        _transfers.update { list -> list.map { if (it.id == id) transform(it) else it } }
    }
}
