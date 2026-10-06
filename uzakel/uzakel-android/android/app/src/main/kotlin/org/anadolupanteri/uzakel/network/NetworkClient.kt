package org.anadolupanteri.uzakel.network

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.anadolupanteri.uzakel.crypto.ResumeCrypto
import org.anadolupanteri.uzakel.crypto.UzakelCrypto
import org.anadolupanteri.uzakel.protocol.DiscoverResponse
import org.anadolupanteri.uzakel.protocol.FileMeta
import org.anadolupanteri.uzakel.protocol.Header
import org.anadolupanteri.uzakel.protocol.InputPacket
import org.anadolupanteri.uzakel.protocol.MouseButton
import org.anadolupanteri.uzakel.protocol.Opcode
import org.anadolupanteri.uzakel.protocol.PairResponse
import org.anadolupanteri.uzakel.protocol.ResumeResponse
import org.anadolupanteri.uzakel.protocol.encodePing
import org.anadolupanteri.uzakel.protocol.encodeResumeRequest
import org.anadolupanteri.uzakel.protocol.Protocol
import org.anadolupanteri.uzakel.protocol.decodeEncryptedFramePayload
import org.anadolupanteri.uzakel.protocol.encode
import org.anadolupanteri.uzakel.protocol.encodeChunk
import org.anadolupanteri.uzakel.protocol.encodeDiscoverRequest
import org.anadolupanteri.uzakel.protocol.encodeEncryptedFrame
import org.anadolupanteri.uzakel.protocol.encodePairRequest
import java.io.EOFException
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

/**
 * A completed, confirmed pairing (ARCHITECTURE.md §2.3.1) — everything
 * [InputChannel] and [NetworkClient.sendFile] need to talk to `address`
 * afterward. `txKey` encrypts what this device sends (the daemon's
 * `c2s_key`, from its point of view); `rxKey` decrypts what the daemon
 * sends back (the daemon's `s2c_key`).
 */
data class PairedSession(val address: InetAddress, val txKey: ByteArray, val rxKey: ByteArray)

/** A fresh PIN pairing: the live session plus the long-lived material that
 * lets later connections skip the PIN (see [NetworkClient.resume]). */
class PairResult(val session: PairedSession, val clientId: ByteArray, val resumeKey: ByteArray)

sealed class ResumeOutcome {
    data class Ok(val session: PairedSession) : ResumeOutcome()
    /** The daemon answered but doesn't (or no longer) know this pairing — a
     * new PIN pairing is needed (expired after 30 idle days, or forgotten). */
    object Rejected : ResumeOutcome()
    object NoResponse : ResumeOutcome()
}

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
 *
 * Every outgoing [InputPacket] is encrypted (ARCHITECTURE.md §2.3.1) with
 * `session.txKey` before it's sent, wrapped as an `ENCRYPTED_FRAME` — the
 * daemon has no plaintext fallback on this channel, so an unencrypted
 * packet would just be dropped.
 */
