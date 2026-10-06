package org.anadolupanteri.bacakonay.ui.codes

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import androidx.compose.animation.animateColorAsState
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
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
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.withFrameMillis
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.anadolupanteri.bacakonay.R
import org.anadolupanteri.bacakonay.domain.model.OtpCode
import org.anadolupanteri.bacakonay.domain.otp.TotpGenerator
import org.anadolupanteri.bacakonay.ui.theme.BacakColors

private const val CLIPBOARD_CLEAR_MS = 30_000L

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun CodesScreen(viewModel: CodesViewModel, onScan: () -> Unit, onManual: () -> Unit) {
    val codes by viewModel.codes.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    var pendingDelete by remember { mutableStateOf<OtpCode?>(null) }

    // Frame clock for the rings (smooth drain between the 1 s code ticks).
    val nowMs by produceState(System.currentTimeMillis()) {
        while (true) withFrameMillis { value = System.currentTimeMillis() }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Bacak Onay") },
                actions = {
                    IconButton(onClick = onManual) {
                        Icon(painterResource(R.drawable.ic_keyboard), contentDescription = "Elle ekle")
                    }
                },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = onScan,
                icon = { Icon(painterResource(R.drawable.ic_qr), contentDescription = null) },
                text = { Text("QR tara") },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        if (codes.isEmpty()) {
            EmptyState(Modifier.padding(padding))
        } else {
            LazyColumn(
                contentPadding = PaddingValues(start = 16.dp, end = 16.dp, top = padding.calculateTopPadding() + 8.dp, bottom = 96.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(codes, key = { it.account.id }) { code ->
                    CodeCard(
                        code = code,
                        nowMs = nowMs,
                        onCopy = {
                            copySensitive(context, code.code)
                            scope.launch { snackbar.showSnackbar("Kod kopyalandı · 30 sn sonra panodan silinecek") }
                            scope.launch {
                                delay(CLIPBOARD_CLEAR_MS)
                                clearIfStillOurs(context, code.code)
                            }
                        },
                        onNextHotp = { viewModel.nextHotp(code.account.id) },
                        onLongPress = { pendingDelete = code },
                    )
                }
            }
        }
    }

    pendingDelete?.let { target ->
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("Hesap silinsin mi?") },
            text = {
                Text(
                    buildString {
                        append("${target.account.issuer} · ${target.account.accountName} bu telefondan silinecek.")
                        if (target.account.isBacakOs) {
                            append(" BacakOS'ta iki adımlı doğrulama açık kalırsa bu hesapla giriş yapılamaz; ")
                            append("önce bilgisayarda `sudo bacakonay kaldir` çalıştırın.")
                        }
                    },
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    viewModel.remove(target.account.id)
                    pendingDelete = null
                }) { Text("Sil", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { pendingDelete = null }) { Text("Vazgeç") } },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun CodeCard(code: OtpCode, nowMs: Long, onCopy: () -> Unit, onNextHotp: () -> Unit, onLongPress: () -> Unit) {
    val account = code.account
    val periodMs = account.period * 1000L
    val remainingMs = periodMs - Math.floorMod(nowMs, periodMs)
    val secondsLeft = ((remainingMs + 999) / 1000).toInt()
    val warn = code.secondsRemaining != null && secondsLeft <= EXPIRY_WARNING_SECONDS
    val codeColor by animateColorAsState(
        if (warn) BacakColors.Rust else MaterialTheme.colorScheme.onSurface,
        label = "codeColor",
    )

    Card(
        shape = RoundedCornerShape(20.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer),
        modifier = Modifier.fillMaxWidth().combinedClickable(onClick = onCopy, onLongClick = onLongPress),
    ) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Avatar(code)
            Spacer(Modifier.width(14.dp))
            Column(Modifier.weight(1f)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        account.issuer.ifEmpty { "Hesap" },
                        style = MaterialTheme.typography.titleSmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    if (account.isBacakOs) {
                        Spacer(Modifier.width(6.dp))
                        Text(
                            "giriş",
                            fontSize = 11.sp,
                            color = BacakColors.AegeanDeep,
                            modifier = Modifier
                                .clip(RoundedCornerShape(6.dp))
                                .background(BacakColors.AegeanPale)
                                .padding(horizontal = 6.dp, vertical = 1.dp),
                        )
                    }
                }
                Text(
                    account.accountName,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Spacer(Modifier.height(4.dp))
                Text(
                    TotpGenerator.formatForDisplay(code.code),
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.Bold,
                    fontSize = 30.sp,
                    letterSpacing = 2.sp,
                    color = codeColor,
                )
            }
            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                if (code.secondsRemaining != null) {
                    CountdownRing(fraction = remainingMs.toFloat() / periodMs, secondsLeft = secondsLeft)
                } else {
                    IconButton(onClick = onNextHotp) {
                        Icon(painterResource(R.drawable.ic_refresh), contentDescription = "Sonraki kod")
                    }
                }
                IconButton(onClick = onCopy) {
                    Icon(painterResource(R.drawable.ic_copy), contentDescription = "Kopyala")
                }
            }
        }
    }
}

@Composable
private fun Avatar(code: OtpCode) {
    val bacak = code.account.isBacakOs
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(44.dp)
            .clip(CircleShape)
            .background(if (bacak) BacakColors.AegeanMid else MaterialTheme.colorScheme.surfaceContainerHigh),
    ) {
        if (bacak) {
            Icon(
                painterResource(R.drawable.ic_computer),
                contentDescription = "BacakOS",
                tint = BacakColors.AegeanFoam,
                modifier = Modifier.size(24.dp),
            )
        } else {
            Text(
                code.account.issuer.firstOrNull()?.uppercase() ?: "?",
                fontWeight = FontWeight.Bold,
                color = MaterialTheme.colorScheme.onSurface,
            )
        }
    }
}

@Composable
private fun EmptyState(modifier: Modifier) {
    Column(
        modifier = modifier.fillMaxSize().padding(32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(
            painterResource(R.drawable.ic_computer),
            contentDescription = null,
            tint = MaterialTheme.colorScheme.primary,
            modifier = Modifier.size(56.dp),
        )
        Spacer(Modifier.height(16.dp))
        Text("Henüz hesap yok", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(8.dp))
        Text(
            "BacakOS'ta terminalde `sudo bacakonay kur` çalıştırın ve ekrandaki QR kodunu \"QR tara\" ile okutun.",
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

private fun copySensitive(context: Context, code: String) {
    val cm = context.getSystemService(ClipboardManager::class.java) ?: return
    val clip = ClipData.newPlainText("Bacak Onay", code)
    // Android 13+: keep the code out of the clipboard preview/overlay.
    clip.description.extras = PersistableBundle().apply {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
        } else {
            putBoolean("android.content.extra.IS_SENSITIVE", true)
        }
    }
    cm.setPrimaryClip(clip)
}

/** Best effort: only clears if the clipboard still holds our code. */
private fun clearIfStillOurs(context: Context, code: String) {
    val cm = context.getSystemService(ClipboardManager::class.java) ?: return
    val current = runCatching { cm.primaryClip?.getItemAt(0)?.text?.toString() }.getOrNull()
    if (current == code && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) cm.clearPrimaryClip()
}
