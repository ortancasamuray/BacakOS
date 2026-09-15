package org.anadolupanteri.uzakel.discovery

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject

/**
 * A previously-paired daemon, remembered so a returning user doesn't need
 * to re-pair every session (see ARCHITECTURE.md §4's `discovery/` bullet).
 * [lastKnownAddress] is re-resolved by a fresh [org.anadolupanteri.uzakel.network.NetworkClient.discoverHosts]
 * scan on each launch rather than trusted blindly — LAN devices commonly
 * change address between sessions (DHCP lease churn).
 */
data class SavedHost(val name: String, val lastKnownAddress: String)

/**
 * Backed by [SharedPreferences] holding one JSON array — this app persists
 * at most a handful of hosts, so a real database (Room, etc.) would be
 * more machinery than the data warrants.
 */
class SavedHostsStore(context: Context) {
    private val prefs: SharedPreferences =
        context.getSharedPreferences("uzakel_saved_hosts", Context.MODE_PRIVATE)

    fun list(): List<SavedHost> {
        val raw = prefs.getString(KEY, null) ?: return emptyList()
        val array = JSONArray(raw)
        return (0 until array.length()).map { i ->
            val obj = array.getJSONObject(i)
            SavedHost(obj.getString("name"), obj.getString("address"))
        }
    }

    fun upsert(host: SavedHost) {
        val existing = list().filterNot { it.name == host.name }
        save(existing + host)
    }

    fun remove(name: String) {
        save(list().filterNot { it.name == name })
    }

    private fun save(hosts: List<SavedHost>) {
        val array = JSONArray()
        for (host in hosts) {
            array.put(JSONObject().put("name", host.name).put("address", host.lastKnownAddress))
        }
        prefs.edit().putString(KEY, array.toString()).apply()
    }

    private companion object {
        const val KEY = "hosts_json"
    }
}
