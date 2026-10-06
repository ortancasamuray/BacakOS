package org.anadolupanteri.bacakonay.domain.otp

/**
 * RFC 4648 base32 — the encoding of `secret=` in `otpauth://` URIs.
 * Decoding is lenient like other authenticators: case-insensitive, spaces,
 * dashes and `=` padding ignored.
 */
object Base32 {
    private const val ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

    fun encode(data: ByteArray): String {
        val out = StringBuilder((data.size * 8 + 4) / 5)
        var buffer = 0L
        var bits = 0
        for (b in data) {
            buffer = (buffer shl 8) or (b.toLong() and 0xff)
            bits += 8
            while (bits >= 5) {
                bits -= 5
                out.append(ALPHABET[((buffer shr bits) and 0x1f).toInt()])
            }
        }
        if (bits > 0) out.append(ALPHABET[((buffer shl (5 - bits)) and 0x1f).toInt()])
        return out.toString()
    }

    /** @return the bytes, or `null` on a character outside the alphabet. */
    fun decode(input: String): ByteArray? {
        val out = java.io.ByteArrayOutputStream(input.length * 5 / 8)
        var buffer = 0L
        var bits = 0
        for (c in input) {
            val v = when (c) {
                in 'A'..'Z' -> c - 'A'
                in 'a'..'z' -> c - 'a'
                in '2'..'7' -> c - '2' + 26
                ' ', '-', '=' -> continue
                else -> return null
            }
            buffer = (buffer shl 5) or v.toLong()
            bits += 5
            if (bits >= 8) {
                bits -= 8
                out.write(((buffer shr bits) and 0xff).toInt())
            }
        }
        return out.toByteArray()
    }
}
