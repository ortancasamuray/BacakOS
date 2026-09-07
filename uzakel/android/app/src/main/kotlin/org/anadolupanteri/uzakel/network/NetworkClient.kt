package org.anadolupanteri.uzakel.network

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.anadolupanteri.uzakel.protocol.DiscoverResponse
import org.anadolupanteri.uzakel.protocol.FileMeta
import org.anadolupanteri.uzakel.protocol.Header
import org.anadolupanteri.uzakel.protocol.InputPacket
import org.anadolupanteri.uzakel.protocol.MouseButton
import org.anadolupanteri.uzakel.protocol.Opcode
import org.anadolupanteri.uzakel.protocol.PairResponse
import org.anadolupanteri.uzakel.protocol.Protocol
import org.anadolupanteri.uzakel.protocol.encode
import org.anadolupanteri.uzakel.protocol.encodeChunk
import org.anadolupanteri.uzakel.protocol.encodeDiscoverRequest
import org.anadolupanteri.uzakel.protocol.encodePairRequest
import java.io.InputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicInteger

/** Matches `uzakel-daemon`'s defaults — see `../../daemon/src/main.rs`. */
object DefaultPorts {
    const val DISCOVERY = 45922
    const val INPUT = 9876
    const val FILE = 9877
}

data class DiscoveredHost(val response: DiscoverResponse, val address: InetAddress)

sealed class FileSendResult {
    object Success : FileSendResult()
    data class Rejected(val reason: String) : FileSendResult()
    object Corrupt : FileSendResult()
    data class Failed(val cause: Throwable) : FileSendResult()
}

/**
 * One fire-and-forget UDP socket for the input channel (ARCHITECTURE.md
 * §2.1) — every send is best-effort, matching the daemon's expectation that
 * a dropped packet is fine but a stale/out-of-order one should be
 * recognizable, hence the monotonic [seq] counter this owns.
 */
class InputChannel(private val host: InetAddress, private val port: Int = DefaultPorts.INPUT) : AutoCloseable {
    // Deliberately NOT connect()ed: every send() already carries the
    // destination in its DatagramPacket, so connect() buys nothing here —
    // and on real hardware it turned out to actively break things.
    // `DatagramSocket.connect()` threw `IllegalArgumentException: connect: -1`
    // on a real device (MIUI/Android 11, Redmi Note 8) during on-device
    // testing, killing the whole control session the moment it opened.
    // A plain unconnected socket sidesteps whatever OS/network-stack
    // quirk caused that and matches how discoverHosts()/pair() already
    // talk UDP elsewhere in this file.
    private val socket = DatagramSocket()
    private val seq = AtomicInteger(1)

    // Every mouseMove/mouseClick/etc. call comes straight from a Compose
    // gesture callback on the main thread — but `DatagramSocket.send()` is
    // still a real syscall, and Android's StrictMode ThreadPolicy blocks it
    // there with a `NetworkOnMainThreadException`. On-device testing showed
    // every single send failing silently this way (the exception was being
    // swallowed by design — see below) with 0 packets ever reaching the
    // daemon despite gesture detection working perfectly. A dedicated
    // background scope moves the actual socket write off the caller's
    // thread without turning every call site into a suspend function.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private fun send(packet: InputPacket) {
        scope.launch {
            val bytes = packet.encode()
            try {
                socket.send(DatagramPacket(bytes, bytes.size, host, port))
            } catch (_: Exception) {
                // Best-effort by design (see class doc) — a send failure here
                // (e.g. transient "network unreachable" on Wi-Fi handoff) is not
                // worth surfacing to the UI, let alone crashing, over one
                // dropped input packet.
            }
        }
    }

    fun mouseMove(dx: Int, dy: Int) = send(InputPacket.MouseMove(seq.getAndIncrement(), dx, dy))
    fun mouseScroll(dx: Int, dy: Int) = send(InputPacket.MouseScroll(seq.getAndIncrement(), dx, dy))
    fun mouseClick(button: MouseButton, pressed: Boolean) =
        send(InputPacket.MouseClick(seq.getAndIncrement(), button, pressed))
    fun keyPress(keycode: Int, modifiers: Int, pressed: Boolean) =
        send(InputPacket.KeyPress(seq.getAndIncrement(), keycode, modifiers, pressed))

    override fun close() {
        scope.cancel()
        socket.close()
    }
}

class NetworkClient {

    /**
     * Broadcasts `DISCOVER_REQUEST` and collects every `DISCOVER_RESPONSE`
     * that arrives within [timeoutMs]. Not real mDNS (see
     * `../../ARCHITECTURE.md`'s note on port 45922) — a plain broadcast, so
     * this only finds hosts on the same subnet.
     */
    suspend fun discoverHosts(timeoutMs: Long = 2000, port: Int = DefaultPorts.DISCOVERY): List<DiscoveredHost> =
        withContext(Dispatchers.IO) {
            val results = mutableListOf<DiscoveredHost>()
            DatagramSocket().use { socket ->
                socket.broadcast = true
                socket.soTimeout = 250 // poll in short slices so the overall timeoutMs is honored precisely

                val requestBytes = encodeDiscoverRequest()
                val broadcastAddr = InetAddress.getByName("255.255.255.255")
                socket.send(DatagramPacket(requestBytes, requestBytes.size, broadcastAddr, port))

                val deadline = System.currentTimeMillis() + timeoutMs
                val buf = ByteArray(512)
                while (System.currentTimeMillis() < deadline) {
                    try {
                        val packet = DatagramPacket(buf, buf.size)
                        socket.receive(packet)
                        // `buf` is reused across iterations, so bound every read by
                        // packet.length (this datagram's actual size), never buf.size
                        // — otherwise a short packet after a longer one reads stale
                        // bytes left over from the previous receive().
                        val received = packet.data.copyOfRange(0, packet.length)
                        if (received.size < Protocol.HEADER_LEN) continue
                        val header = Header.decode(received)
                        if (header.opcode == Opcode.DISCOVER_RESPONSE &&
                            received.size >= Protocol.HEADER_LEN + header.payloadLen
                        ) {
                            val payload = received.copyOfRange(Protocol.HEADER_LEN, Protocol.HEADER_LEN + header.payloadLen)
                            val response = DiscoverResponse.decodePayload(payload)
                            results.add(DiscoveredHost(response, packet.address))
                        }
                    } catch (_: java.net.SocketTimeoutException) {
                        // expected — just means no datagram in this polling slice
                    } catch (_: Exception) {
                        // malformed packet from something else on the LAN — ignore and keep listening
                    }
                }
            }
            results.distinctBy { it.address }
        }

