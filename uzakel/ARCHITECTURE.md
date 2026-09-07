# Uzakel — Architecture

> **Status: design phase — nothing here is implemented yet.** This document
> is the target design the first implementation should follow; treat every
> "will"/"is" below as intent, not a description of existing code.

Two components, three network channels, one shared wire protocol.

---

## 1. System overview

```
┌─────────────────────────────┐                    ┌───────────────────────────────┐
│   Android client (Kotlin)   │                    │  BacakOS daemon (Rust)         │
│                             │                     │                               │
│  ┌───────────┐  UDP :9876   │   discovery/input   │  ┌────────────┐               │
│  │ Trackpad/  │─────────────┼─────────────────────▶│ input_manager│──▶ /dev/uinput │
│  │ Keyboard   │             │  (unreliable, fast)  │  └────────────┘               │
│  └───────────┘             │                      │                               │
│  ┌───────────┐  TCP :9877   │   file transfer       │  ┌────────────┐               │
│  │ File       │◀────────────┼──────────────────────▶│ file_server │──▶ ~/İndirilenler│
│  │ Transfer   │             │  (reliable, chunked) │  └────────────┘               │
│  └───────────┘             │                      │                               │
│  ┌───────────┐  mDNS/UDP    │   discovery           │  ┌────────────┐               │
│  │ Discovery  │◀────────────┼──────────────────────▶│ discovery   │               │
│  └───────────┘  bcast :5353 │                      │  └────────────┘               │
└─────────────────────────────┘                    └───────────────────────────────┘
```

Three independent channels, each suited to its traffic:

| Channel | Transport | Why |
|---|---|---|
| Discovery | mDNS, falling back to UDP broadcast on port 5353 | Zero-config LAN pairing; a lost broadcast just means the next periodic one succeeds. |
| Input (mouse/keyboard) | UDP, custom port (default 9876) | Latency matters more than reliability — a dropped `MOUSE_MOVE` delta is imperceptible; a retransmitted stale one would feel laggy. |
| File transfer | TCP, custom port (default 9877) | Correctness matters more than latency — files must arrive byte-exact. |

---

## 2. Wire protocol

All packets share a fixed binary header, little-endian:

```
┌─────────┬─────────┬──────────────┬─────────────────┐
│ magic   │ version │ opcode       │ payload_len      │
│ u16     │ u8      │ u8           │ u32              │
├─────────┴─────────┴──────────────┴─────────────────┤
│ payload (payload_len bytes, opcode-specific layout)  │
└──────────────────────────────────────────────────────┘
```

`magic` is a fixed constant so a stray packet on the port (or a version
mismatch) is rejected before touching `/dev/uinput`. `version` lets the
daemon and client detect a protocol skew and refuse to pair rather than
silently misinterpret payloads.

### 2.1 Input channel opcodes (UDP)

| Opcode | Payload | Meaning |
|---|---|---|
| `MOUSE_MOVE` | `dx: i16, dy: i16` | Relative pointer delta, already scaled by the client's sensitivity/acceleration curve. |
| `MOUSE_CLICK` | `button: u8, state: u8` | `button`: left/right/middle; `state`: down/up. |
| `MOUSE_SCROLL` | `dx: i16, dy: i16` | Relative scroll delta (two-finger drag on the trackpad view). |
| `KEY_PRESS` | `keycode: u16, modifiers: u8, state: u8` | `modifiers` is a bitmask (Shift/Ctrl/Alt/Super); `state` is down/up so held keys and repeats are the client's responsibility, not the wire's. |

Every input packet also carries a monotonic `seq: u32` in its payload
prefix so `input_manager.rs` can drop an out-of-order UDP packet instead of
momentarily moving the pointer backwards.

### 2.2 File transfer channel (TCP)

A transfer is one TCP connection, one file, three phases:

1. **Handshake** — `FILE_META { name, size: u64, sha256: [u8; 32] }`,
   acknowledged by `FILE_ACCEPT` or `FILE_REJECT { reason }` (disk full,
   name collision policy, user declined).
2. **Streaming** — the file body as consecutive 64 KiB chunks, each preceded
   by `CHUNK { index: u32, len: u32 }`; the receiver can send
   `TRANSFER_CANCEL` at any point, which both sides treat as an immediate
   close.
3. **Verification** — receiver computes SHA-256 over the reassembled file
   and compares against the handshake's hash; mismatch reports
   `FILE_CORRUPT` and the receiver deletes the partial file rather than
   keeping a silently-truncated one.

Direction is symmetric — the same state machine runs whichever side is
sending (Android → BacakOS or BacakOS → Android); only who initiates the
TCP connection differs.

### 2.3 Pairing

