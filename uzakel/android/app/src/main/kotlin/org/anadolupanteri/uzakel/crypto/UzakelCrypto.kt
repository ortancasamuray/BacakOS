package org.anadolupanteri.uzakel.crypto

import org.bouncycastle.crypto.agreement.X25519Agreement
import org.bouncycastle.crypto.digests.SHA256Digest
import org.bouncycastle.crypto.generators.HKDFBytesGenerator
import org.bouncycastle.crypto.macs.HMac
import org.bouncycastle.crypto.modes.ChaCha20Poly1305
import org.bouncycastle.crypto.params.AEADParameters
import org.bouncycastle.crypto.params.HKDFParameters
import org.bouncycastle.crypto.params.KeyParameter
import org.bouncycastle.crypto.params.X25519PrivateKeyParameters
import org.bouncycastle.crypto.params.X25519PublicKeyParameters
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.SecureRandom

/**
 * X25519 ECDH + HKDF-SHA256 key derivation + ChaCha20-Poly1305 AEAD for
 * the end-to-end encrypted session pairing establishes
 * (`../../ARCHITECTURE.md` §2.3.1). This is the Kotlin twin of
 * `daemon/src/crypto.rs` — every derivation step here has to match that
 * file byte-for-byte, since the two never share code, only a wire format
 * and a shared derivation scheme. That match was independently verified
 * with a fixed known-answer test vector (fixed private keys, fixed PIN)
 * cross-checked between a Rust binary and a standalone BouncyCastle
 * program before this was wired into the app — see `daemon/examples/kat.rs`.
 *
 * Bouncy Castle is used instead of `javax.crypto`/`KeyPairGenerator`
 * because Android's own X25519 support only arrives on API 33+ (minSdk
 * here is 26), and because BC's lightweight API (`X25519PrivateKeyParameters`,
 * `ChaCha20Poly1305`, …) gives raw primitives with no library-owned
 * key/wire format wrapping them — exactly what byte-for-byte interop with
 * a from-scratch Rust implementation needs.
 *
 * See `daemon/src/crypto.rs`'s doc comment for what this scheme does and
 * doesn't guarantee (real confidentiality against passive eavesdropping;
 * not a full PAKE against an active attacker who already knows the PIN).
 */
object UzakelCrypto {
    const val PUBKEY_LEN = 32
    const val TAG_LEN = 32
    const val NONCE_LEN = 12

    private val random = SecureRandom()

    /** One fresh X25519 keypair for a single pairing attempt. */
    class EphemeralKeypair private constructor(
        private val privateKey: X25519PrivateKeyParameters,
        val publicBytes: ByteArray,
    ) {
        companion object {
            fun generate(): EphemeralKeypair {
                val sk = X25519PrivateKeyParameters(random)
                return EphemeralKeypair(sk, sk.generatePublicKey().encoded)
            }
        }

        /**
         * Completes the ECDH exchange and derives both directional session
         * keys plus the PIN-bound confirmation tag. `clientPubkey`/
         * `daemonPubkey` must be passed in that fixed order regardless of
         * which side calls this — the transcript (and therefore the
         * derived keys) has to match `daemon/src/crypto.rs`'s.
         */
        fun derive(
            theirPublicBytes: ByteArray,
            pin: Int,
            clientPubkey: ByteArray,
            daemonPubkey: ByteArray,
        ): SessionMaterial {
            val theirPublic = X25519PublicKeyParameters(theirPublicBytes, 0)
            val agreement = X25519Agreement()
            agreement.init(privateKey)
            val shared = ByteArray(agreement.agreementSize)
            agreement.calculateAgreement(theirPublic, shared, 0)

            val hkdf = HKDFBytesGenerator(SHA256Digest())
            val prk = hkdf.extractPRK("uzakel-pairing-v1".toByteArray(Charsets.UTF_8), shared)

            val transcript = clientPubkey + daemonPubkey

            val c2sKey = expand(prk, "uzakel c2s".toByteArray(Charsets.UTF_8) + transcript)
            val s2cKey = expand(prk, "uzakel s2c".toByteArray(Charsets.UTF_8) + transcript)
            val pinBytes = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(pin).array()
            val confirmKey = expand(prk, "uzakel confirm".toByteArray(Charsets.UTF_8) + transcript + pinBytes)

            val hmac = HMac(SHA256Digest())
            hmac.init(KeyParameter(confirmKey))
            hmac.update(transcript, 0, transcript.size)
            val confirmTag = ByteArray(hmac.macSize)
            hmac.doFinal(confirmTag, 0)

            return SessionMaterial(c2sKey, s2cKey, confirmTag)
        }

        private fun expand(prk: ByteArray, info: ByteArray): ByteArray {
            val hkdf = HKDFBytesGenerator(SHA256Digest())
            hkdf.init(HKDFParameters.skipExtractParameters(prk, info))
            val out = ByteArray(32)
            hkdf.generateBytes(out, 0, 32)
            return out
        }
    }

