# Uzakel — Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

> **Status: `daemon/` (Rust) is implemented and matches this document;
> `android/` doesn't exist yet.** Where this doc describes daemon behavior,
> it's describing real code in `daemon/src/`. Where it describes the Android
> client, it's still a plan.

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
│  ┌───────────┐  UDP bcast   │   discovery           │  ┌────────────┐               │
│  │ Discovery  │◀────────────┼──────────────────────▶│ discovery   │               │
│  └───────────┘  :45922      │                      │  └────────────┘               │
└─────────────────────────────┘                    └───────────────────────────────┘
```

Three independent channels, each suited to its traffic:

| Channel | Transport | Why |
|---|---|---|
| Discovery + pairing | UDP broadcast, port 45922 (`UZAKEL_DISCOVERY_PORT`) | Zero-config LAN pairing; a lost broadcast just means the next periodic one succeeds. **Not real mDNS** — see the note below. |
| Input (mouse/keyboard) | UDP, port 9876 (`UZAKEL_INPUT_PORT`) | Latency matters more than reliability — a dropped `MOUSE_MOVE` delta is imperceptible; a retransmitted stale one would feel laggy. |
| File transfer | TCP, port 9877 (`UZAKEL_FILE_PORT`) | Correctness matters more than latency — files must arrive byte-exact. |

**On the discovery port:** the original plan here was mDNS on the standard
5353 port. The implementation is a plain UDP broadcast responder instead, on
a dedicated port (45922) — `avahi-daemon` already owns 5353 on most desktop
Linux systems, and a hand-rolled responder sharing that port with genuine
mDNS/DNS-SD traffic would risk confusing (or being confused by) it. Real
mDNS support, if wanted later, is still open — see §5.

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

Direction is meant to be symmetric — the same state machine on whichever
side is sending — but **only the receive side (Android → BacakOS) is
implemented in `file_server.rs` today**; a daemon-initiated send
(BacakOS → Android) isn't wired up yet (§5). `TRANSFER_CANCEL` in the
current code is read by the receiver in place of the next `CHUNK`, so
today it's the sender that aborts a transfer by sending it instead of
more data — not something the receiver sends back.

### 2.3 Pairing

`discovery.rs` generates a fresh 6-digit PIN at startup (and again after
every successful pairing, so a captured PIN can't be replayed) and shows it
via a desktop notification. A client sends `PAIR_REQUEST { pin }` on the
discovery UDP socket; the daemon compares it and replies
`PAIR_RESPONSE { accepted }`.

**This is not yet a security boundary.** A correct PIN today only proves
the daemon logged a match — nothing ties that approval to the input (UDP)
or file-transfer (TCP) sockets, which currently accept packets from *any*
sender on the LAN, paired or not, and there's no TLS/cert material
exchanged at all. Wiring a trust store through to those two channels and
picking one of the TLS approaches in §5 is the actual security work still
open here; treat the current PIN check as a UX nicety ("here's the PIN the
phone should show you"), not a guarantee.

---

## 3. Daemon (Rust) — module design

```
daemon/src/
├── main.rs            # env-based config, binds all three sockets, opens /dev/uinput, spawns the three tasks
├── discovery.rs        # UDP broadcast responder; answers DISCOVER_REQUEST + PIN pairing (§2.3)
├── protocol.rs          # packet header/opcode definitions + encode/decode, shared by every module
├── input_manager.rs     # UDP socket → parses input opcodes → replays via /dev/uinput
└── file_server.rs       # TCP listener → chunked receive, SHA-256 verify, writes into ~/İndirilenler
```

- **`input_manager.rs`** owns a virtual mouse and keyboard device created
  through `/dev/uinput` via the `input-linux` crate (registering every valid
  evdev keycode up front, plus the three mouse buttons and the X/Y/wheel
  relative axes). It applies a mild acceleration curve to raw deltas before
  injecting them — on top of whatever curve the client already applied, as a
  safety net for a client that sends raw deltas — but does **not** clamp
  pointer position itself: every event is `REL_X`/`REL_Y` (relative), so
  screen-edge clamping is entirely the compositor's job, same as for a real
  mouse. Runs on a `tokio` task reading the UDP socket in a tight loop, and
  tracks a per-source-address last-seen `seq` so an out-of-order or
  duplicate UDP packet is dropped instead of moving the pointer backwards.
- **`file_server.rs`** is a plain `tokio` TCP listener; each accepted
  connection gets its own task running the receive side of the state
  machine from §2.2, picking a collision-safe destination filename
  (`name`, then `name (2)`, `name (3)`, …) the same way `altay`'s transfer
  module does on the desktop side. On successful verification it shells out
  to `notify-send` (a deliberate scope cut — see §5 — rather than speaking
  `org.freedesktop.Notifications` over D-Bus directly).
- **`discovery.rs`** answers `DISCOVER_REQUEST` with the daemon's name and
  version, and runs the PIN pairing handshake from §2.3 — both on the same
  UDP broadcast socket.
- Intended to run as a `systemd --user` service (see README) so it starts
  with the user's session and never needs root — it opens `/dev/uinput` via
  `uinput` group membership, which BacakOS's session setup would need to
  grant the user the same way `bacak/packaging/bacak-session` does for other
  user-scope desktop services. The actual systemd unit file isn't written
  yet (§5).

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

- **Pairing isn't enforced yet.** The input and file-transfer sockets accept
  from any LAN sender regardless of PIN pairing state — see the caveat in
  §2.3. This is the biggest gap between "what's built" and "what would be
  safe to expose beyond a trusted home LAN."
- TLS material for the post-pairing sessions: self-signed cert pinned at
  pairing time (simplest, no CA involved) vs. a lighter PSK scheme keyed off
  the PIN exchange — and then actually gating the UDP/TCP sockets on it.
- Daemon-initiated file sends (BacakOS → Android) — `file_server.rs` only
  implements the receive side right now.
- Real mDNS/DNS-SD instead of the plain UDP broadcast responder on 45922
  (would need an mDNS crate or hand-rolled multicast DNS records).
- A `systemd --user` unit file + packaging (`.deb`) — the daemon runs fine
  from the shell today but isn't installed as a service anywhere yet.
- Whether `MOUSE_SCROLL` needs its own acceleration curve distinct from
  `MOUSE_MOVE`'s, once real trackpad use surfaces a preference.
- Multi-client behavior: can two phones control the same daemon
  simultaneously, or does pairing a new client evict the previous one? Right
  now every paired-or-not client is accepted equally, so this doesn't yet
  apply in practice.
- Replacing the `notify-send` shell-out in `discovery.rs`/`file_server.rs`
  with a native `org.freedesktop.Notifications` D-Bus call (e.g. via
  `zbus`), removing the runtime dependency on `libnotify-bin`.
