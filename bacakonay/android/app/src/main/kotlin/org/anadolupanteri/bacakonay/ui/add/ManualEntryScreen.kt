package org.anadolupanteri.bacakonay.ui.add

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import org.anadolupanteri.bacakonay.R
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpType
import org.anadolupanteri.bacakonay.domain.otp.Base32
import java.util.UUID

/** Manual fallback when the QR can't be scanned (no camera, remote session…). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ManualEntryScreen(onSave: (OtpAccount) -> Unit, onBack: () -> Unit) {
    var issuer by remember { mutableStateOf(OtpAccount.BACAKOS_ISSUER) }
    var account by remember { mutableStateOf("") }
    var secret by remember { mutableStateOf("") }
    var type by remember { mutableStateOf(OtpType.TOTP) }
    var algorithm by remember { mutableStateOf(OtpAlgorithm.SHA1) }
    var digits by remember { mutableStateOf(6) }
    var error by remember { mutableStateOf<String?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Elle ekle") },
                navigationIcon = {
                    IconButton(onClick = onBack) { Icon(painterResource(R.drawable.ic_back), contentDescription = "Geri") }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding).padding(16.dp).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(issuer, { issuer = it }, label = { Text("Sağlayıcı") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(account, { account = it }, label = { Text("Hesap (ör. ayse@bilgisayar)") }, singleLine = true, modifier = Modifier.fillMaxWidth())
            OutlinedTextField(
                secret,
                { secret = it },
                label = { Text("Gizli anahtar (base32)") },
                singleLine = true,
                visualTransformation = PasswordVisualTransformation(),
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Characters, keyboardType = KeyboardType.Password, autoCorrectEnabled = false),
                modifier = Modifier.fillMaxWidth(),
            )
            Text("Tür", style = MaterialTheme.typography.labelLarge)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OtpType.entries.forEach { t -> FilterChip(selected = type == t, onClick = { type = t }, label = { Text(t.name) }) }
            }
            Text("Algoritma", style = MaterialTheme.typography.labelLarge)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OtpAlgorithm.entries.forEach { a -> FilterChip(selected = algorithm == a, onClick = { algorithm = a }, label = { Text(a.uriName) }) }
            }
            Text("Hane", style = MaterialTheme.typography.labelLarge)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                listOf(6, 8).forEach { d -> FilterChip(selected = digits == d, onClick = { digits = d }, label = { Text("$d") }) }
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
            Button(
                modifier = Modifier.fillMaxWidth().height(52.dp),
                onClick = {
                    val key = Base32.decode(secret)
                    error = when {
                        account.isBlank() -> "Hesap adı gerekli"
                        key == null -> "Gizli anahtar geçerli base32 değil"
                        key.size < 10 -> "Gizli anahtar çok kısa (en az 16 karakter)"
                        else -> null
                    }
                    if (error == null && key != null) {
                        onSave(
                            OtpAccount(
                                id = UUID.randomUUID().toString(),
                                issuer = issuer.trim(),
                                accountName = account.trim(),
                                secret = key,
                                type = type,
                                algorithm = algorithm,
                                digits = digits,
                                createdAt = System.currentTimeMillis(),
                            ),
                        )
                    }
                },
            ) { Text("Kaydet") }
        }
    }
}