    /** Everything derived from one completed pairing's ECDH exchange. */
    data class SessionMaterial(
        val c2sKey: ByteArray,
        val s2cKey: ByteArray,
        val confirmTag: ByteArray,
    )

    /**
     * Encrypts outgoing frames for one direction of one paired session.
     * Nonces are a plain little-endian counter zero-extended to 12 bytes —
     * safe because each [Cipher] is built from a key used for exactly one
     * pairing session's lifetime (a fresh ECDH exchange every time), so the
     * (key, nonce) pair this produces is never reused across sessions.
     */
    class Cipher(key: ByteArray) {
        private val keyParam = KeyParameter(key)
        private var nextCounter: Long = 0

        @Synchronized
        fun seal(plaintext: ByteArray): Pair<ByteArray, ByteArray> {
            val nonce = counterNonce(nextCounter)
            nextCounter++
            val aead = ChaCha20Poly1305()
            aead.init(true, AEADParameters(keyParam, 128, nonce))
            val out = ByteArray(aead.getOutputSize(plaintext.size))
            var len = aead.processBytes(plaintext, 0, plaintext.size, out, 0)
            len += aead.doFinal(out, len)
            return nonce to out.copyOf(len)
        }
    }

    /**
     * Decrypts incoming frames for one direction of one paired session,
     * with replay protection: a nonce counter at or below the highest one
     * already accepted is rejected — the same philosophy
     * `daemon/src/crypto.rs`'s `Opener` uses.
     */
    class Opener(key: ByteArray) {
        private val keyParam = KeyParameter(key)
        private var highestSeen: Long? = null

        /**
         * Returns `null` on any failure: bad tag (wrong key,
         * corrupted/forged ciphertext) or a replayed/stale nonce.
         */
        @Synchronized
        fun open(nonce: ByteArray, ciphertext: ByteArray): ByteArray? {
            val counter = ByteBuffer.wrap(nonce, 0, 8).order(ByteOrder.LITTLE_ENDIAN).long
            val highest = highestSeen
            if (highest != null && counter <= highest) return null

            return try {
                val aead = ChaCha20Poly1305()
                aead.init(false, AEADParameters(keyParam, 128, nonce))
                val out = ByteArray(aead.getOutputSize(ciphertext.size))
                var len = aead.processBytes(ciphertext, 0, ciphertext.size, out, 0)
                len += aead.doFinal(out, len)
                highestSeen = counter
                out.copyOf(len)
            } catch (_: Exception) {
                null
            }
        }
    }

    private fun counterNonce(counter: Long): ByteArray {
        val nonce = ByteArray(NONCE_LEN)
        ByteBuffer.wrap(nonce, 0, 8).order(ByteOrder.LITTLE_ENDIAN).putLong(counter)
        return nonce
    }
}
