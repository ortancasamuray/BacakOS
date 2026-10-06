package org.anadolupanteri.uzakel.network

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.anadolupanteri.uzakel.discovery.PairingStore
import org.anadolupanteri.uzakel.discovery.SavedDevice
import java.net.InetAddress

sealed class ConnState {
    object Connected : ConnState()
    /** Lost contact; trying the last address, then a fresh discovery. */
    object Reconnecting : ConnState()
    /** No answer anywhere; keeps retrying in the background. */
    object Unreachable : ConnState()
    /** The computer answered but no longer knows this pairing. */
    object NeedsPairing : ConnState()
}

/**
 * One live control connection to a paired computer.
 *
 * Sends an encrypted PING every [PING_MS]; the daemon's PONG proves the
 * session is alive on both ends. After [DEAD_MS] without one it re-keys
 * with [NetworkClient.resume] — first at the last known address, then at
 * whatever address a broadcast discovery finds (same computer name first),
 * which covers a daemon restart, the phone's Wi-Fi handoff and the PC's new
 * DHCP lease alike, all without a PIN.
 */
class Connection(
    private val client: NetworkClient,
    private val store: PairingStore,
    val device: SavedDevice,
    initial: PairedSession,
) : AutoCloseable {
    private val _state = MutableStateFlow<ConnState>(ConnState.Connected)
    val state: StateFlow<ConnState> = _state.asStateFlow()

    @Volatile private var lastPong = System.currentTimeMillis()
    val channel = InputChannel(initial, onPong = {
        lastPong = System.currentTimeMillis()
        if (_state.value != ConnState.Connected && _state.value != ConnState.NeedsPairing) _state.value = ConnState.Connected
    })
    val session: PairedSession get() = channel.session

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    init {
        scope.launch { monitor() }
    }

    private suspend fun monitor() {
        while (scope.isActive) {
            channel.ping()
            delay(PING_MS)
            if (_state.value == ConnState.NeedsPairing) return
            if (System.currentTimeMillis() - lastPong > DEAD_MS) reconnect()
        }
    }

    /** Also callable from the UI ("Yeniden dene"). */
    fun retryNow() {
        scope.launch { reconnect() }
    }

    private suspend fun reconnect() {
        _state.value = ConnState.Reconnecting
        var backoff = 2_000L
        while (scope.isActive) {
            when (val r = tryResume()) {
                is ResumeOutcome.Ok -> {
                    channel.updateSession(r.session)
                    store.touch(device.id, r.session.address.hostAddress ?: device.address)
                    lastPong = System.currentTimeMillis()
                    _state.value = ConnState.Connected
                    return
                }
                ResumeOutcome.Rejected -> {
                    store.remove(device.id)
                    _state.value = ConnState.NeedsPairing
                    return
                }
                ResumeOutcome.NoResponse -> {
                    _state.value = ConnState.Unreachable
                    delay(backoff)
                    backoff = (backoff * 2).coerceAtMost(15_000L)
                    if (System.currentTimeMillis() - lastPong <= DEAD_MS) {
                        _state.value = ConnState.Connected // a late PONG arrived
                        return
                    }
                }
            }
        }
    }

    private suspend fun tryResume(): ResumeOutcome {
        val (id, key) = store.credentials(device.id) ?: return ResumeOutcome.Rejected
        val last = runCatching { InetAddress.getByName(channel.session.address.hostAddress) }.getOrNull()
        if (last != null) {
            val r = client.resume(last, id, key)
            if (r !is ResumeOutcome.NoResponse) return r
        }
        // The PC may have a new address: ask the LAN, our computer's name first.
        val hosts = runCatching { client.discoverHosts(timeoutMs = 1500) }.getOrDefault(emptyList())
            .sortedByDescending { it.response.daemonName == device.name }
        for (h in hosts) {
            if (h.address == last) continue
            val r = client.resume(h.address, id, key)
            if (r is ResumeOutcome.Ok) return r
            // A "Rejected" from a *different* computer means nothing for us.
            if (r is ResumeOutcome.Rejected && h.response.daemonName == device.name) return r
        }
        return ResumeOutcome.NoResponse
    }

    override fun close() {
        scope.cancel()
        channel.close()
    }

    companion object {
        const val PING_MS = 2_000L
        const val DEAD_MS = 7_000L

        /** Open a connection to a saved computer without a PIN. */
        suspend fun open(client: NetworkClient, store: PairingStore, device: SavedDevice): Pair<Connection?, ResumeOutcome> {
            val (id, key) = store.credentials(device.id) ?: return null to ResumeOutcome.Rejected
            var outcome = runCatching { InetAddress.getByName(device.address) }.getOrNull()
                ?.let { client.resume(it, id, key) } ?: ResumeOutcome.NoResponse
            if (outcome is ResumeOutcome.NoResponse) {
                val hosts = runCatching { client.discoverHosts(timeoutMs = 1500) }.getOrDefault(emptyList())
                    .sortedByDescending { it.response.daemonName == device.name }
                for (h in hosts) {
                    val r = client.resume(h.address, id, key)
                    if (r is ResumeOutcome.Ok || (r is ResumeOutcome.Rejected && h.response.daemonName == device.name)) {
                        outcome = r
                        break
                    }
                }
            }
            return when (val o = outcome) {
                is ResumeOutcome.Ok -> {
                    store.touch(device.id, o.session.address.hostAddress ?: device.address)
                    Connection(client, store, device, o.session) to o
                }
                ResumeOutcome.Rejected -> {
                    store.remove(device.id)
                    null to o
                }
                ResumeOutcome.NoResponse -> null to o
            }
        }
    }
}

/**
 * The one live [Connection]. Opening a new one closes the previous, and the
 * activity closes it when it finishes — earlier, leaving the control screen
 * with Back left its PING/resume loop running in the process, and each
 * zombie re-keyed the daemon's session for this phone, kicking the visible
 * connection out (seen on real hardware as three phone sockets fighting).
 */
object ActiveConnection {
    private var current: Connection? = null

    @Synchronized
    fun set(connection: Connection) {
        current?.takeIf { it !== connection }?.close()
        current = connection
    }

    @Synchronized
    fun close() {
        current?.close()
        current = null
    }
}
