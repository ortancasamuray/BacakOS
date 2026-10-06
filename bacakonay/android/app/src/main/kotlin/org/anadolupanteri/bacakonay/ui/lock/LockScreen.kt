package org.anadolupanteri.bacakonay.ui.lock

import android.content.Intent
import android.provider.Settings
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.anadolupanteri.bacakonay.R
import org.anadolupanteri.bacakonay.data.vault.VaultManager

/**
 * Shown whenever the vault is locked. Opens the biometric prompt on its own
 * when the screen appears, so a normal launch is: open app → finger → codes.
 */
@Composable
fun LockScreen(vault: VaultManager, biometric: BiometricAuthenticator) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var invalidated by remember { mutableStateOf(false) }
    val unavailable = remember { biometric.unavailableReason() }

    fun unlock() {
        if (busy || unavailable != null) return
        busy = true
        message = null
        scope.launch {
            try {
                val pending = withContext(Dispatchers.Default) { vault.begin() }
                val (title, cipher) = when (pending) {
                    is VaultManager.Pending.Setup -> "Bacak Onay'ı kur" to pending.cipher
                    is VaultManager.Pending.Unlock -> "Bacak Onay'ın kilidini aç" to pending.cipher
                    VaultManager.Pending.Invalidated -> {
                        invalidated = true
                        return@launch
                    }
                }
                when (val r = biometric.authenticate(cipher, title, "Kodlarınız cihaz donanımında şifreli")) {
                    is BiometricAuthenticator.Outcome.Success ->
                        withContext(Dispatchers.Default) { vault.complete(pending, r.cipher) }
                    is BiometricAuthenticator.Outcome.Failed -> message = r.message
                    BiometricAuthenticator.Outcome.Cancelled -> Unit
                }
            } catch (e: Exception) {
                message = "Kasa açılamadı: ${e.message ?: e.javaClass.simpleName}"
            } finally {
                busy = false
            }
        }
    }

    LaunchedEffect(Unit) { unlock() }

    Column(
        modifier = Modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Image(painterResource(R.drawable.ic_shield), contentDescription = null, modifier = Modifier.size(96.dp))
        Spacer(Modifier.height(24.dp))
        Text("Bacak Onay", style = MaterialTheme.typography.headlineMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "BacakOS iki adımlı doğrulama",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(40.dp))
        if (unavailable != null) {
            Text(unavailable, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.error)
            Spacer(Modifier.height(16.dp))
            OutlinedButton(onClick = {
                context.startActivity(Intent(Settings.ACTION_SECURITY_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }) { Text("Güvenlik ayarlarını aç") }
        } else {
            Button(onClick = ::unlock, enabled = !busy, modifier = Modifier.fillMaxWidth().height(52.dp)) {
                Text(if (busy) "Bekleniyor…" else "Kilidi aç")
            }
            message?.let {
                Spacer(Modifier.height(16.dp))
                Text(it, textAlign = TextAlign.Center, color = MaterialTheme.colorScheme.error)
            }
        }
    }

    if (invalidated) {
        AlertDialog(
            onDismissRequest = {},
            title = { Text("Kasa anahtarı geçersiz") },
            text = {
                Text(
                    "Cihazın ekran kilidi kaldırıldığı için Android, kasayı koruyan donanım anahtarını sildi. " +
                        "Kayıtlı kodlar kurtarılamaz. Yeni bir kasa oluşturup hesaplarınızı (BacakOS'ta " +
                        "`sudo bacakonay kur --zorla`) yeniden eklemeniz gerekir.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    vault.reset()
                    invalidated = false
                    unlock()
                }) { Text("Yeni kasa oluştur") }
            },
        )
    }
}