First contact between a client and a daemon goes through a PIN handshake
before either side accepts input or file packets from the other: the
daemon displays a short PIN (desktop notification), the client sends it
back over the discovery response channel, and the daemon marks that
client's certificate/key as trusted for future TLS-wrapped sessions. This
is what stands between "any phone on the LAN" and "a phone the user
actually approved."

---

## 3. Daemon (Rust) — module design

```
daemon/src/
├── main.rs            # arg parsing, systemd notify, wires the three services together
├── discovery.rs        # mDNS/UDP broadcast responder; answers with daemon version + pairing state
├── protocol.rs          # packet header/opcode definitions shared by input_manager and file_server
├── input_manager.rs     # UDP socket → parses input opcodes → replays via /dev/uinput
└── file_server.rs       # TCP listener → chunked receive/send, SHA-256, writes into ~/İndirilenler
```

- **`input_manager.rs`** owns a virtual mouse and keyboard device created
  through `/dev/uinput` (via the `input-linux` or `evdev` crate). It applies
  an acceleration curve to raw deltas before injecting them, and clamps
  synthesized pointer motion to the compositor's known screen bounds so a
  burst of packets can't fling the cursor off-screen. Runs on a `tokio` task
  reading the UDP socket in a tight loop — no channel/queueing layer between
  socket and injection, since added latency there defeats the point of using
  UDP in the first place.
- **`file_server.rs`** is a plain `tokio` TCP listener; each accepted
  connection gets its own task running the three-phase state machine from
  §2.2. On successful verification it fires a desktop notification (via
  `libnotify`/the freedesktop notification bus — the same mechanism
  `bacak-compositor`'s `plugins/*` already use elsewhere in BacakOS) naming
  the received file.
- **`discovery.rs`** answers broadcast/mDNS queries with the daemon's
  version and current pairing state (accepting new pairs / locked to
  already-paired clients only), and is the channel the PIN handshake (§2.3)
  rides on before the UDP/TCP channels are opened.
- Runs as a `systemd --user` service (see README) so it starts with the
  user's session and never needs root — it opens `/dev/uinput` via the
  `uinput` group membership BacakOS's session setup grants the user, the
  same pattern `bacak/packaging/bacak-session` uses for other user-scope
  desktop services.

---

## 4. Android client (Kotlin) — module design

```
android/app/src/main/kotlin/org/anadolupanteri/uzakel/
├── discovery/    # host scan + a persisted list of previously-paired hosts
├── network/      # NetworkClient: coroutine/Flow-based UDP input socket + TCP file socket
├── input/        # TrackpadView: raw touch deltas → sensitivity/acceleration → UDP packets
├── transfer/     # FileTransferManager: SAF file picking, chunked upload/download, progress Flow
└── ui/           # Compose screens: trackpad, virtual keyboard, device list, transfer panel
```

- **`input/TrackpadView`** is a Compose `pointerInput` surface translating
  raw touch events into the same `dx`/`dy` deltas `input_manager.rs` expects
  — one-finger drag = `MOUSE_MOVE`, one-finger tap = `MOUSE_CLICK` (left),
  two-finger tap = `MOUSE_CLICK` (right), two-finger drag = `MOUSE_SCROLL`.
  Sensitivity and acceleration are applied client-side before the packet is
  sent, so the daemon never has to guess the phone's touch resolution.
- **`network/NetworkClient`** owns both sockets behind a coroutine/`Flow`
  API: input packets are fire-and-forget sends (no ack expected, matching
  §2.1's UDP design), while file transfer exposes a `Flow<TransferProgress>`
  the transfer panel collects to drive its progress bar.
- **`transfer/FileTransferManager`** uses Android's Storage Access Framework
  for both picking a file to send and choosing a destination for a
  download, so it never needs broad storage permissions — matching the
  "no shell-out, sandboxed by design" instinct the rest of BacakOS's file
  handling (`altay`'s `security::Sandbox`) follows on the desktop side.
- **`discovery/`** persists paired hosts (name, last-known IP, trust key
  from the PIN handshake) so a returning user doesn't need to re-pair every
  session, and re-resolves the current IP via mDNS/broadcast on each launch
  since LAN devices commonly change address between sessions (DHCP lease
  churn).

---

## 5. Open questions / not yet decided

- Exact crate choice for `/dev/uinput` access (`input-linux` vs. hand-rolled
  ioctl bindings) — needs a spike against a real BacakOS session running
  `bacak-compositor` under `udev`/DRM.
- TLS material for the post-pairing sessions: self-signed cert pinned at
  pairing time (simplest, no CA involved) vs. a lighter PSK scheme keyed off
  the PIN exchange.
- Whether `MOUSE_SCROLL` needs its own acceleration curve distinct from
  `MOUSE_MOVE`'s, once real trackpad use surfaces a preference.
- Multi-client behavior: can two phones control the same daemon
  simultaneously, or does pairing a new client evict the previous one?
