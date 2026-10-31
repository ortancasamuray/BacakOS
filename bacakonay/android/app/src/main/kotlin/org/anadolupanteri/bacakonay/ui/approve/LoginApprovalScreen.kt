package org.anadolupanteri.bacakonay.ui.approve

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.launch
import org.anadolupanteri.bacakonay.R
import org.anadolupanteri.bacakonay.data.network.LoginApprovalClient
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.otp.LoginApproval
import org.anadolupanteri.bacakonay.domain.otp.LoginRequest
import org.anadolupanteri.bacakonay.domain.otp.TotpGenerator

private sealed interface State {
    data object Asking : State
    data object Sending : State
    data object Done : State
    data class Failed(val reason: String) : State
}

/**
 * "Girişi onayla": which computer and user the scanned login QR is for, and
 * one tap to send the code. No extra biometric: the vault is already open
 * (it was unlocked with the phone's own screen lock to get here).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LoginApprovalScreen(request: LoginRequest, account: OtpAccount?, onDone: () -> Unit) {
    var state by remember { mutableStateOf<State>(State.Asking) }
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Girişi onayla") },
                navigationIcon = {
                    IconButton(onClick = onDone) { Icon(painterResource(R.drawable.ic_back), contentDescription = "Geri") }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Card(Modifier.fillMaxWidth()) {
                Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("Bilgisayar", style = MaterialTheme.typography.labelMedium)
                    Text(request.machine.ifEmpty { request.host }, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
                    Text(request.host, style = MaterialTheme.typography.bodySmall)
                    Spacer(Modifier.height(8.dp))
                    Text("Kullanıcı", style = MaterialTheme.typography.labelMedium)
                    Text(request.user, fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
                }
            }

            if (account == null) {
                Text(
                    "Bu telefonda ${request.user} için kayıtlı bir BacakOS hesabı yok. Önce bilgisayarda " +
                        "Bacak Onay penceresinden telefonu eşleyin.",
                    color = MaterialTheme.colorScheme.error,
                    textAlign = TextAlign.Center,
                )
                OutlinedButton(onClick = onDone, Modifier.fillMaxWidth()) { Text("Geri") }
                return@Column
            }

            when (val s = state) {
                State.Asking, is State.Failed -> {
                    Text(
                        "Bu girişi şu anda siz başlattıysanız onaylayın. Başkası bu QR'ı size okutuyorsa onaylamayın.",
                        textAlign = TextAlign.Center,
                    )
                    if (s is State.Failed) {
                        Text(s.reason, color = MaterialTheme.colorScheme.error, textAlign = TextAlign.Center)
                    }
                    Button(
                        onClick = {
                            state = State.Sending
                            scope.launch {
                                // The code of this very moment; the computer allows ±30 s drift.
                                val code = TotpGenerator.codeFor(account, System.currentTimeMillis()).code
                                val frame = LoginApproval.seal(request, code)
                                state = when (val r = LoginApprovalClient.send(request, frame)) {
                                    LoginApprovalClient.Outcome.Delivered -> State.Done
                                    is LoginApprovalClient.Outcome.Failed -> State.Failed(r.reason)
                                }
                            }
                        },
                        modifier = Modifier.fillMaxWidth().height(56.dp),
                    ) { Text(if (s is State.Failed) "Yeniden dene" else "Onayla", fontSize = 18.sp) }
                    OutlinedButton(onClick = onDone, Modifier.fillMaxWidth()) { Text("Vazgeç") }
                }
                State.Sending -> {
                    CircularProgressIndicator()
                    Text("Gönderiliyor…")
                }
                State.Done -> {
                    Text("✓ Onay gönderildi", fontSize = 22.sp, fontWeight = FontWeight.SemiBold)
                    Text("Bilgisayar ekranından devam edin.", textAlign = TextAlign.Center)
                    Button(onClick = onDone, Modifier.fillMaxWidth()) { Text("Tamam") }
                }
            }
        }
    }
}
