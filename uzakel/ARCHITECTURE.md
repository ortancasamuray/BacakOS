# Uzakel — Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

> **Status: both `daemon/` (Rust) and `android/` (Kotlin) have a first pass
> implemented and match this document.** The Android build was verified end
> to end with a real Gradle + Android SDK (`assembleDebug` produces a debug
> APK, `lintDebug` passes) — see README.md. What's *not* verified yet is the
> two sides actually talking to each other on real hardware, and pairing
> still isn't enforced on the input/file sockets (§2.3, §5).

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

## 4. Android client (Kotlin + Jetpack Compose) — module design

```
android/app/src/main/kotlin/org/anadolupanteri/uzakel/
├── protocol/      # Protocol.kt — byte-for-byte Kotlin mirror of daemon/src/protocol.rs
├── discovery/     # SavedHostsStore: SharedPreferences-backed list of previously-paired hosts
├── network/       # NetworkClient: discovery scan, PIN pairing, InputChannel (UDP), sendFile (TCP)
├── input/         # TrackpadView (multi-touch), KeyCodes (evdev keycode table), IME→typeChar bridge
├── transfer/      # FileTransferManager: SAF file picking, SHA-256, progress via StateFlow
├── ui/            # UzakelApp (root), DeviceListScreen, ControlScreen, TransferScreen
└── MainActivity.kt
```

There's no navigation library — `UzakelApp` switches between three screens
with a small `sealed class Screen` and a `when`, which is simpler than
pulling in Navigation-Compose for a graph this shallow (device list →
control → transfer, and back).

- **`protocol/Protocol.kt`** is the actual interop contract with the daemon
  — every `ByteBuffer` layout in it (`Header`, `InputPacket.encode()`,
  `FileMeta.encode()`, `encodeChunk`, `DiscoverResponse.decodePayload`, …)
  has to match `daemon/src/protocol.rs` byte-for-byte, since these two
  files never share code, only a wire format. `NetworkClient` bounds every
  UDP read by the datagram's actual `packet.length`, not the receive
  buffer's capacity, before touching it — the buffer is reused across
  `receive()` calls, so a short packet arriving after a longer one would
  otherwise read stale bytes.
- **`input/TrackpadView`** is a Compose `pointerInput`/`awaitEachGesture`
  surface (not `detectDragGestures`, which only tracks one pointer) —
  one-finger drag = `MOUSE_MOVE`, one-finger tap = left click, two-finger
  tap = right click, two-finger drag = `MOUSE_SCROLL`. Deltas are averaged
  across active pointers and scaled by a sensitivity factor client-side
  before a packet is ever built.
- **Keyboard input has no custom on-screen keyboard.** `ControlScreen`
  keeps a zero-height `BasicTextField` permanently focused-on-demand and
  fed a single zero-width placeholder character; the system IME's edits to
  that field are diffed (`input/Typing.kt`'s `typeChar`) into
  `KEY_PRESS` packets via `input/KeyCodes.kt`'s evdev keycode table, and a
  *shrinking* value (the placeholder can't get shorter through normal
  typing) is read back as a `KEY_BACKSPACE` press. This reuses whatever
  keyboard the user already has — autocorrect, swipe typing, non-Latin
  layouts included — instead of the app drawing its own. A row of
  Ctrl/Alt/Super toggle chips plus Esc/Tab/arrows/Enter/Backspace buttons
  covers what a soft IME can't produce; since the daemon just replays raw
  key up/down state to the kernel, holding a modifier chip while typing via
  the IME bridge produces genuine combos (e.g. Ctrl+C) even though the two
  code paths never coordinate directly.
- **`network/NetworkClient`** — `discoverHosts()` broadcasts
  `DISCOVER_REQUEST` and collects responses for a fixed window; `pair()`
  sends `PAIR_REQUEST { pin }` and waits for one `PAIR_RESPONSE`;
  `InputChannel` is a small fire-and-forget UDP wrapper owning the
  monotonic `seq` counter from §2.1; `sendFile()` runs the three-phase
  upload from §2.2 and treats a read timeout on the trailer frame as
  success, since the daemon only ever speaks up on `FILE_CORRUPT`.
- **`transfer/FileTransferManager`** uses Android's Storage Access
  Framework for picking a file to send, so it never needs broad storage
  permissions — matching the "no shell-out, sandboxed by design" instinct
  `altay`'s `security::Sandbox` follows on the desktop side. SHA-256 is
  computed in a full pass over the SAF stream before the handshake (the
  protocol needs the hash up front), so a large file is read twice; an
  incremental digest alongside the send would remove that cost — see §5.
  Only sending is implemented, matching the daemon's receive-only
  `file_server.rs`.
- **`discovery/SavedHostsStore`** persists paired hosts (name, last-known
  IP) in `SharedPreferences` as one JSON array — plenty for the handful of
  hosts a phone realistically pairs with, so a real database felt like more
  machinery than the data warrants. The address is only a *starting point*
  for a saved host, not trusted blindly: `DeviceListScreen` re-resolves it
  via `InetAddress.getByName` on connect, since LAN devices commonly change
  address between sessions (DHCP lease churn) — there's no fresh broadcast
  re-scan wired into "connect to a saved host" yet (§5).

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
- **The two sides have never actually talked to each other.** Both build
  and pass their own tests/lint independently, but no one has run the
  daemon and the Android app against real BacakOS + Android hardware on
  the same network yet — that's the next thing to actually validate before
  trusting any of the above.
- `DeviceListScreen`'s "connect to a saved host" path uses the persisted IP
  as-is rather than re-scanning first; if the host's address changed since
  it was saved, connecting silently fails instead of falling back to a
  fresh discovery broadcast.
- `FileTransferManager` reads the whole file twice (once to hash, once to
  send) since `FILE_META`'s SHA-256 has to be known before streaming
  starts (§2.2) — an incrementally-hashed send (compute the digest as
  chunks go out, verify against a value only known after the last chunk)
  would need a protocol change to move verification to a trailer frame the
  sender computes, not the receiver.
- Android reconnect/retry behavior on Wi-Fi handoff or the daemon
  restarting mid-session — `InputChannel` currently just swallows send
  failures silently (see its doc comment) with no reconnect logic.
