package org.anadolupanteri.uzakel.discovery

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Seals small secrets (pairing resume keys) with a non-exportable AES-256-GCM
 * key in the Android Keystore. No user-authentication requirement: the app
 * has to reconnect in the background after a Wi-Fi handoff without a
 * prompt — the protection here is that the raw key never sits in app
 * storage or backups, only its Keystore-sealed form.
 */
object KeystoreBox {
    private const val ALIAS = "uzakel.pairings.v1"
    private const val IV_LEN = 12

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    /** `iv || ciphertext+tag`. */
    fun seal(plain: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, key())
        return c.iv + c.doFinal(plain)
    }

    /** `null` if the blob is corrupt or the Keystore key is gone. */
    fun open(blob: ByteArray): ByteArray? = runCatching {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, blob, 0, IV_LEN))
        c.doFinal(blob, IV_LEN, blob.size - IV_LEN)
    }.getOrNull()
}
