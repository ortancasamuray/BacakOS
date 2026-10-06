package org.anadolupanteri.bacakonay.domain

import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpType
import org.anadolupanteri.bacakonay.domain.otp.Base32
import org.anadolupanteri.bacakonay.domain.otp.OtpAuthUri
import org.anadolupanteri.bacakonay.domain.otp.TotpGenerator
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class OtpAuthUriTest {

    /** Byte-for-byte the URI `bacakonay kur` prints (see bacakonay-core uri.rs test). */
    private val fromBacakonayCli =
        "otpauth://totp/BacakOS:ay%C5%9Fe@bacak%20pc?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ" +
            "&issuer=BacakOS&algorithm=SHA1&digits=6&period=30"

    @Test
    fun parsesTheBacakOsEnrollmentQr() {
        val r = OtpAuthUri.parse(fromBacakonayCli) as OtpAuthUri.Result.Ok
        val a = r.account
        assertEquals("BacakOS", a.issuer)
        assertEquals("ayşe@bacak pc", a.accountName)
        assertTrue(a.isBacakOs)
        assertEquals(OtpType.TOTP, a.type)
        assertEquals(OtpAlgorithm.SHA1, a.algorithm)
        assertArrayEquals("12345678901234567890".toByteArray(), a.secret)
        // Same code the PAM module will expect at that instant.
        assertEquals("287082", TotpGenerator.totp(a.secret, 59, a.algorithm, a.digits, a.period))
    }

    @Test
    fun parsesThirdPartyVariants() {
        val gh = OtpAuthUri.parse("otpauth://totp/GitHub:ali?secret=JBSWY3DPEHPK3PXP&issuer=GitHub") as OtpAuthUri.Result.Ok
        assertEquals("GitHub", gh.account.issuer)
        assertEquals("ali", gh.account.accountName)
        val noIssuer = OtpAuthUri.parse("otpauth://totp/ali%40ornek.com?secret=JBSWY3DPEHPK3PXP&digits=8&algorithm=SHA512")
            as OtpAuthUri.Result.Ok
        assertEquals("", noIssuer.account.issuer)
        assertEquals(8, noIssuer.account.digits)
        assertEquals(OtpAlgorithm.SHA512, noIssuer.account.algorithm)
        val hotp = OtpAuthUri.parse("otpauth://hotp/X:y?secret=JBSWY3DPEHPK3PXP&counter=7") as OtpAuthUri.Result.Ok
        assertEquals(7L, hotp.account.counter)
    }

    @Test
    fun rejectsBadInput() {
        listOf(
            "https://example.com",
            "otpauth://totp/x",
            "otpauth://totp/x?secret=!!!",
            "otpauth://totp/x?secret=JBSW", // too short
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=5",
            "otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&algorithm=MD5",
            "otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP", // HOTP without counter
            "otpauth://steam/x?secret=JBSWY3DPEHPK3PXP",
        ).forEach { assertTrue(it, OtpAuthUri.parse(it) is OtpAuthUri.Result.Error) }
    }

    @Test
    fun base32Rfc4648() {
        mapOf("" to "", "f" to "MY", "fo" to "MZXQ", "foo" to "MZXW6", "foob" to "MZXW6YQ", "fooba" to "MZXW6YTB", "foobar" to "MZXW6YTBOI")
            .forEach { (plain, enc) ->
                assertEquals(enc, Base32.encode(plain.toByteArray()))
                assertArrayEquals(plain.toByteArray(), Base32.decode(enc))
            }
        assertArrayEquals("foobar".toByteArray(), Base32.decode("mzxw 6ytb-oi======"))
        assertEquals(null, Base32.decode("MZXW1"))
    }
}
