package org.anadolupanteri.uzakel.protocol

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest

/**
 * Kotlin mirror of `uzakel/daemon/src/protocol.rs`. Every layout here MUST
 * match that file byte-for-byte — see its module doc for why this is
 * hand-rolled little-endian encoding rather than a serialization library:
 * the wire format is the contract between two different languages, and an
 * explicit, symmetric encode/decode pair is easier to keep in lockstep
 * across them than two independent derive macros would be.
 */
object Protocol {
    const val MAGIC: Int = 0x557A // "Uz"
    const val VERSION: Int = 1
    const val HEADER_LEN: Int = 8
    const val CHUNK_SIZE: Int = 64 * 1024
}

enum class Opcode(val value: Int) {
    MOUSE_MOVE(1),
    MOUSE_CLICK(2),
    MOUSE_SCROLL(3),
    KEY_PRESS(4),

    FILE_META(10),
    FILE_ACCEPT(11),
    FILE_REJECT(12),
    CHUNK(13),
    TRANSFER_CANCEL(14),
    FILE_CORRUPT(15),

    DISCOVER_REQUEST(20),
    DISCOVER_RESPONSE(21),
    PAIR_REQUEST(22),
    PAIR_RESPONSE(23);

    companion object {
        fun fromByte(b: Int): Opcode? = values().find { it.value == b }
    }
}

class ProtocolException(message: String) : Exception(message)

data class Header(val opcode: Opcode, val payloadLen: Int) {
    fun encodeInto(out: ByteArrayOutputStream) {
        val buf = ByteBuffer.allocate(Protocol.HEADER_LEN).order(ByteOrder.LITTLE_ENDIAN)
        buf.putShort(Protocol.MAGIC.toShort())
        buf.put(Protocol.VERSION.toByte())
        buf.put(opcode.value.toByte())
        buf.putInt(payloadLen)
        out.write(buf.array())
    }

    companion object {
        /** `buf` must contain at least [Protocol.HEADER_LEN] bytes starting at offset 0. */
        fun decode(buf: ByteArray): Header {
            if (buf.size < Protocol.HEADER_LEN) {
                throw ProtocolException("packet too short: need ${Protocol.HEADER_LEN} bytes, got ${buf.size}")
            }
            val bb = ByteBuffer.wrap(buf, 0, Protocol.HEADER_LEN).order(ByteOrder.LITTLE_ENDIAN)
            val magic = bb.short.toInt() and 0xFFFF
            if (magic != Protocol.MAGIC) {
                throw ProtocolException("bad magic: expected ${Protocol.MAGIC.toString(16)}, got ${magic.toString(16)}")
            }
            val version = bb.get().toInt() and 0xFF
            if (version != Protocol.VERSION) {
                throw ProtocolException("unsupported protocol version: $version (client speaks ${Protocol.VERSION})")
            }
            val opcodeByte = bb.get().toInt() and 0xFF
            val opcode = Opcode.fromByte(opcodeByte) ?: throw ProtocolException("unknown opcode byte: $opcodeByte")
            val payloadLen = bb.int
            return Header(opcode, payloadLen)
        }
    }
}

/** Builds a full frame (header + payload) ready to send. */
fun frame(opcode: Opcode, payload: ByteArray): ByteArray {
    val out = ByteArrayOutputStream(Protocol.HEADER_LEN + payload.size)
    Header(opcode, payload.size).encodeInto(out)
    out.write(payload)
    return out.toByteArray()
}

// ── Input channel (UDP) ─────────────────────────────────────────────────────

enum class MouseButton(val value: Int) {
    LEFT(0), RIGHT(1), MIDDLE(2);

    companion object {
        fun fromByte(b: Int): MouseButton? = values().find { it.value == b }
    }
}

/** Bitmask matching `KEY_PRESS`'s `modifiers` byte in protocol.rs. */
object Modifiers {
    const val SHIFT = 0b0001
    const val CTRL = 0b0010
    const val ALT = 0b0100
    const val SUPER = 0b1000
}

sealed class InputPacket {
    abstract val seq: Int

    data class MouseMove(override val seq: Int, val dx: Int, val dy: Int) : InputPacket()
    data class MouseScroll(override val seq: Int, val dx: Int, val dy: Int) : InputPacket()
    data class MouseClick(override val seq: Int, val button: MouseButton, val pressed: Boolean) : InputPacket()
    data class KeyPress(
        override val seq: Int,
        val keycode: Int,
        val modifiers: Int,
        val pressed: Boolean,
    ) : InputPacket()
}

