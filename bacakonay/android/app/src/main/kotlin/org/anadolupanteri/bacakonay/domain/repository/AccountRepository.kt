package org.anadolupanteri.bacakonay.domain.repository

import kotlinx.coroutines.flow.StateFlow
import org.anadolupanteri.bacakonay.domain.model.OtpAccount

/**
 * The domain's view of saved accounts. The implementation keeps them in an
 * encrypted vault that only opens after a biometric/device-credential check;
 * while locked, [accounts] is empty and mutations fail.
 */
interface AccountRepository {
    val accounts: StateFlow<List<OtpAccount>>
    val isUnlocked: StateFlow<Boolean>

    suspend fun add(account: OtpAccount)
    suspend fun remove(id: String)
    /** HOTP: advance the counter (shows the next code). */
    suspend fun incrementCounter(id: String)
    /** Drop decrypted secrets from memory. */
    fun lock()
}
