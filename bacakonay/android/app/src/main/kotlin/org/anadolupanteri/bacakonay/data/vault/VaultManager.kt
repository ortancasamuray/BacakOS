package org.anadolupanteri.bacakonay.data.vault

import android.security.keystore.KeyPermanentlyInvalidatedException
import org.anadolupanteri.bacakonay.data.crypto.CryptoManager
import javax.crypto.Cipher

/**
 * Lock/unlock state machine for the vault. The UI asks for a [Cipher], has
 * the user authenticate it through `BiometricPrompt.CryptoObject`, and hands
 * the authenticated cipher back to [complete]; only then does the vault key
 * exist in memory.
 */
class VaultManager(
    private val crypto: CryptoManager,
    private val store: VaultStore,
    private val onUnlocked: (vaultKey: ByteArray) -> Unit,
) {
    sealed interface Pending {
        /** First run: authenticate to create and wrap a fresh vault key. */
        data class Setup(val cipher: Cipher) : Pending
        data class Unlock(val cipher: Cipher, val wrapped: ByteArray) : Pending
        /** The Keystore key is gone (screen lock removed) — vault unrecoverable. */
        data object Invalidated : Pending
    }

    fun begin(): Pending = try {
        if (!store.isInitialized() || !crypto.hasWrapKey()) {
            // A vault file without its key (or vice versa) can't be opened.
            store.wipe()
            crypto.deleteWrapKey()
            Pending.Setup(crypto.wrapCipher())
        } else {
            val (iv, wrapped) = store.readWrappedKey()
            Pending.Unlock(crypto.unwrapCipher(iv), wrapped)
        }
    } catch (_: KeyPermanentlyInvalidatedException) {
        Pending.Invalidated
    }

    /** Finish with the cipher returned by a successful BiometricPrompt. */
    fun complete(pending: Pending, authenticated: Cipher) {
        when (pending) {
            is Pending.Setup -> {
                val vaultKey = CryptoManager.newVaultKey()
                val wrapped = authenticated.doFinal(vaultKey)
                store.writeWrappedKey(authenticated.iv, wrapped)
                store.writeAccounts(vaultKey, emptyList())
                onUnlocked(vaultKey)
            }
            is Pending.Unlock -> onUnlocked(authenticated.doFinal(pending.wrapped))
            Pending.Invalidated -> error("invalidated vault cannot be completed")
        }
    }

    /** Start over after [Pending.Invalidated] (the old secrets are lost). */
    fun reset() {
        store.wipe()
        crypto.deleteWrapKey()
    }
}
