package org.anadolupanteri.bacakonay

import android.app.Application
import org.anadolupanteri.bacakonay.data.crypto.CryptoManager
import org.anadolupanteri.bacakonay.data.repository.VaultAccountRepository
import org.anadolupanteri.bacakonay.data.vault.VaultManager
import org.anadolupanteri.bacakonay.data.vault.VaultStore

/** Hand-wired dependency container — the graph is four objects, no DI framework needed. */
class AppContainer(app: Application) {
    private val store = VaultStore(app.filesDir)
    val repository = VaultAccountRepository(store)
    val vault = VaultManager(CryptoManager(), store, repository::onUnlocked)
}

class BacakOnayApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
    }
}