class InputChannel(
    initial: PairedSession,
    private val port: Int = DefaultPorts.INPUT,
    private val onPong: () -> Unit = {},
) : AutoCloseable {
    /** Keys + address in use; swapped atomically by [updateSession] when a
     * resumption re-keys the session or the PC moved to a new address. */
    private class Keys(val session: PairedSession) {
        val cipher = UzakelCrypto.Cipher(session.txKey)
        val opener = UzakelCrypto.Opener(session.rxKey)
    }

    @Volatile private var keys = Keys(initial)
    val session: PairedSession get() = keys.session

    // Deliberately NOT connect()ed: every send() already carries the
    // destination in its DatagramPacket, so connect() buys nothing here —
    // and on real hardware it turned out to actively break things.
    // `DatagramSocket.connect()` threw `IllegalArgumentException: connect: -1`
    // on a real device (MIUI/Android 11, Redmi Note 8) during on-device
    // testing, killing the whole control session the moment it opened.
    private val socket = DatagramSocket().apply { soTimeout = 1000 }
    private val seq = AtomicInteger(1)

    // Every send comes straight from a Compose gesture callback on the main
    // thread, where StrictMode forbids socket I/O (NetworkOnMainThread —
    // found on-device); sends and the PONG listener run on this IO scope.
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    init {
        // PONG listener: the daemon answers PING under the same session, so
        // a decryptable PONG proves both ends still agree on the keys.
        scope.launch {
            val buf = ByteArray(512)
            while (isActive) {
                try {
                    val packet = DatagramPacket(buf, buf.size)
                    socket.receive(packet)
                    val datagram = packet.data.copyOfRange(0, packet.length)
                    val outer = Header.decode(datagram)
                    if (outer.opcode != Opcode.ENCRYPTED_FRAME) continue
                    val (nonce, ct) = decodeEncryptedFramePayload(datagram.copyOfRange(Protocol.HEADER_LEN, datagram.size))
                    val inner = keys.opener.open(nonce, ct) ?: continue
                    if (Header.decode(inner).opcode == Opcode.PONG) onPong()
                } catch (_: java.net.SocketTimeoutException) {
                    // poll slice — loop re-checks isActive
                } catch (_: java.net.SocketException) {
                    break // closed
                } catch (_: Exception) {
                    // stray/garbled datagram
                }
            }
        }
    }

    fun updateSession(session: PairedSession) {
        keys = Keys(session)
    }

    private fun sendInner(innerFrame: ByteArray) {
        val k = keys
        scope.launch {
            try {
                val (nonce, ciphertext) = k.cipher.seal(innerFrame)
                val outer = encodeEncryptedFrame(nonce, ciphertext)
                socket.send(DatagramPacket(outer, outer.size, k.session.address, port))
            } catch (_: Exception) {
                // Best effort: a dropped input packet isn't worth surfacing.
                // Connectivity itself is tracked by PING/PONG (Connection).
            }
        }
    }

    private fun send(packet: InputPacket) = sendInner(packet.encode())

    fun ping() = sendInner(encodePing(System.nanoTime()))

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

    /**
     * Runs the full pairing handshake (ARCHITECTURE.md §2.3.1): generates
     * an ephemeral X25519 keypair, sends it with the PIN, and — if the
     * daemon accepts — verifies its `confirmTag` before trusting the
     * exchange (this is what catches a man-in-the-middle who intercepted
     * the key exchange without knowing the PIN; see
     * `UzakelCrypto`/`daemon/src/crypto.rs`'s doc comments). Returns `null`
     * on a wrong PIN, no response, or a `confirmTag` mismatch — the caller
     * can't distinguish those cases, which is intentional: a mismatched
     * confirm tag and a wrong PIN should look the same to whoever's
     * looking at the UI.
     */
    suspend fun pair(
        host: InetAddress,
        pin: Int,
        port: Int = DefaultPorts.DISCOVERY,
        timeoutMs: Int = 3000,
    ): PairResult? = withContext(Dispatchers.IO) {
        val keypair = UzakelCrypto.EphemeralKeypair.generate()
        val clientPubkey = keypair.publicBytes

        DatagramSocket().use { socket ->
            socket.soTimeout = timeoutMs
            val requestBytes = encodePairRequest(pin, clientPubkey)
            socket.send(DatagramPacket(requestBytes, requestBytes.size, host, port))

            val buf = ByteArray(256)
            val packet = DatagramPacket(buf, buf.size)
            socket.receive(packet)
            val received = packet.data.copyOfRange(0, packet.length)
            if (received.size < Protocol.HEADER_LEN) return@withContext null
            val header = Header.decode(received)
            if (header.opcode != Opcode.PAIR_RESPONSE || received.size < Protocol.HEADER_LEN + header.payloadLen) {
                return@withContext null
            }
            val payload = received.copyOfRange(Protocol.HEADER_LEN, Protocol.HEADER_LEN + header.payloadLen)
            val response = PairResponse.decodePayload(payload)
            if (!response.accepted) return@withContext null

            val material = keypair.derive(response.daemonPubkey, pin, clientPubkey, response.daemonPubkey)
            if (!material.confirmTag.contentEquals(response.confirmTag)) {
                // Either the daemon didn't actually know the PIN (a
                // man-in-the-middle substituted its own keys), or a
                // transport error corrupted the response. Either way, this
                // exchange isn't trustworthy — refuse it rather than using
                // keys we can't verify came from the real daemon.
                return@withContext null
            }

            // From the client's side: c2sKey is what *we* encrypt with (tx),
            // s2cKey is what the daemon encrypts with, so it's our rx.
            PairResult(
                PairedSession(host, txKey = material.c2sKey, rxKey = material.s2cKey),
                clientId = material.clientId,
                resumeKey = material.resumeKey,
            )
        }
    }

    /**
     * PIN-less reconnect (ARCHITECTURE.md §2.3.3): proves possession of the
     * pairing's [resumeKey] with a fresh nonce, checks the daemon's proof
     * in return, and derives brand-new session keys from both nonces.
     */
    suspend fun resume(
        host: InetAddress,
        clientId: ByteArray,
        resumeKey: ByteArray,
        port: Int = DefaultPorts.DISCOVERY,
        timeoutMs: Int = 1500,
    ): ResumeOutcome = withContext(Dispatchers.IO) {
        val clientNonce = ResumeCrypto.newNonce()
        val request = encodeResumeRequest(clientId, clientNonce, ResumeCrypto.requestMac(resumeKey, clientId, clientNonce))
        try {
            DatagramSocket().use { socket ->
                socket.soTimeout = timeoutMs
                socket.send(DatagramPacket(request, request.size, host, port))
                val buf = ByteArray(256)
                val deadline = System.currentTimeMillis() + timeoutMs
                while (System.currentTimeMillis() < deadline) {
                    val packet = DatagramPacket(buf, buf.size)
                    socket.receive(packet)
                    val received = packet.data.copyOfRange(0, packet.length)
                    if (received.size < Protocol.HEADER_LEN) continue
                    val header = Header.decode(received)
                    if (header.opcode != Opcode.RESUME_RESPONSE) continue
                    val resp = ResumeResponse.decodePayload(received.copyOfRange(Protocol.HEADER_LEN, received.size))
                    if (!resp.accepted) return@withContext ResumeOutcome.Rejected
                    // Only the real daemon (holder of resumeKey) can produce this.
                    if (!ResumeCrypto.ctEq(resp.mac, ResumeCrypto.responseMac(resumeKey, clientNonce, resp.daemonNonce))) {
                        return@withContext ResumeOutcome.NoResponse
                    }
                    val (c2s, s2c) = ResumeCrypto.sessionKeys(resumeKey, clientNonce, resp.daemonNonce)
                    return@withContext ResumeOutcome.Ok(PairedSession(host, txKey = c2s, rxKey = s2c))
                }
                ResumeOutcome.NoResponse
            }
        } catch (_: Exception) {
            ResumeOutcome.NoResponse
        }
    }

    /**
     * Sends one file over the three-phase protocol from ARCHITECTURE.md
     * §2.2, with every frame (both directions) wrapped in an
     * `EncryptedFrame` under `session`'s keys per §2.3.1 — there's no
     * plaintext fallback once a session exists. A short wait for an
     * optional `FILE_CORRUPT` from the receiver follows streaming (the
     * daemon side, `file_server.rs`, sends nothing back on success — only
     * on a checksum mismatch — so a read timeout here is treated as
     * success, not a failure).
     *
     * Only this send direction is implemented — matching the daemon, which
     * only implements *receiving* right now (see ARCHITECTURE.md §5).
     */
    suspend fun sendFile(
        session: PairedSession,
        name: String,
        size: Long,
        sha256: ByteArray,
        input: InputStream,
        port: Int = DefaultPorts.FILE,
        onProgress: (sent: Long, total: Long) -> Unit = { _, _ -> },
    ): FileSendResult = withContext(Dispatchers.IO) {
        try {
            Socket().use { socket ->
                socket.connect(InetSocketAddress(session.address, port), 5000)
                val out = socket.getOutputStream()
                val inStream = socket.getInputStream()
                val cipher = UzakelCrypto.Cipher(session.txKey)
                val opener = UzakelCrypto.Opener(session.rxKey)

                fun writeEncrypted(innerFrame: ByteArray) {
                    val (nonce, ciphertext) = cipher.seal(innerFrame)
                    out.write(encodeEncryptedFrame(nonce, ciphertext))
                }

                fun readEncrypted(): Pair<Opcode, ByteArray> {
                    val headerBuf = ByteArray(Protocol.HEADER_LEN)
                    readFully(inStream, headerBuf)
                    val outerHeader = Header.decode(headerBuf)
                    if (outerHeader.opcode != Opcode.ENCRYPTED_FRAME) {
                        throw IllegalStateException("expected ENCRYPTED_FRAME, got ${outerHeader.opcode}")
                    }
                    val outerPayload = ByteArray(outerHeader.payloadLen)
                    readFully(inStream, outerPayload)
                    val (nonce, ciphertext) = decodeEncryptedFramePayload(outerPayload)
                    val innerBytes = opener.open(nonce, ciphertext)
                        ?: throw IllegalStateException("failed to decrypt/authenticate frame from daemon")
                    val innerHeader = Header.decode(innerBytes)
                    val innerPayload = innerBytes.copyOfRange(Protocol.HEADER_LEN, innerBytes.size)
                    if (innerPayload.size != innerHeader.payloadLen) {
                        throw IllegalStateException("inner frame length mismatch after decryption")
                    }
                    return innerHeader.opcode to innerPayload
                }

                // Phase 1: handshake.
                writeEncrypted(FileMeta(name, size, sha256).encode())
                out.flush()

                val (opcode, payload) = readEncrypted()
                when (opcode) {
                    Opcode.FILE_REJECT -> return@withContext FileSendResult.Rejected(String(payload, Charsets.UTF_8))
                    Opcode.FILE_ACCEPT -> { /* proceed to phase 2 */ }
                    else -> return@withContext FileSendResult.Failed(
                        IllegalStateException("unexpected response opcode $opcode after FILE_META"),
                    )
                }

                // Phase 2: streaming, CHUNK_SIZE at a time.
                val chunkBuf = ByteArray(Protocol.CHUNK_SIZE)
                var sent = 0L
                var index = 0
                while (sent < size) {
                    val read = input.read(chunkBuf)
                    if (read <= 0) break
                    writeEncrypted(encodeChunk(index, chunkBuf, 0, read))
                    sent += read
                    index += 1
                    onProgress(sent, size)
                }
                out.flush()

                // Phase 3: verification is the receiver's job; we only find
                // out if it *failed* (FILE_CORRUPT), within a short window.
                socket.soTimeout = 3000
                try {
                    val (trailerOpcode, _) = readEncrypted()
                    if (trailerOpcode == Opcode.FILE_CORRUPT) {
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
        if (read < 0) throw EOFException("connection closed after $offset/${buf.size} bytes")
        offset += read
    }
}
