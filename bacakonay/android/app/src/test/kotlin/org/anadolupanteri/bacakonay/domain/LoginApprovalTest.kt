package org.anadolupanteri.bacakonay.domain

import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.otp.LoginApproval
import org.anadolupanteri.bacakonay.domain.otp.LoginRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.Base64

class LoginApprovalTest {

    private val key = ByteArray(32) { 9 }
    private val qr = "bacakonay://onay?v=1&h=192.168.1.150&p=40123&u=ay%C5%9Fe&s=bacak%20pc&k=" +
        Base64.getUrlEncoder().withoutPadding().encodeToString(key)

    @Test
    fun parsesTheGreeterQr() {
        val r = (LoginApproval.parse(qr) as LoginApproval.Result.Ok).request
        assertEquals(LoginRequest("192.168.1.150", 40123, "ayşe", "bacak pc", key), r)
    }

    @Test
    fun otherQrsAreNotLoginQrsAndBadOnesAreRefused() {
        assertNull(LoginApproval.parse("otpauth://totp/BacakOS:a@b?secret=GEZDGNBVGY3TQOJQ"))
        // Not on the local network: the code must not go to the internet.
        assertTrue(LoginApproval.parse(qr.replace("192.168.1.150", "8.8.8.8")) is LoginApproval.Result.Error)
        assertTrue(LoginApproval.parse(qr.replace("192.168.1.150", "evil.example")) is LoginApproval.Result.Error)
        assertTrue(LoginApproval.parse(qr.replace("v=1", "v=2")) is LoginApproval.Result.Error)
        assertTrue(LoginApproval.parse(qr.substringBefore("&k=")) is LoginApproval.Result.Error)
    }

    /** Byte-for-byte what the greeter's test opens (telefon_onayi.rs). */
    @Test
    fun sealsTheFrameTheGreeterOpens() {
        val req = LoginRequest("10.0.0.2", 1, "ayse", "pc", key)
        val frame = LoginApproval.seal(req, "123456", iv = ByteArray(12) { 7 })
        val hex = frame.joinToString("") { "%02x".format(it) }
        assertEquals("424f310707070707070707070707076889beab5d47294054ebcbbbfb1200e4acd0cd46dbdb", hex)
    }

    @Test
    fun picksTheEnrolledAccountForThisUserAndMachine() {
        fun acc(id: String, issuer: String, name: String) = OtpAccount(id, issuer, name, ByteArray(20))
        val req = LoginRequest("10.0.0.2", 1, "ayse", "pc", key)
        val list = listOf(acc("1", "GitHub", "ayse@pc"), acc("2", "BacakOS", "ayse@lab"), acc("3", "BacakOS", "ayse@pc"))
        assertEquals("3", req.matchAccount(list)?.id)
        // Renamed machine: falls back to the user only when that's unambiguous.
        assertEquals("2", req.matchAccount(list.take(2))?.id)
        assertNull(req.copy(machine = "yeni").matchAccount(list))
    }
}