/**
 * Encodes an [InputPacket] to a ready-to-send UDP datagram. `dx`/`dy` are
 * clamped to the wire format's `i16` range — callers applying their own
 * sensitivity/acceleration curve (see `input/TrackpadView.kt`) must do so
 * before calling this, not rely on it to scale anything down.
 */
fun InputPacket.encode(): ByteArray {
    fun i16(v: Int): Int = v.coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())

    return when (this) {
        is InputPacket.MouseMove -> {
            val bb = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN)
            bb.putInt(seq); bb.putShort(i16(dx).toShort()); bb.putShort(i16(dy).toShort())
            frame(Opcode.MOUSE_MOVE, bb.array())
        }
        is InputPacket.MouseScroll -> {
            val bb = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN)
            bb.putInt(seq); bb.putShort(i16(dx).toShort()); bb.putShort(i16(dy).toShort())
            frame(Opcode.MOUSE_SCROLL, bb.array())
        }
        is InputPacket.MouseClick -> {
            val bb = ByteBuffer.allocate(6).order(ByteOrder.LITTLE_ENDIAN)
            bb.putInt(seq); bb.put(button.value.toByte()); bb.put((if (pressed) 1 else 0).toByte())
            frame(Opcode.MOUSE_CLICK, bb.array())
        }
        is InputPacket.KeyPress -> {
            val bb = ByteBuffer.allocate(8).order(ByteOrder.LITTLE_ENDIAN)
            bb.putInt(seq); bb.putShort(keycode.toShort()); bb.put(modifiers.toByte())
            bb.put((if (pressed) 1 else 0).toByte())
            frame(Opcode.KEY_PRESS, bb.array())
        }
    }
}

// ── File transfer channel (TCP) ─────────────────────────────────────────────

data class FileMeta(val name: String, val size: Long, val sha256: ByteArray) {
    fun encode(): ByteArray {
        val nameBytes = name.toByteArray(Charsets.UTF_8)
        val bb = ByteBuffer.allocate(2 + nameBytes.size + 8 + 32).order(ByteOrder.LITTLE_ENDIAN)
        bb.putShort(nameBytes.size.toShort())
        bb.put(nameBytes)
        bb.putLong(size)
        bb.put(sha256)
        return frame(Opcode.FILE_META, bb.array())
    }

    override fun equals(other: Any?): Boolean =
        other is FileMeta && name == other.name && size == other.size && sha256.contentEquals(other.sha256)

    override fun hashCode(): Int = name.hashCode() * 31 + size.hashCode()
}

fun encodeFileReject(reason: String): ByteArray = frame(Opcode.FILE_REJECT, reason.toByteArray(Charsets.UTF_8))

fun encodeSimple(opcode: Opcode): ByteArray = frame(opcode, ByteArray(0))

/** One `CHUNK` frame: `index: u32, len: u32` followed by `len` bytes of data. */
fun encodeChunk(index: Int, data: ByteArray, offset: Int = 0, length: Int = data.size - offset): ByteArray {
    val bb = ByteBuffer.allocate(8 + length).order(ByteOrder.LITTLE_ENDIAN)
    bb.putInt(index)
    bb.putInt(length)
    bb.put(data, offset, length)
    return frame(Opcode.CHUNK, bb.array())
}

fun sha256(bytes: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(bytes)

// ── Discovery / pairing (UDP broadcast) ─────────────────────────────────────

data class DiscoverResponse(
    val daemonName: String,
    val daemonVersion: Triple<Int, Int, Int>,
    val acceptingNewPairs: Boolean,
) {
    companion object {
        fun decodePayload(payload: ByteArray): DiscoverResponse {
            val bb = ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN)
            val nameLen = bb.short.toInt() and 0xFFFF
            val nameBytes = ByteArray(nameLen)
            bb.get(nameBytes)
            val major = bb.get().toInt() and 0xFF
            val minor = bb.get().toInt() and 0xFF
            val patch = bb.get().toInt() and 0xFF
            val accepting = bb.get().toInt() != 0
            return DiscoverResponse(String(nameBytes, Charsets.UTF_8), Triple(major, minor, patch), accepting)
        }
    }
}

fun encodeDiscoverRequest(): ByteArray = encodeSimple(Opcode.DISCOVER_REQUEST)

fun encodePairRequest(pin: Int): ByteArray {
    val bb = ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN)
    bb.putInt(pin)
    return frame(Opcode.PAIR_REQUEST, bb.array())
}

data class PairResponse(val accepted: Boolean) {
    companion object {
        fun decodePayload(payload: ByteArray): PairResponse {
            if (payload.isEmpty()) throw ProtocolException("empty PairResponse payload")
            return PairResponse(payload[0].toInt() != 0)
        }
    }
}
