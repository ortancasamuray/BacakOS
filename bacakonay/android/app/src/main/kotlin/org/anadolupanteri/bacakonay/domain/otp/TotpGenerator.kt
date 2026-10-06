package org.anadolupanteri.bacakonay.domain.otp

import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpCode
import org.anadolupanteri.bacakonay.domain.model.OtpType
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/**
 * RFC 4226 (HOTP) / RFC 6238 (TOTP) in plain Kotlin on top of the platform
 * `javax.crypto.Mac` — no third-party OTP library. Must produce exactly what
 * `bacakonay-core` (Rust, PAM side) accepts; both are tested against the
 * RFC appendix vectors.
 */
object TotpGenerator {
    private val POW10 = intArrayOf(1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000)

    /** HOTP value for [counter] as a zero-padded string of [digits] digits. */
    fun hotp(secret: ByteArray, counter: Long, algorithm: OtpAlgorithm, digits: Int): String {
        require(digits in 6..8) { "digits must be 6..8" }
        val mac = Mac.getInstance(algorithm.jcaName)
        mac.init(SecretKeySpec(secret, algorithm.jcaName))
        val msg = ByteArray(8) { i -> (counter ushr (56 - 8 * i)).toByte() }
        val hash = mac.doFinal(msg)
        // Dynamic truncation, RFC 4226 §5.3.
        val offset = hash[hash.size - 1].toInt() and 0x0f
        val binary = ((hash[offset].toInt() and 0x7f) shl 24) or
            ((hash[offset + 1].toInt() and 0xff) shl 16) or
            ((hash[offset + 2].toInt() and 0xff) shl 8) or
            (hash[offset + 3].toInt() and 0xff)
        return (binary % POW10[digits]).toString().padStart(digits, '0')
    }

    fun timeStep(unixSeconds: Long, period: Int): Long = Math.floorDiv(unixSeconds, period.toLong())

    fun totp(secret: ByteArray, unixSeconds: Long, algorithm: OtpAlgorithm, digits: Int, period: Int): String =
        hotp(secret, timeStep(unixSeconds, period), algorithm, digits)

    /** Display-ready code for [account] at [nowMillis]. */
    fun codeFor(account: OtpAccount, nowMillis: Long): OtpCode = when (account.type) {
        OtpType.TOTP -> {
            val seconds = nowMillis / 1000
            val periodMs = account.period * 1000L
            val remainingMs = periodMs - Math.floorMod(nowMillis, periodMs)
            OtpCode(
                account = account,
                code = totp(account.secret, seconds, account.algorithm, account.digits, account.period),
                secondsRemaining = ((remainingMs + 999) / 1000).toInt(),
                fractionRemaining = remainingMs.toFloat() / periodMs,
            )
        }
        OtpType.HOTP -> OtpCode(
            account = account,
            code = hotp(account.secret, account.counter, account.algorithm, account.digits),
            secondsRemaining = null,
            fractionRemaining = null,
        )
    }

    /** "123456" → "123 456", "12345678" → "1234 5678" (easier to read aloud). */
    fun formatForDisplay(code: String): String {
        val half = code.length / 2
        return code.substring(0, half) + " " + code.substring(half)
    }
}
