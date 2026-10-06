package org.anadolupanteri.bacakonay.ui.codes

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpCode
import org.anadolupanteri.bacakonay.domain.otp.TotpGenerator
import org.anadolupanteri.bacakonay.domain.repository.AccountRepository

class CodesViewModel(private val repository: AccountRepository) : ViewModel() {

    /** Ticks on every wall-clock second boundary, so codes flip exactly on time. */
    private val secondTicker = flow {
        while (true) {
            val now = System.currentTimeMillis()
            emit(now)
            delay(1000 - now % 1000)
        }
    }

    /** BacakOS logins first, then alphabetical. */
    val codes: StateFlow<List<OtpCode>> =
        combine(repository.accounts, secondTicker) { accounts, now ->
            accounts
                .sortedWith(compareByDescending<OtpAccount> { it.isBacakOs }.thenBy { it.issuer.lowercase() }.thenBy { it.accountName.lowercase() })
                .map { TotpGenerator.codeFor(it, now) }
        }
            .flowOn(Dispatchers.Default)
            .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    fun add(account: OtpAccount) = viewModelScope.launch { repository.add(account) }

    fun remove(id: String) = viewModelScope.launch { repository.remove(id) }

    fun nextHotp(id: String) = viewModelScope.launch { repository.incrementCounter(id) }
}
