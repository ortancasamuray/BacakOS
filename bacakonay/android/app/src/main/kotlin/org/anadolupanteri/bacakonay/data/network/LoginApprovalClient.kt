package org.anadolupanteri.bacakonay.data.network

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.anadolupanteri.bacakonay.domain.otp.LoginRequest
import java.net.InetSocketAddress
import java.net.Socket

/**
 * Delivers a sealed code to the greeter that showed the login QR. The only
 * network traffic the app makes: one short TCP exchange with the computer on
 * the local network the user just scanned.
 */
object LoginApprovalClient {
    private const val TIMEOUT_MS = 5_000

    sealed interface Outcome {
        data object Delivered : Outcome
        data class Failed(val reason: String) : Outcome
    }

    suspend fun send(request: LoginRequest, frame: ByteArray): Outcome = withContext(Dispatchers.IO) {
        try {
            Socket().use { s ->
                s.connect(InetSocketAddress(request.host, request.port), TIMEOUT_MS)
                s.soTimeout = TIMEOUT_MS
                s.getOutputStream().apply { write(frame); flush() }
                s.shutdownOutput()
                when (s.getInputStream().bufferedReader().readLine()) {
                    "ALINDI" -> Outcome.Delivered
                    else -> Outcome.Failed("Bilgisayar onayı reddetti. Giriş ekranındaki QR'ı yeniden okutun.")
                }
            }
        } catch (e: java.io.IOException) {
            Outcome.Failed("Bilgisayara ulaşılamadı. Telefonla bilgisayar aynı Wi-Fi ağında mı?")
        }
    }
}
