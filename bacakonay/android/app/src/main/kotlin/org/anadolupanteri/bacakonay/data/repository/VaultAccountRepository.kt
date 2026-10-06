package org.anadolupanteri.bacakonay.data.repository

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import org.anadolupanteri.bacakonay.data.vault.VaultStore
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.repository.AccountRepository

/** [AccountRepository] over the encrypted [VaultStore]. */
class VaultAccountRepository(private val store: VaultStore) : AccountRepository {

    private val _accounts = MutableStateFlow<List<OtpAccount>>(emptyList())
    override val accounts: StateFlow<List<OtpAccount>> = _accounts.asStateFlow()

    private val _unlocked = MutableStateFlow(false)
    override val isUnlocked: StateFlow<Boolean> = _unlocked.asStateFlow()

    private val mutex = Mutex()
    @Volatile private var vaultKey: ByteArray? = null

    /** Called by `VaultManager` once the user authenticated. */
    fun onUnlocked(key: ByteArray) {
        vaultKey = key
        _accounts.value = store.readAccounts(key)
        _unlocked.value = true
    }

    override suspend fun add(account: OtpAccount) = mutate { list ->
        // Re-scanning the same QR replaces the entry instead of duplicating it.
        list.filterNot { it.issuer == account.issuer && it.accountName == account.accountName } + account
    }

    override suspend fun remove(id: String) = mutate { list -> list.filterNot { it.id == id } }

    override suspend fun incrementCounter(id: String) = mutate { list ->
        list.map { if (it.id == id) it.copy(counter = it.counter + 1) else it }
    }

    override fun lock() {
        vaultKey?.fill(0)
        vaultKey = null
        _accounts.value.forEach { it.secret.fill(0) }
        _accounts.value = emptyList()
        _unlocked.value = false
    }

    private suspend fun mutate(change: (List<OtpAccount>) -> List<OtpAccount>) = mutex.withLock {
        val key = vaultKey ?: throw IllegalStateException("Kasa kilitli")
        val updated = change(_accounts.value)
        withContext(Dispatchers.IO) { store.writeAccounts(key, updated) }
        _accounts.value = updated
    }
}
