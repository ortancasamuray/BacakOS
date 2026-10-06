package org.anadolupanteri.uzakel.crypto

import org.bouncycastle.crypto.digests.SHA256Digest
import org.bouncycastle.crypto.generators.HKDFBytesGenerator
import org.bouncycastle.crypto.macs.HMac
import org.bouncycastle.crypto.params.HKDFParameters
import org.bouncycastle.crypto.params.KeyParameter
import java.security.SecureRandom

/**
 * PIN-less session resumption — the Kotlin twin of `daemon/src/resume.rs`
 * (ARCHITECTURE.md §2.3.3). Both sides keep the `resumeKey` from the first
 * PIN pairing; every (re)connect exchanges two fresh nonces and derives
 * brand-new session keys, so `UzakelCrypto.Cipher`'s counter nonces can
 * safely start at zero again. Verified against the Rust implementation with
 * the fixed vector in `ResumeCryptoTest`.
 */
object ResumeCrypto {
    const val CLIENT_ID_LEN = 16
    const val NONCE_LEN = 32
    const val MAC_LEN = 32

    private val random = SecureRandom()

    fun newNonce(): ByteArray = ByteArray(NONCE_LEN).also(random::nextBytes)

    private fun hmac(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val m = HMac(SHA256Digest())
        m.init(KeyParameter(key))
        for (p in parts) m.update(p, 0, p.size)
        return ByteArray(MAC_LEN).also { m.doFinal(it, 0) }
    }

    fun requestMac(resumeKey: ByteArray, clientId: ByteArray, clientNonce: ByteArray): ByteArray =
        hmac(resumeKey, "uzakel resume req".toByteArray(), clientId, clientNonce)

    fun responseMac(resumeKey: ByteArray, clientNonce: ByteArray, daemonNonce: ByteArray): ByteArray =
        hmac(resumeKey, "uzakel resume resp".toByteArray(), clientNonce, daemonNonce)

    /** `(c2sKey, s2cKey)` — the client encrypts with c2s, decrypts with s2c. */
    fun sessionKeys(resumeKey: ByteArray, clientNonce: ByteArray, daemonNonce: ByteArray): Pair<ByteArray, ByteArray> {
        fun derive(label: String): ByteArray {
            val g = HKDFBytesGenerator(SHA256Digest())
            g.init(HKDFParameters(resumeKey, "uzakel-resume-v1".toByteArray(), label.toByteArray() + clientNonce + daemonNonce))
            return ByteArray(32).also { g.generateBytes(it, 0, 32) }
        }
        return derive("uzakel c2s") to derive("uzakel s2c")
    }

    fun ctEq(a: ByteArray, b: ByteArray): Boolean {
        if (a.size != b.size) return false
        var acc = 0
        for (i in a.indices) acc = acc or (a[i].toInt() xor b[i].toInt())
        return acc == 0
    }
}
