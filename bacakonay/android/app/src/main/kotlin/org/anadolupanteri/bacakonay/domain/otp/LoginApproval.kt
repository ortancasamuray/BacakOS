package org.anadolupanteri.bacakonay.domain.otp

import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpType
import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * A login the user approves from the phone ("QR ile giriş"): Turan's greeter
 * shows `bacakonay://onay?v=1&h=<ip>&p=<port>&u=<user>&s=<host>&k=<key>`
 * beside the Bacak Onay code prompt. The app sends the account's current
 * code to `h:p`, sealed with the one-time key `k` (see
 * `turan/crates/bacak-greeter/src/telefon_onayi.rs`). The computer still
 * verifies the code itself; nothing but that one code ever leaves the phone.
 */
data class LoginRequest(
    val host: String,
    val port: Int,
    val user: String,
    val machine: String,
    val key: ByteArray,
) {
    /** The BacakOS account `bacakonay kur` enrolled for this user on this machine. */
    fun matchAccount(accounts: List<OtpAccount>): OtpAccount? {
        val bacak = accounts.filter { it.isBacakOs && it.type == OtpType.TOTP }
        bacak.firstOrNull { it.accountName == "$user@$machine" }?.let { return it }
        // The machine was renamed after enrolling: the user alone, if unambiguous.
        return bacak.filter { it.accountName.substringBefore('@') == user }.singleOrNull()
    }

    override fun equals(other: Any?): Boolean =
        other is LoginRequest && host == other.host && port == other.port && user == other.user &&
            machine == other.machine && key.contentEquals(other.key)

    override fun hashCode(): Int = (host.hashCode() * 31 + port) * 31 + user.hashCode()

    /** Never print key material. */
    override fun toString(): String = "LoginRequest($user@$machine via $host:$port)"
}

object LoginApproval {
    private const val PREFIX = "bacakonay://onay?"
    private val MAGIC = "BO1".toByteArray()

    sealed interface Result {
        data class Ok(val request: LoginRequest) : Result
        data class Error(val reason: String) : Result
    }

    /** `null` when [raw] isn't a login QR at all (it may be an `otpauth://` one). */
    fun parse(raw: String): Result? {
        val text = raw.trim()
        if (!text.startsWith(PREFIX, ignoreCase = true)) return null
        val p = OtpAuthUri.parseQuery(text.substring(PREFIX.length)) ?: return Result.Error("QR çözülemedi")
        if (p["v"] != "1") return Result.Error("Bu giriş QR'ı daha yeni bir sürümden; uygulamayı güncelleyin")
        val host = p["h"].orEmpty()
        // Only a computer on this network: a QR can't send the code anywhere else.
        if (!isLocalIpv4(host)) return Result.Error("QR yerel ağdaki bir bilgisayarı göstermiyor")
        val port = p["p"]?.toIntOrNull()?.takeIf { it in 1..65535 } ?: return Result.Error("QR'da port yok")
        val user = p["u"].orEmpty().ifEmpty { return Result.Error("QR'da kullanıcı yok") }
        val key = runCatching { Base64.getUrlDecoder().decode(p["k"].orEmpty()) }.getOrNull()
            ?.takeIf { it.size == 32 } ?: return Result.Error("QR'daki anahtar geçersiz")
        return Result.Ok(LoginRequest(host, port, user, p["s"].orEmpty(), key))
    }

    /** `"BO1" ‖ iv ‖ AES-256-GCM(key, iv, code, aad = "bacakonay-onay-v1:" ‖ user)`. */
    fun seal(request: LoginRequest, code: String, iv: ByteArray = ByteArray(12).also { SecureRandom().nextBytes(it) }): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(request.key, "AES"), GCMParameterSpec(128, iv))
        cipher.updateAAD("bacakonay-onay-v1:${request.user}".toByteArray(Charsets.UTF_8))
        return MAGIC + iv + cipher.doFinal(code.toByteArray(Charsets.US_ASCII))
    }

    /** RFC 1918 and link-local IPv4 literals only (no names: nothing to resolve). */
    private fun isLocalIpv4(host: String): Boolean {
        val o = host.split('.').map { it.toIntOrNull() ?: return false }
        if (o.size != 4 || o.any { it !in 0..255 }) return false
        return o[0] == 10 || (o[0] == 172 && o[1] in 16..31) || (o[0] == 192 && o[1] == 168) ||
            (o[0] == 169 && o[1] == 254)
    }
}
