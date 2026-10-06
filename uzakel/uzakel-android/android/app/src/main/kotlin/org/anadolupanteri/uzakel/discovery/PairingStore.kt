package org.anadolupanteri.uzakel.discovery

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject
import java.util.Base64

/**
 * A BacakOS computer this phone has paired with. [id] is the hex
 * `clientId` from the pairing (also the daemon's key for it); the resume key
 * itself is only ever stored Keystore-sealed (see [KeystoreBox]).
 */
data class SavedDevice(
    val id: String,
    val name: String,
    val address: String,
    val pairedAt: Long,
    val lastSeen: Long,
)

/**
 * Paired computers, in [SharedPreferences] as one JSON array (a handful of
 * entries; no database needed). Entries not used for [MAX_IDLE_MS]
 * (30 days) are dropped on every read — the daemon forgets them after the
 * same idle time (`daemon/src/pairings.rs`), so keeping them here would
 * only offer a "Bağlan" that can't work.
 */
class PairingStore(context: Context) {
    private val prefs: SharedPreferences = context.getSharedPreferences("uzakel_pairings", Context.MODE_PRIVATE)

    init {
        // Pre-resumption builds saved only (name, IP) — useless without keys.
        context.deleteSharedPreferences("uzakel_saved_hosts")
    }

    private data class Row(val device: SavedDevice, val sealedKey: String)

    private fun rows(): List<Row> {
        val raw = prefs.getString(KEY, null) ?: return emptyList()
        return runCatching {
            val arr = JSONArray(raw)
            (0 until arr.length()).map { i ->
                val o = arr.getJSONObject(i)
                Row(
                    SavedDevice(o.getString("id"), o.getString("name"), o.getString("address"), o.getLong("pairedAt"), o.getLong("lastSeen")),
                    o.getString("rk"),
                )
            }
        }.getOrDefault(emptyList())
    }

    private fun save(rows: List<Row>) {
        val arr = JSONArray()
        for (r in rows) {
            arr.put(
                JSONObject()
                    .put("id", r.device.id).put("name", r.device.name).put("address", r.device.address)
                    .put("pairedAt", r.device.pairedAt).put("lastSeen", r.device.lastSeen).put("rk", r.sealedKey),
            )
        }
        prefs.edit().putString(KEY, arr.toString()).apply()
    }

    /** Most recently used first; prunes idle entries as a side effect. */
    fun list(now: Long = System.currentTimeMillis()): List<SavedDevice> {
        val all = rows()
        val fresh = all.filter { now - it.device.lastSeen < MAX_IDLE_MS }
        if (fresh.size != all.size) save(fresh)
        return fresh.map { it.device }.sortedByDescending { it.lastSeen }
    }

    fun add(name: String, address: String, clientId: ByteArray, resumeKey: ByteArray): SavedDevice {
        val now = System.currentTimeMillis()
        val device = SavedDevice(hex(clientId), name, address, now, now)
        val sealed = Base64.getEncoder().encodeToString(KeystoreBox.seal(resumeKey))
        // Re-pairing the same computer replaces its old entry.
        save(rows().filterNot { it.device.id == device.id || it.device.name == name } + Row(device, sealed))
        return device
    }

    /** `(clientId, resumeKey)`, or `null` if missing/undecryptable. */
    fun credentials(id: String): Pair<ByteArray, ByteArray>? {
        val row = rows().firstOrNull { it.device.id == id } ?: return null
        val key = KeystoreBox.open(Base64.getDecoder().decode(row.sealedKey)) ?: return null
        return unhex(id) to key
    }

    fun touch(id: String, address: String) {
        val now = System.currentTimeMillis()
        save(rows().map { if (it.device.id == id) it.copy(device = it.device.copy(address = address, lastSeen = now)) else it })
    }

    fun remove(id: String) = save(rows().filterNot { it.device.id == id })

    companion object {
        private const val KEY = "devices_json"
        const val MAX_IDLE_MS = 30L * 24 * 60 * 60 * 1000

        fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }
        fun unhex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
    }
}
