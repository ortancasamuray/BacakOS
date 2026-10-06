package org.anadolupanteri.bacakonay.data.crypto

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Hardware-backed key handling for the vault.
 *
 * Two-level design (the same shape as Aegis):
 *
 * 1. **Wrap key** — AES-256-GCM in the Android Keystore (StrongBox when the
 *    device has one, otherwise the TEE). It never leaves the secure hardware
 *    and is usable **only after a fresh user authentication** for each
 *    operation: biometric (or, on Android 11+, the device PIN/pattern too).
 *    It is used with `BiometricPrompt.CryptoObject`, so the authentication is
 *    cryptographically bound to the decryption — not just a UI gate.
 * 2. **Vault key** — 256 random bits that encrypt the account list. It is
 *    stored only wrapped by (1); after unlocking it lives in memory until the
 *    app locks, so adding/removing accounts doesn't need a second prompt.
 */
class CryptoManager(private val alias: String = WRAP_KEY_ALIAS) {

    private val keyStore: KeyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }

    fun hasWrapKey(): Boolean = keyStore.containsAlias(alias)

    fun deleteWrapKey() {
        if (hasWrapKey()) keyStore.deleteEntry(alias)
    }

    /** Cipher to wrap a new vault key; authenticate it via BiometricPrompt before use. */
    fun wrapCipher(): Cipher {
        val key = wrapKey() ?: createWrapKey()
        return Cipher.getInstance(TRANSFORMATION).apply { init(Cipher.ENCRYPT_MODE, key) }
    }

    /**
     * Cipher to unwrap the vault key; authenticate it via BiometricPrompt.
     * Throws [android.security.keystore.KeyPermanentlyInvalidatedException]
     * if the device lock was removed (the key is gone for good).
     */
    fun unwrapCipher(iv: ByteArray): Cipher {
        val key = wrapKey() ?: throw IllegalStateException("wrap key missing")
        return Cipher.getInstance(TRANSFORMATION).apply {
            init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, iv))
        }
    }

    private fun wrapKey(): SecretKey? = (keyStore.getEntry(alias, null) as? KeyStore.SecretKeyEntry)?.secretKey

    private fun createWrapKey(): SecretKey {
        fun spec(strongBox: Boolean): KeyGenParameterSpec {
            val b = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .setRandomizedEncryptionRequired(true)
                .setUserAuthenticationRequired(true)
                // Enrolling a new fingerprint must not destroy the user's 2FA
                // secrets (there is no other copy). Removing the screen lock
                // still invalidates the key, as Android mandates.
                .setInvalidatedByBiometricEnrollment(false)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                // Per-use auth (timeout 0) with biometric OR device credential.
                b.setUserAuthenticationParameters(
                    0,
                    KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL,
                )
            } else {
                // Before Android 11 per-use keys accept strong biometrics only.
                @Suppress("DEPRECATION")
                b.setUserAuthenticationValidityDurationSeconds(-1)
            }
            if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) b.setIsStrongBoxBacked(true)
            return b.build()
        }

        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
        return try {
            gen.init(spec(strongBox = true))
            gen.generateKey()
        } catch (_: Exception) {
            // No StrongBox (most phones: StrongBoxUnavailableException) → TEE.
            gen.init(spec(strongBox = false))
            gen.generateKey()
        }
    }

    companion object {
        const val WRAP_KEY_ALIAS = "bacakonay.vault.wrap.v1"
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val GCM_TAG_BITS = 128
        private const val GCM_IV_BYTES = 12
        private val random = SecureRandom()

        fun newVaultKey(): ByteArray = ByteArray(32).also(random::nextBytes)

        /** AES-256-GCM with a software key: returns `iv || ciphertext+tag`. */
        fun seal(key: ByteArray, plaintext: ByteArray, aad: ByteArray): ByteArray {
            val iv = ByteArray(GCM_IV_BYTES).also(random::nextBytes)
            val c = Cipher.getInstance(TRANSFORMATION)
            c.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(GCM_TAG_BITS, iv))
            c.updateAAD(aad)
            return iv + c.doFinal(plaintext)
        }

        /** Inverse of [seal]; throws `AEADBadTagException` on tampering. */
        fun open(key: ByteArray, blob: ByteArray, aad: ByteArray): ByteArray {
            require(blob.size > GCM_IV_BYTES) { "short blob" }
            val c = Cipher.getInstance(TRANSFORMATION)
            c.init(
                Cipher.DECRYPT_MODE,
                SecretKeySpec(key, "AES"),
                GCMParameterSpec(GCM_TAG_BITS, blob, 0, GCM_IV_BYTES),
            )
            c.updateAAD(aad)
            return c.doFinal(blob, GCM_IV_BYTES, blob.size - GCM_IV_BYTES)
        }
    }
}