    /** Sends `PAIR_REQUEST { pin }` and waits for one `PAIR_RESPONSE`. */
    suspend fun pair(
        host: InetAddress,
        pin: Int,
        port: Int = DefaultPorts.DISCOVERY,
        timeoutMs: Int = 3000,
    ): Boolean = withContext(Dispatchers.IO) {
        DatagramSocket().use { socket ->
            socket.soTimeout = timeoutMs
            val requestBytes = encodePairRequest(pin)
            socket.send(DatagramPacket(requestBytes, requestBytes.size, host, port))

            val buf = ByteArray(64)
            val packet = DatagramPacket(buf, buf.size)
            socket.receive(packet)
            val received = packet.data.copyOfRange(0, packet.length)
            if (received.size < Protocol.HEADER_LEN) return@withContext false
            val header = Header.decode(received)
            if (header.opcode != Opcode.PAIR_RESPONSE || received.size < Protocol.HEADER_LEN + header.payloadLen) {
                return@withContext false
            }
            val payload = received.copyOfRange(Protocol.HEADER_LEN, Protocol.HEADER_LEN + header.payloadLen)
            PairResponse.decodePayload(payload).accepted
        }
    }

    /**
     * Sends one file over the three-phase protocol from ARCHITECTURE.md
     * §2.2: handshake, chunked streaming, then a short wait for an optional
     * `FILE_CORRUPT` from the receiver (the daemon side, `file_server.rs`,
     * sends nothing back on success — only on a checksum mismatch — so a
     * read timeout here is treated as success, not a failure).
     *
     * Only this send direction is implemented — matching the daemon, which
     * only implements *receiving* right now (see ARCHITECTURE.md §5).
     */
    suspend fun sendFile(
        host: InetAddress,
        name: String,
        size: Long,
        sha256: ByteArray,
        input: InputStream,
        port: Int = DefaultPorts.FILE,
        onProgress: (sent: Long, total: Long) -> Unit = { _, _ -> },
    ): FileSendResult = withContext(Dispatchers.IO) {
        try {
            Socket().use { socket ->
                socket.connect(InetSocketAddress(host, port), 5000)
                val out = socket.getOutputStream()
                val inStream = socket.getInputStream()

                // Phase 1: handshake.
                out.write(FileMeta(name, size, sha256).encode())
                out.flush()

                val headerBuf = ByteArray(Protocol.HEADER_LEN)
                readFully(inStream, headerBuf)
                val header = Header.decode(headerBuf)
                when (header.opcode) {
                    Opcode.FILE_REJECT -> {
                        val reasonBytes = ByteArray(header.payloadLen)
                        readFully(inStream, reasonBytes)
                        return@withContext FileSendResult.Rejected(String(reasonBytes, Charsets.UTF_8))
                    }
                    Opcode.FILE_ACCEPT -> { /* proceed to phase 2 */ }
                    else -> return@withContext FileSendResult.Failed(
                        IllegalStateException("unexpected response opcode ${header.opcode} after FILE_META"),
                    )
                }

                // Phase 2: streaming, CHUNK_SIZE at a time.
                val chunkBuf = ByteArray(Protocol.CHUNK_SIZE)
                var sent = 0L
                var index = 0
                while (sent < size) {
                    val read = input.read(chunkBuf)
                    if (read <= 0) break
                    out.write(encodeChunk(index, chunkBuf, 0, read))
                    sent += read
                    index += 1
                    onProgress(sent, size)
                }
                out.flush()

                // Phase 3: verification is the receiver's job; we only find
                // out if it *failed* (FILE_CORRUPT), within a short window.
                socket.soTimeout = 3000
                try {
                    val trailerHeader = ByteArray(Protocol.HEADER_LEN)
                    readFully(inStream, trailerHeader)
                    val trailer = Header.decode(trailerHeader)
                    if (trailer.opcode == Opcode.FILE_CORRUPT) {
                        return@withContext FileSendResult.Corrupt
                    }
                } catch (_: java.net.SocketTimeoutException) {
                    // No response within the window — the daemon only speaks
                    // up on failure, so silence here means success.
                }
                FileSendResult.Success
            }
        } catch (e: Exception) {
            FileSendResult.Failed(e)
        }
    }
}

private fun readFully(stream: InputStream, buf: ByteArray) {
    var offset = 0
    while (offset < buf.size) {
        val read = stream.read(buf, offset, buf.size - offset)
        if (read < 0) throw java.io.EOFException("connection closed after $offset/${buf.size} bytes")
        offset += read
    }
}
