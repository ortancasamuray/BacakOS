package org.anadolupanteri.uzakel.crypto

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Known-answer vectors shared with `daemon/src/resume.rs` and
 * `daemon/examples/kat.rs` (and cross-checked against an independent Python
 * computation) — the Kotlin and Rust implementations never share code, so
 * these fixed values are what proves they interoperate.
 */
class ResumeCryptoTest {
    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
    private fun fill(v: Int, n: Int) = ByteArray(n) { v.toByte() }

    @Test
    fun resumeVectorMatchesRust() {
        val rk = fill(7, 32)
        val cid = fill(1, 16)
        val cn = fill(2, 32)
        val dn = fill(3, 32)
        assertEquals("323314182b3dc9d2b4a6ba85d2189ce8242a0f862c8f485457927f5b8fb1b69f", hex(ResumeCrypto.requestMac(rk, cid, cn)))
        assertEquals("12ec08c387dc31ea00a45de2d91e159b6345b2ff21e5c381463cebff60ba22a8", hex(ResumeCrypto.responseMac(rk, cn, dn)))
        val (c2s, s2c) = ResumeCrypto.sessionKeys(rk, cn, dn)
        assertEquals("95782a01c12187161a6900e9c73c819d5f537cae01c5b1d383970a0daf9b3d3a", hex(c2s))
        assertEquals("25fac1a4c0086dd74b86039bd7b86e5a350a80f6c04651af52f12905983d5604", hex(s2c))
    }

    /** Same fixed keys as daemon/examples/kat.rs (client [1;32], daemon [2;32], PIN 123456). */
    @Test
    fun pairingDerivesSameResumeMaterialAsRust() {
        val client = UzakelCrypto.EphemeralKeypair.forTest(fill(1, 32))
        val daemon = UzakelCrypto.EphemeralKeypair.forTest(fill(2, 32))
        assertEquals("a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209", hex(client.publicBytes))
        val m = client.derive(daemon.publicBytes, 123456, client.publicBytes, daemon.publicBytes)
        assertEquals("295559ddcb2d3470054af4ca1f37253d6257ecedb5a5cc8f7216935133d207bc", hex(m.c2sKey))
        assertEquals("018f5c552140156c63b1d3413400bf1aea066d236ae0380815f5948a7fd9e158", hex(m.resumeKey))
        assertEquals("350309096f43b1f12539f35d3460ae98", hex(m.clientId))
    }

    @Test
    fun resumedKeysEncryptAcrossDirections() {
        val rk = fill(9, 32)
        val cn = ResumeCrypto.newNonce()
        val dn = ResumeCrypto.newNonce()
        val (c2s, s2c) = ResumeCrypto.sessionKeys(rk, cn, dn)
        val (nonce, ct) = UzakelCrypto.Cipher(c2s).seal("merhaba".toByteArray())
        assertEquals("merhaba", String(UzakelCrypto.Opener(c2s).open(nonce, ct)!!))
        assertTrue(UzakelCrypto.Opener(s2c).open(nonce, ct) == null)
        assertFalse(ResumeCrypto.ctEq(c2s, s2c))
    }
}
