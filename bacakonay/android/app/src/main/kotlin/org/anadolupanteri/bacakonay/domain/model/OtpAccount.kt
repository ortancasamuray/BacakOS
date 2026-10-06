package org.anadolupanteri.bacakonay.domain.model

/** HMAC hash behind the OTP (RFC 6238 §1.2 allows SHA-1/256/512). */
enum class OtpAlgorithm(val jcaName: String, val uriName: String) {
    SHA1("HmacSHA1", "SHA1"),
    SHA256("HmacSHA256", "SHA256"),
    SHA512("HmacSHA512", "SHA512");

    companion object {
        fun fromUriName(name: String): OtpAlgorithm? =
            entries.firstOrNull { it.uriName.equals(name.replace("-", ""), ignoreCase = true) }
    }
}

enum class OtpType { TOTP, HOTP }

/**
 * One saved authenticator entry. [secret] is the raw key bytes — it only ever
 * exists decrypted in memory while the vault is unlocked (see
 * `data.vault.VaultStore`).
 */
data class OtpAccount(
    val id: String,
    val issuer: String,
    val accountName: String,
    val secret: ByteArray,
    val type: OtpType = OtpType.TOTP,
    val algorithm: OtpAlgorithm = OtpAlgorithm.SHA1,
    val digits: Int = 6,
    val period: Int = 30,
    /** HOTP moving factor; unused for TOTP. */
    val counter: Long = 0,
    val createdAt: Long = 0,
) {
    /** Entries enrolled by `bacakonay kur` on a BacakOS machine. */
    val isBacakOs: Boolean get() = issuer == BACAKOS_ISSUER

    // ByteArray has identity equality; compare contents so StateFlow
    // de-duplication and tests behave.
    override fun equals(other: Any?): Boolean =
        other is OtpAccount && id == other.id && issuer == other.issuer &&
            accountName == other.accountName && secret.contentEquals(other.secret) &&
            type == other.type && algorithm == other.algorithm && digits == other.digits &&
            period == other.period && counter == other.counter && createdAt == other.createdAt

    override fun hashCode(): Int = id.hashCode() * 31 + counter.hashCode()

    /** Never print key material. */
    override fun toString(): String = "OtpAccount(id=$id, issuer=$issuer, account=$accountName, $type)"

    companion object {
        const val BACAKOS_ISSUER = "BacakOS"
    }
}

/** A code ready for display, recomputed by the UI ticker. */
data class OtpCode(
    val account: OtpAccount,
    val code: String,
    /** Seconds left in the current TOTP period (null for HOTP). */
    val secondsRemaining: Int?,
    /** 1.0 → just issued, 0.0 → about to expire (null for HOTP). */
    val fractionRemaining: Float?,
)
