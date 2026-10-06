package org.anadolupanteri.bacakonay.data

import org.anadolupanteri.bacakonay.data.crypto.CryptoManager
import org.anadolupanteri.bacakonay.data.vault.VaultStore
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpType
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import javax.crypto.AEADBadTagException

/** Software side of the vault (the Keystore wrap needs a device). */
class VaultCodecTest {

    @Test
    fun accountsRoundTripThroughJson() {
        val accounts = listOf(
            OtpAccount("1", "BacakOS", "os2@pc", "12345678901234567890".toByteArray(), createdAt = 5),
            OtpAccount("2", "X", "y", ByteArray(32) { it.toByte() }, OtpType.HOTP, OtpAlgorithm.SHA256, 8, 60, 42, 7),
        )
        assertEquals(accounts, VaultStore.decode(VaultStore.encode(accounts)))
    }

    @Test
    fun sealOpenAndTamperDetection() {
        val key = CryptoManager.newVaultKey()
        val aad = "bacakonay.vault.v1".toByteArray()
        val blob = CryptoManager.seal(key, "gizli".toByteArray(), aad)
        assertArrayEquals("gizli".toByteArray(), CryptoManager.open(key, blob, aad))
        // Flip one ciphertext bit → GCM tag check fails.
        val tampered = blob.copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() }
        assertThrows(AEADBadTagException::class.java) { CryptoManager.open(key, tampered, aad) }
        // Wrong AAD (e.g. a blob from another file) also fails.
        assertThrows(AEADBadTagException::class.java) { CryptoManager.open(key, blob, "baska".toByteArray()) }
        // Fresh IV per seal.
        assert(!CryptoManager.seal(key, "a".toByteArray(), aad).contentEquals(CryptoManager.seal(key, "a".toByteArray(), aad)))
    }
}
