package org.anadolupanteri.bacakonay.domain.otp

import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpType
import java.util.UUID

/**
 * Parser for the Key URI Format (`otpauth://totp/Issuer:account?secret=…`),
 * which is exactly what `bacakonay kur` prints as a QR on BacakOS — and what
 * every other service's 2FA QR uses, so Bacak Onay works for those too.
 *
 * Written against plain strings (not `android.net.Uri`) so it's unit-tested
 * on the JVM.
 */
object OtpAuthUri {

    sealed interface Result {
        data class Ok(val account: OtpAccount) : Result
        data class Error(val reason: String) : Result
    }

    fun parse(raw: String, nowMillis: Long = System.currentTimeMillis()): Result {
        val text = raw.trim()
        if (!text.startsWith("otpauth://", ignoreCase = true)) return Result.Error("Bu bir otpauth:// kodu değil")
        val rest = text.substring("otpauth://".length)
        val slash = rest.indexOf('/')
        if (slash < 0) return Result.Error("Eksik etiket")
        val type = when (rest.substring(0, slash).lowercase()) {
            "totp" -> OtpType.TOTP
            "hotp" -> OtpType.HOTP
            else -> return Result.Error("Desteklenmeyen tür")
        }
        val afterType = rest.substring(slash + 1)
        val q = afterType.indexOf('?')
        val label = percentDecode(if (q < 0) afterType else afterType.substring(0, q))
            ?: return Result.Error("Etiket çözülemedi")
        val params = parseQuery(if (q < 0) "" else afterType.substring(q + 1))
            ?: return Result.Error("Parametreler çözülemedi")

        val secret = params["secret"]?.let(Base32::decode)
        if (secret == null || secret.size < 10) return Result.Error("Gizli anahtar eksik ya da geçersiz")

        val labelIssuer = label.substringBefore(':', "").trim()
        val accountName = label.substringAfter(':').trim()
        val issuer = params["issuer"]?.trim().takeUnless { it.isNullOrEmpty() } ?: labelIssuer

        val algorithm = params["algorithm"]?.let { OtpAlgorithm.fromUriName(it) ?: return Result.Error("Bilinmeyen algoritma") }
            ?: OtpAlgorithm.SHA1
        val digits = params["digits"]?.toIntOrNull() ?: 6
        if (digits !in 6..8) return Result.Error("Hane sayısı 6–8 olmalı")
        val period = params["period"]?.toIntOrNull() ?: 30
        if (period !in 15..300) return Result.Error("Süre 15–300 sn olmalı")
        val counter = params["counter"]?.toLongOrNull() ?: 0L
        if (type == OtpType.HOTP && params["counter"] == null) return Result.Error("HOTP için counter gerekli")

        return Result.Ok(
            OtpAccount(
                id = UUID.randomUUID().toString(),
                issuer = issuer,
                accountName = accountName.ifEmpty { issuer },
                secret = secret,
                type = type,
                algorithm = algorithm,
                digits = digits,
                period = period,
                counter = counter,
                createdAt = nowMillis,
            ),
        )
    }

    private fun parseQuery(query: String): Map<String, String>? {
        val out = HashMap<String, String>()
        if (query.isEmpty()) return out
        for (pair in query.split('&')) {
            if (pair.isEmpty()) continue
            val key = percentDecode(pair.substringBefore('='))?.lowercase() ?: return null
            val value = percentDecode(pair.substringAfter('=', "")) ?: return null
            out.putIfAbsent(key, value)
        }
        return out
    }

    /** RFC 3986 percent-decoding (UTF-8). `+` stays a literal plus. */
    private fun percentDecode(s: String): String? {
        val bytes = java.io.ByteArrayOutputStream(s.length)
        var i = 0
        while (i < s.length) {
            val c = s[i]
            if (c == '%') {
                if (i + 2 >= s.length) return null
                val v = s.substring(i + 1, i + 3).toIntOrNull(16) ?: return null
                bytes.write(v)
                i += 3
            } else {
                bytes.write(c.toString().toByteArray(Charsets.UTF_8))
                i++
            }
        }
        return bytes.toString(Charsets.UTF_8.name())
    }
}
