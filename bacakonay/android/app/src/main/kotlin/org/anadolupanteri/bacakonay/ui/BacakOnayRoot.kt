package org.anadolupanteri.bacakonay.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import org.anadolupanteri.bacakonay.AppContainer
import org.anadolupanteri.bacakonay.domain.otp.LoginApproval
import org.anadolupanteri.bacakonay.domain.otp.LoginRequest
import org.anadolupanteri.bacakonay.domain.otp.OtpAuthUri
import org.anadolupanteri.bacakonay.ui.approve.LoginApprovalScreen
import org.anadolupanteri.bacakonay.ui.add.ManualEntryScreen
import org.anadolupanteri.bacakonay.ui.codes.CodesScreen
import org.anadolupanteri.bacakonay.ui.codes.CodesViewModel
import org.anadolupanteri.bacakonay.ui.lock.BiometricAuthenticator
import org.anadolupanteri.bacakonay.ui.lock.LockScreen
import org.anadolupanteri.bacakonay.ui.scan.QrScanScreen

private enum class Screen { Codes, Scan, Manual, Approve }

/**
 * Top-level navigation. Three screens don't justify navigation-compose: a
 * small state machine, gated by the vault lock (locked → only [LockScreen]).
 */
@Composable
fun BacakOnayRoot(activity: FragmentActivity, container: AppContainer) {
    val unlocked by container.repository.isUnlocked.collectAsStateWithLifecycle()
    val biometric = remember(activity) { BiometricAuthenticator(activity) }
    val codesVm: CodesViewModel = viewModel(factory = viewModelFactory { initializer { CodesViewModel(container.repository) } })
    var screen by remember { mutableStateOf(Screen.Codes) }
    var scanError by remember { mutableStateOf<String?>(null) }
    // Bumped on a rejected QR so the scanner remounts with a fresh analyzer.
    var scanAttempt by remember { mutableStateOf(0) }
    // What the scanner is for: a login to approve, or an account to add.
    var scanForLogin by remember { mutableStateOf(true) }
    var loginRequest by remember { mutableStateOf<LoginRequest?>(null) }
    val accounts by container.repository.accounts.collectAsStateWithLifecycle()

    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        if (!unlocked) {
            screen = Screen.Codes
            LockScreen(container.vault, biometric)
            return@Surface
        }
        BackHandler(enabled = screen != Screen.Codes) { screen = Screen.Codes }
        when (screen) {
            Screen.Codes -> CodesScreen(
                viewModel = codesVm,
                onLogin = { scanError = null; scanForLogin = true; screen = Screen.Scan },
                onScan = { scanError = null; scanForLogin = false; screen = Screen.Scan },
                onManual = { screen = Screen.Manual },
            )
            Screen.Scan -> key(scanAttempt) {
                QrScanScreen(
                    error = scanError,
                    hint = if (scanForLogin) {
                        "Bilgisayarın giriş ekranında, Bacak Onay kodu sorulurken çıkan QR'ı çerçeveye alın"
                    } else {
                        "Bilgisayardaki Bacak Onay penceresinin gösterdiği QR'ı çerçeveye alın"
                    },
                    onScanned = { raw ->
                        // Either kind of QR works from either button.
                        when (val login = LoginApproval.parse(raw)) {
                            is LoginApproval.Result.Ok -> {
                                loginRequest = login.request
                                screen = Screen.Approve
                                return@QrScanScreen
                            }
                            is LoginApproval.Result.Error -> {
                                scanError = login.reason
                                scanAttempt++
                                return@QrScanScreen
                            }
                            null -> {}
                        }
                        when (val r = OtpAuthUri.parse(raw)) {
                            is OtpAuthUri.Result.Ok -> {
                                codesVm.add(r.account)
                                screen = Screen.Codes
                            }
                            is OtpAuthUri.Result.Error -> {
                                scanError = r.reason
                                scanAttempt++
                            }
                        }
                    },
                    onBack = { screen = Screen.Codes },
                )
            }
            Screen.Approve -> loginRequest?.let { req ->
                LoginApprovalScreen(
                    request = req,
                    account = req.matchAccount(accounts),
                    onDone = { loginRequest = null; screen = Screen.Codes },
                )
            } ?: run { screen = Screen.Codes }
            Screen.Manual -> ManualEntryScreen(
                onSave = { codesVm.add(it); screen = Screen.Codes },
                onBack = { screen = Screen.Codes },
            )
        }
    }
}
