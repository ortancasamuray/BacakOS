package org.anadolupanteri.bacakonay.ui.lock

import android.os.Build
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import kotlinx.coroutines.suspendCancellableCoroutine
import javax.crypto.Cipher
import kotlin.coroutines.resume

/**
 * Coroutine wrapper around [BiometricPrompt] with a [BiometricPrompt.CryptoObject]:
 * the returned cipher is the Keystore cipher *unlocked by* this authentication.
 */
class BiometricAuthenticator(private val activity: FragmentActivity) {

    sealed interface Outcome {
        data class Success(val cipher: Cipher) : Outcome
        data class Failed(val message: String) : Outcome
        data object Cancelled : Outcome
    }

    /** Android 11+ allows PIN/pattern with a CryptoObject; older, biometrics only. */
    private val authenticators: Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) BIOMETRIC_STRONG or DEVICE_CREDENTIAL else BIOMETRIC_STRONG

    /** `null` when usable, else a user-facing reason. */
    fun unavailableReason(): String? = when (BiometricManager.from(activity).canAuthenticate(authenticators)) {
        BiometricManager.BIOMETRIC_SUCCESS -> null
        BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                "Cihazda ekran kilidi (PIN, desen ya da parmak izi) tanımlı değil."
            } else {
                "Bu Android sürümünde Bacak Onay için parmak izi tanımlı olmalı."
            }
        BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE,
        BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE -> "Cihazın güvenli kimlik doğrulama donanımı kullanılamıyor."
        BiometricManager.BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED -> "Cihaz için güvenlik güncellemesi gerekli."
        else -> "Kimlik doğrulama kullanılamıyor."
    }

    suspend fun authenticate(cipher: Cipher, title: String, subtitle: String): Outcome =
        suspendCancellableCoroutine { cont ->
            val prompt = BiometricPrompt(
                activity,
                ContextCompat.getMainExecutor(activity),
                object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                        val c = result.cryptoObject?.cipher
                        if (cont.isActive) {
                            cont.resume(if (c != null) Outcome.Success(c) else Outcome.Failed("Şifreleme nesnesi alınamadı"))
                        }
                    }

                    override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                        if (!cont.isActive) return
                        val cancelled = errorCode == BiometricPrompt.ERROR_USER_CANCELED ||
                            errorCode == BiometricPrompt.ERROR_NEGATIVE_BUTTON ||
                            errorCode == BiometricPrompt.ERROR_CANCELED
                        cont.resume(if (cancelled) Outcome.Cancelled else Outcome.Failed(errString.toString()))
                    }
                    // onAuthenticationFailed (one bad finger) keeps the prompt open.
                },
            )
            val info = BiometricPrompt.PromptInfo.Builder()
                .setTitle(title)
                .setSubtitle(subtitle)
                .setAllowedAuthenticators(authenticators)
                .apply { if (authenticators and DEVICE_CREDENTIAL == 0) setNegativeButtonText("Vazgeç") }
                .setConfirmationRequired(false)
                .build()
            prompt.authenticate(info, BiometricPrompt.CryptoObject(cipher))
            cont.invokeOnCancellation { prompt.cancelAuthentication() }
        }
}
