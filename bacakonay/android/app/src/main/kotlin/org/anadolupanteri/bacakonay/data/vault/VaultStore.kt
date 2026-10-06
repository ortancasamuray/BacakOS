package org.anadolupanteri.bacakonay.data.vault

import androidx.core.util.AtomicFile
import org.anadolupanteri.bacakonay.data.crypto.CryptoManager
import org.anadolupanteri.bacakonay.domain.model.OtpAccount
import org.anadolupanteri.bacakonay.domain.model.OtpAlgorithm
import org.anadolupanteri.bacakonay.domain.model.OtpType
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.Base64

/**
 * On-disk vault in the app's private `filesDir` (excluded from backups):
 *
 * * `vault.key` — the vault key wrapped by the Keystore key:
 *   `[version=1][12-byte IV][ciphertext+tag]`.
 * * `vault.bin` — the account list as JSON, sealed with the vault key
 *   (`CryptoManager.seal`, AAD = [AAD]).
 *
 * Writes go through [AtomicFile], so a crash mid-save never corrupts the
 * previous vault.
 */
class VaultStore(dir: File) {
    private val keyFile = AtomicFile(File(dir, "vault.key"))
    private val dataFile = AtomicFile(File(dir, "vault.bin"))

    fun isInitialized(): Boolean = keyFile.baseFile.exists()

    /** `(iv, wrapped)` of the stored vault key. */
    fun readWrappedKey(): Pair<ByteArray, ByteArray> {
        val raw = keyFile.readFully()
        require(raw.size > 13 && raw[0] == VERSION) { "vault.key bozuk" }
        return raw.copyOfRange(1, 13) to raw.copyOfRange(13, raw.size)
    }

    fun writeWrappedKey(iv: ByteArray, wrapped: ByteArray) {
        require(iv.size == 12)
        write(keyFile, byteArrayOf(VERSION) + iv + wrapped)
    }

    fun readAccounts(vaultKey: ByteArray): List<OtpAccount> {
        if (!dataFile.baseFile.exists()) return emptyList()
        val json = CryptoManager.open(vaultKey, dataFile.readFully(), AAD)
        try {
            return decode(String(json, Charsets.UTF_8))
        } finally {
            json.fill(0)
        }
    }

    fun writeAccounts(vaultKey: ByteArray, accounts: List<OtpAccount>) {
        val json = encode(accounts).toByteArray(Charsets.UTF_8)
        try {
            write(dataFile, CryptoManager.seal(vaultKey, json, AAD))
        } finally {
            json.fill(0)
        }
    }

    /** Forget everything (after the Keystore key was permanently invalidated). */
    fun wipe() {
        keyFile.delete()
        dataFile.delete()
    }

    private fun write(file: AtomicFile, bytes: ByteArray) {
        val out = file.startWrite()
        try {
            out.write(bytes)
            file.finishWrite(out)
        } catch (e: Exception) {
            file.failWrite(out)
            throw e
        }
    }

    companion object {
        private const val VERSION: Byte = 1
        private val AAD = "bacakonay.vault.v1".toByteArray()

        internal fun encode(accounts: List<OtpAccount>): String {
            val arr = JSONArray()
            for (a in accounts) {
                arr.put(
                    JSONObject()
                        .put("id", a.id)
                        .put("issuer", a.issuer)
                        .put("account", a.accountName)
                        .put("secret", Base64.getEncoder().encodeToString(a.secret))
                        .put("type", a.type.name)
                        .put("algorithm", a.algorithm.name)
                        .put("digits", a.digits)
                        .put("period", a.period)
                        .put("counter", a.counter)
                        .put("createdAt", a.createdAt),
                )
            }
            return JSONObject().put("version", 1).put("accounts", arr).toString()
        }

        internal fun decode(json: String): List<OtpAccount> {
            val arr = JSONObject(json).getJSONArray("accounts")
            return List(arr.length()) { i ->
                val o = arr.getJSONObject(i)
                OtpAccount(
                    id = o.getString("id"),
                    issuer = o.optString("issuer"),
                    accountName = o.optString("account"),
                    secret = Base64.getDecoder().decode(o.getString("secret")),
                    type = OtpType.valueOf(o.optString("type", "TOTP")),
                    algorithm = OtpAlgorithm.valueOf(o.optString("algorithm", "SHA1")),
                    digits = o.optInt("digits", 6),
                    period = o.optInt("period", 30),
                    counter = o.optLong("counter", 0),
                    createdAt = o.optLong("createdAt", 0),
                )
            }
        }
    }
}
