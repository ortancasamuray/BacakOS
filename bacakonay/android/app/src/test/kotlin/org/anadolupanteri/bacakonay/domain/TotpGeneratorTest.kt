package org.anadolupanteri.bacakonay.domain

import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.otp.TotpGenerator
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** RFC appendix vectors — the same ones bacakonay-core (Rust) is tested with. */
class TotpGeneratorTest {
    private val seed20 = "12345678901234567890".toByteArray()
    private val seed32 = "12345678901234567890123456789012".toByteArray()
    private val seed64 = "1234567890123456789012345678901234567890123456789012345678901234".toByteArray()

    @Test
    fun rfc4226AppendixD() {
        val expected = listOf("755224", "287082", "359152", "969429", "338314", "254676", "287922", "162583", "399871", "520489")
        expected.forEachIndexed { i, want ->
            assertEquals(want, TotpGenerator.hotp(seed20, i.toLong(), OtpAlgorithm.SHA1, 6))
        }
    }

    @Test
    fun rfc6238AppendixB() {
        val cases = listOf(
            Triple(59L, "94287082", listOf("46119246", "90693936")),
            Triple(1111111109L, "07081804", listOf("68084774", "25091201")),
            Triple(1111111111L, "14050471", listOf("67062674", "99943326")),
            Triple(1234567890L, "89005924", listOf("91819424", "93441116")),
            Triple(2000000000L, "69279037", listOf("90698825", "38618901")),
            Triple(20000000000L, "65353130", listOf("77737706", "47863826")),
        )
        for ((t, sha1, others) in cases) {
            assertEquals(sha1, TotpGenerator.totp(seed20, t, OtpAlgorithm.SHA1, 8, 30))
            assertEquals(others[0], TotpGenerator.totp(seed32, t, OtpAlgorithm.SHA256, 8, 30))
            assertEquals(others[1], TotpGenerator.totp(seed64, t, OtpAlgorithm.SHA512, 8, 30))
        }
    }

    @Test
    fun codeForReportsRemainingTime() {
        val a = OtpAccount(id = "x", issuer = "BacakOS", accountName = "a", secret = seed20)
        val c = TotpGenerator.codeFor(a, nowMillis = 59_000L)
        assertEquals("287082", c.code) // step 1, 6 digits (RFC 4226 counter 1)
        assertEquals(1, c.secondsRemaining)
        val h = TotpGenerator.codeFor(a.copy(type = org.anadolupanteri.bacakonay.domain.model.OtpType.HOTP, counter = 9), 0)
        assertEquals("520489", h.code)
        assertNull(h.secondsRemaining)
    }

    @Test
    fun displayGrouping() {
        assertEquals("123 456", TotpGenerator.formatForDisplay("123456"))
        assertEquals("1234 5678", TotpGenerator.formatForDisplay("12345678"))
    }
}
