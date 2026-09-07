package org.anadolupanteri.uzakel.network

import java.net.URLDecoder

/** What the BacakOS panel's pairing QR code (§2.3.1's "QR pairing" note in
 * ARCHITECTURE.md) encodes — everything [DeviceListScreen]'s manual-entry
 * form asks the user for, minus having to type it. */
data class ScannedPairingInfo(val host: String, val port: Int, val pin: Int, val name: String?)

private const val URI_PREFIX = "uzakel://pair?"

/**
 * Parses a `uzakel://pair?host=...&port=...&pin=...&name=...` URI — the
 * exact shape `bacak-compositor`'s Uzakel panel encodes into its QR (see
 * `state.rs`'s `open_uzakel_panel`). `port` defaults to the standard
 * discovery port when absent (kept optional so a shorter QR is possible
 * later without breaking older scanners). Returns `null` for anything that
 * isn't this app's own scheme — including a stray QR code from something
 * else entirely, which this must not misinterpret as a pairing attempt.
 */
fun parsePairingUri(raw: String): ScannedPairingInfo? {
    if (!raw.startsWith(URI_PREFIX)) return null
    val query = raw.substring(URI_PREFIX.length)
    val params = query.split("&").mapNotNull { part ->
        val eq = part.indexOf('=')
        if (eq < 0) null else part.substring(0, eq) to decode(part.substring(eq + 1))
    }.toMap()

    val host = params["host"]?.takeIf { it.isNotBlank() } ?: return null
    val pin = params["pin"]?.toIntOrNull() ?: return null
    val port = params["port"]?.toIntOrNull() ?: DefaultPorts.DISCOVERY
    return ScannedPairingInfo(host, port, pin, params["name"])
}

private fun decode(s: String): String = runCatching { URLDecoder.decode(s, "UTF-8") }.getOrDefault(s)
