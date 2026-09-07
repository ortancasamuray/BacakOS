# Uzakel — Architecture

🌐 [Türkçe özet](ARCHITECTURE.tr.md) · **English**

> **Status: verified end-to-end on real hardware, pairing enforced,
> traffic encrypted.** A physical Android phone and a live BacakOS
> session have discovered, PIN-paired, moved the real cursor, and
> transferred a real file — all confirmed via kernel-level capture, not
> just app-level logs (§6). Pairing now runs a real ephemeral X25519 ECDH
> exchange with a PIN-bound confirmation tag, and every input/file frame
> after that is ChaCha20-Poly1305-encrypted under the derived session
> keys (§2.3) — not just gated by IP. The Rust and Kotlin derivations were
> cross-checked byte-for-byte with a fixed known-answer test vector before
> being wired together (`daemon/examples/kat.rs`). See §2.3 for exactly
> what this scheme does and doesn't guarantee against an active attacker.
> QR pairing (§2.3.2) is real-hardware verified too — a phone scanning the
> `bacak-compositor` panel's QR paired and drove the cursor, one rendering
> bug found and fixed along the way (§6).

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
via a desktop notification. A client sends `PAIR_REQUEST { pin, client_pubkey }`
on the discovery UDP socket — `client_pubkey` is a fresh, single-use X25519
public key generated for this pairing attempt. The daemon generates its own
ephemeral X25519 keypair, does the ECDH, and replies
`PAIR_RESPONSE { accepted, daemon_pubkey, confirm_tag }`.

#### 2.3.1 Session key derivation

Both sides derive the same key material from the ECDH shared secret via
HKDF-SHA256 (`daemon/src/crypto.rs`, mirrored bit-for-bit by
`android/.../crypto/UzakelCrypto.kt`):

```
shared      = X25519(my_ephemeral_secret, their_ephemeral_pubkey)
prk         = HKDF-Extract(salt = "uzakel-pairing-v1", ikm = shared)
transcript  = client_pubkey || daemon_pubkey
c2s_key     = HKDF-Expand(prk, info = "uzakel c2s" || transcript)
s2c_key     = HKDF-Expand(prk, info = "uzakel s2c" || transcript)
confirm_key = HKDF-Expand(prk, info = "uzakel confirm" || transcript || pin)
confirm_tag = HMAC-SHA256(confirm_key, transcript)
```

`c2s_key` encrypts client→daemon traffic, `s2c_key` encrypts daemon→client
traffic — directional keys so a compromised nonce counter on one direction
can't be replayed against the other. `confirm_tag` is what actually ties the
exchange to the PIN: the daemon includes it in `PAIR_RESPONSE`, and the
Android client independently recomputes it from its own derivation before
trusting the response. A mismatch (wrong PIN, or a man-in-the-middle who
intercepted the ECDH exchange but doesn't know the PIN) makes the client
reject the pairing outright rather than encrypting anything under
attacker-controlled keys.

On a successful pairing, the daemon stores `(c2s_key, s2c_key)` in a
[`TrustStore`](../daemon/src/trust.rs) session keyed by the client's IP.
From then on, **every frame on the input and file-transfer sockets is
wrapped in `ENCRYPTED_FRAME { nonce: [u8; 12], ciphertext }`** — a
ChaCha20-Poly1305 AEAD ciphertext of the real inner frame, with a plain
little-endian counter (zero-extended to 12 bytes) as the nonce. Both
`input_manager.rs` and `file_server.rs` require this wrapper — there is
**no plaintext fallback** once a session exists; an un-decryptable or
replayed (counter ≤ highest seen) frame is dropped exactly like an
unpaired one always was. Confirmed on real hardware: before pairing, a
real phone's trackpad drags produced zero kernel input events on the
daemon side; after pairing, the same drags produced real `REL_X`/`REL_Y`
events (§6) — now carried as encrypted frames rather than plaintext ones.

**What this scheme does and doesn't guarantee:** it's ephemeral X25519 +
a PIN-bound confirmation tag, not a full PAKE (Password-Authenticated Key
Exchange). Confidentiality against a passive eavesdropper on the LAN is
real — traffic is genuinely encrypted, not just IP-gated. Active-attacker
resistance depends entirely on the PIN: an attacker who intercepts the
ECDH exchange *and* already knows the PIN (e.g. shoulder-surfed it) can
still complete a valid-looking handshake, because the PIN itself carries
no brute-force resistance of its own (a real PAKE like SPAKE2 would fold
the PIN into the key exchange itself, making an offline guess-and-check
attack on it infeasible; this scheme instead uses the PIN only to
authenticate a key exchange that already happened). Session trust is also
still **in-memory only** — it doesn't survive a daemon restart, so
Android's "Bağlan" (saved-host reconnect) always re-runs the full PIN +
ECDH handshake rather than reusing old keys, since it has no way to know
whether the daemon still remembers the old session (and reusing keys
across daemon restarts would risk nonce-counter reuse anyway). A real
TLS/PSK or SPAKE2 approach (§5) remains the next step if this needs to
hold up against a more capable active attacker.

#### 2.3.2 QR pairing

Manual entry (host IP + 6-digit PIN, both typed on the phone) is still the
baseline path — QR is an alternative on top of it, not a different
protocol. The daemon writes its current pairing state to
`~/.cache/uzakel/pairing.json` (`daemon/src/pairing_state.rs`) every time it
generates a fresh PIN — at startup, and again after every successful pair,
same trigger as the desktop notification. The file's LAN address comes from
a side-effect-free `UdpSocket::connect` route lookup (never sends a
packet), not from the discovery socket's own bind address (`0.0.0.0`, not
useful to a phone).

`bacak-compositor`'s Control Center has an "Uzakel'e Bağlan" tile
(`bacak/crates/bacak-compositor/src/plugins/uzakel.rs`, gated by
`/usr/share/bacak/plugins/uzakel.plugin` like every other optional Control
Center section) that reads this file fresh on every open and renders it as
a QR code:

```
uzakel://pair?host=<ip>&port=<discovery_port>&pin=<pin>&name=<url-encoded daemon name>
```

The Android app's camera screen (`ui/QrScanScreen.kt`, ZXing decoding a
`CameraX` `ImageAnalysis` frame — not Google's ML Kit, specifically to
avoid a Google-Play-Services runtime dependency for a LAN-only app) parses
this with `network/PairingUri.kt` and runs the *exact same*
`NetworkClient.pair()` call a manually typed PIN would — the QR only saves
typing the PIN and IP, it carries no cryptographic material itself. The
ECDH exchange, confirm-tag check, and everything in §2.3.1 happen
identically either way.

Since the file is a plain `.cache`-style scratch file with no daemon-side
server (see the module doc on `pairing_state.rs` for why: no ordering
guarantee between the daemon and the compositor starting, so nothing should
have to synchronously ask the daemon for this), it's inherently a *local*
mechanism — reading it requires being logged into the same BacakOS session
the daemon is running in, which is an acceptable trust boundary (anyone who
can read that file already controls the desktop being paired with).

---

## 3. Daemon (Rust) — module design

```
daemon/src/
├── main.rs            # env-based config, binds all three sockets, opens /dev/uinput, spawns the three tasks
├── discovery.rs        # UDP broadcast responder; answers DISCOVER_REQUEST + runs the ECDH pairing handshake (§2.3)
├── crypto.rs            # X25519 ECDH + HKDF-SHA256 key derivation + ChaCha20-Poly1305 seal/open (§2.3.1)
├── pairing_state.rs      # writes ~/.cache/uzakel/pairing.json (current PIN + LAN address) for QR pairing (§2.3.2)
├── trust.rs             # TrustStore — per-IP session (derived keys + AEAD state), checked by input_manager + file_server
├── protocol.rs          # packet header/opcode definitions + encode/decode, shared by every module
├── input_manager.rs     # UDP socket → looks up TrustStore session → decrypts → parses input opcodes → replays via /dev/uinput
└── file_server.rs       # TCP listener → looks up TrustStore session → decrypts/encrypts frames → chunked receive, SHA-256 verify, writes into ~/İndirilenler
```

- **`crypto.rs`** is the ECDH/HKDF/AEAD primitives from §2.3.1:
  `EphemeralKeypair::generate()`/`derive()` for the handshake, and
  `Cipher`/`Opener` wrapping ChaCha20-Poly1305 with a monotonic
  counter-nonce and replay rejection. Independently cross-checked against
  a parallel Kotlin/BouncyCastle implementation via a fixed known-answer
  test vector (`daemon/examples/kat.rs`) before either side trusted the
  other's bytes.
- **`trust.rs`** is one `TrustStore` (an `Arc<Mutex<HashMap<IpAddr, Session>>>`
  behind a small API), constructed once in `main.rs` and cloned into all
  three tasks. `discovery.rs` is the only writer (`trust()` after a
  confirm-tag-verified pairing, storing the derived `Cipher`/`Opener` pair);
  `input_manager.rs` and `file_server.rs` are read-only callers (`session()`)
  that use the stored keys to decrypt/encrypt every frame. Still IP-keyed
  and in-memory-only — see its doc comment and §2.3.1 for exactly what that
  does and doesn't guarantee.
- **`input_manager.rs`** owns a virtual mouse and keyboard device created
  through `/dev/uinput` via the `input-linux` crate (registering every valid
  evdev keycode up front, plus the three mouse buttons and the X/Y/wheel
  relative axes). It applies a mild acceleration curve to raw deltas before
  injecting them — on top of whatever curve the client already applied, as a
  safety net for a client that sends raw deltas — but does **not** clamp
  pointer position itself: every event is `REL_X`/`REL_Y` (relative), so
  screen-edge clamping is entirely the compositor's job, same as for a real
  mouse. Runs on a `tokio` task reading the UDP socket in a tight loop,
  drops every packet from an address `TrustStore` has no session for and
  every `ENCRYPTED_FRAME` that fails to decrypt/authenticate under that
  session's key (§2.3.1), and tracks a per-source-address last-seen `seq`
  (read from the *decrypted* inner packet) so an out-of-order or duplicate
  packet is dropped instead of moving the pointer backwards.
- **`file_server.rs`** is a plain `tokio` TCP listener; a connection from an
  address `TrustStore` has no session for gets an immediate `FILE_REJECT`
  and is dropped before the handshake even starts — this initial rejection
  is the one frame on this channel still sent in plaintext, since no
  session key exists yet to encrypt it under. A trusted connection gets its
  own task running the receive side of the state machine from §2.2 over
  `ENCRYPTED_FRAME`-wrapped traffic in both directions, picking a
  collision-safe destination filename
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
├── crypto/        # UzakelCrypto.kt — X25519/HKDF/ChaCha20-Poly1305, the Kotlin twin of daemon/src/crypto.rs (§2.3.1)
├── discovery/     # SavedHostsStore: SharedPreferences-backed list of previously-paired hosts
├── network/       # NetworkClient + PairingUri.kt: discovery scan, ECDH+PIN pairing (manual or QR, §2.3.2), encrypted InputChannel (UDP), encrypted sendFile (TCP)
├── input/         # TrackpadView (multi-touch), KeyCodes (evdev keycode table), IME→typeChar bridge
├── transfer/      # FileTransferManager: SAF file picking, SHA-256, progress via StateFlow
├── ui/            # UzakelApp (root), DeviceListScreen, QrScanScreen, ControlScreen, TransferScreen
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
- **`crypto/UzakelCrypto.kt`** is the Kotlin twin of `daemon/src/crypto.rs`
  (§2.3.1) — `EphemeralKeypair.generate()`/`derive()` for the handshake,
  `Cipher`/`Opener` wrapping BouncyCastle's ChaCha20-Poly1305 with the same
  counter-nonce and replay-rejection scheme. BouncyCastle rather than
  `javax.crypto`, since Android's own X25519 support only arrives on API
  33+ while this app's `minSdk` is 26. Cross-checked byte-for-byte against
  the Rust side via a fixed known-answer test vector before being wired
  into `NetworkClient`.
- **`network/NetworkClient`** — `discoverHosts()` broadcasts
  `DISCOVER_REQUEST` and collects responses for a fixed window; `pair()`
  generates an ephemeral keypair, sends `PAIR_REQUEST { pin, client_pubkey }`,
  and on `PAIR_RESPONSE` locally recomputes and checks `confirm_tag` before
  trusting the daemon's keys, returning a `PairedSession` (address + both
  directional session keys) rather than a plain boolean — a confirm-tag
  mismatch is treated the same as a wrong PIN. `InputChannel` is a small
  fire-and-forget UDP wrapper that encrypts every packet via a `Cipher`
  before sending, wrapping it in `ENCRYPTED_FRAME`, and owns the monotonic
  `seq` counter from §2.1 (carried inside the encrypted payload); `sendFile()`
  runs the three-phase upload from §2.2 with every frame in both directions
  wrapped/unwrapped via `Cipher`/`Opener`, and treats a read timeout on the
  trailer frame as success, since the daemon only ever speaks up on
  `FILE_CORRUPT`.
- **`ui/QrScanScreen`** (§2.3.2) is a `CameraX` `Preview` + `ImageAnalysis`
  bound to the back camera, decoded frame-by-frame with ZXing's
  `MultiFormatReader` directly over the analysis frame's Y-plane (no
  bitmap conversion — QR decoding only needs luminance). Deliberately
  ZXing, not Google's ML Kit: ML Kit's on-device barcode scanner still
  needs Google Play Services present at runtime, and this app otherwise
  never depends on anything beyond the LAN. `network/PairingUri.kt` parses
  the decoded `uzakel://pair?host=...&port=...&pin=...&name=...` string;
  `DeviceListScreen` then calls the exact same `NetworkClient.pair()` a
  manually typed PIN would — the QR only saves typing, it carries no key
  material of its own.
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

- ~~TLS material for the post-pairing sessions~~ — **done**, via ephemeral
  X25519 ECDH + PIN-bound confirmation + ChaCha20-Poly1305 (§2.3.1), not
  TLS/certs. Still open: upgrading the PIN check itself to a real PAKE
  (e.g. SPAKE2) so a PIN alone can't authenticate a session an active
  attacker already intercepted — see §2.3.1's caveat.
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
  `zbus`), removing the runtime dependency on `libnotify-bin` — and, more
  importantly, showing the pairing PIN *on screen* rather than depending on
  a notification daemon at all. Real-hardware testing hit exactly this: the
  test session had no notification service running, so the PIN was only
  ever visible in the daemon's own log — a real UX gap, not just a nice-to-have.
- `DeviceListScreen`'s "Bağlan" (saved host) path uses the persisted IP
  as-is rather than re-scanning first; if the host's address changed since
  it was saved, the re-pairing attempt (now required every time, see §2.3)
  just times out with "PIN yanlış veya cihaz yanıt vermedi" rather than
  falling back to a fresh discovery broadcast to find the new address —
  not silent anymore, but not a great error message for this specific
  cause either.
- `FileTransferManager` reads the whole file twice (once to hash, once to
  send) since `FILE_META`'s SHA-256 has to be known before streaming
  starts (§2.2) — an incrementally-hashed send (compute the digest as
  chunks go out, verify against a value only known after the last chunk)
  would need a protocol change to move verification to a trailer frame the
  sender computes, not the receiver.
- Android reconnect/retry behavior on Wi-Fi handoff or the daemon
  restarting mid-session — `InputChannel` currently just swallows send
  failures silently (see its doc comment) with no reconnect logic.
- A custom cursor (round, grows/shrinks on touch) for on-screen visibility
  from a distance — a `bacak-compositor` cursor-rendering feature, not
  something Uzakel itself can provide; out of scope here but worth linking
  to from that project when it's picked up.

---

## 6. What real-hardware testing found

Both sides built, unit-tested, and lint-passed independently well before
either touched real hardware — none of that caught what actually broke the
first time a real phone talked to a real daemon over real Wi-Fi. Three real
bugs, found and fixed in that order:

1. **`DatagramSocket.connect()` crashed the app the instant `ControlScreen`
   opened**, with `IllegalArgumentException: connect: -1`, on a real Redmi
   Note 8 (MIUI, Android 11) — reproducible every time. `InputChannel`
   didn't need the peer "connected" (every `send()` already carries the
   destination in its `DatagramPacket`), so the fix was just not calling
   `connect()` at all — see the doc comment on `InputChannel` in
   `network/NetworkClient.kt`.
2. **Every single input packet was silently failing to send, 100% loss,
   with zero visible symptoms.** `channel.mouseMove()` etc. are called
   directly from Compose gesture callbacks, which run on the main thread —
   but the actual `DatagramSocket.send()` syscall is exactly what Android's
   StrictMode `NetworkOnMainThreadException` exists to block. The socket
   itself opened fine (created via `withContext(Dispatchers.IO)`), so
   nothing about setup looked wrong; only every individual send afterward
   silently threw and was swallowed by `InputChannel`'s deliberately
   best-effort `catch (_: Exception) {}`. Gesture detection, delta math, and
   the daemon's own protocol handling were all independently confirmed
   correct throughout — this bug hid in the one place neither side's own
   testing could reach: the seam between a UI callback and a blocking
   socket call. Found by temporarily surfacing `InputChannel`'s real
   exception on-screen instead of swallowing it, which is what actually
   showed `NetworkOnMainThreadException` on the first try. Fixed by giving
   `InputChannel` its own background `CoroutineScope` (`Dispatchers.IO`)
   and launching each `send()` there instead of running it inline on the
   caller's thread.
3. **`DeviceListScreen` showed a paired host twice** — once from a fresh
   discovery response ("Eşleştir") and once from the saved-hosts list
   ("Bağlan") — because nothing filtered discovered hosts against the
   already-saved list. Fixed by excluding any discovered address already
   present in `savedList`.

A fourth finding was a tuning issue rather than a bug: the default
client-side sensitivity (`TrackpadView`'s `sensitivity = 1.5f`) combined
with the daemon's own acceleration curve (`input_manager.rs`'s `accelerate`)
to feel far too fast on a real trackpad — real captured `REL_X`/`REL_Y`
values reached 111 for what was meant to be a modest finger drag. Lowered
to `sensitivity = 0.4f`; this document's earlier note that
`input_manager.rs`'s curve is "a safety net, not the primary feel-tuning
knob" held up — the client-side number was what actually needed changing.

None of this diagnosis would have been possible without instrumenting the
running app directly: `/proc/net/udp` turned out to be **useless** for this
(Android 10+ hides other apps' sockets from it, even to `adb shell` — a
false negative that briefly pointed the investigation the wrong way), while
temporary on-screen counters (`moveCount`, `sendErrorCount`, `lastError`)
and capturing raw kernel events off `/dev/input/eventN` while the phone was
actively driving it were what actually pinned down each bug. Both are worth
reaching for again before assuming "no visible errors" means "working."

### Pairing enforcement (`trust.rs`), verified the same way

Once the input path was confirmed working, the same real phone + real
daemon setup was used to verify pairing enforcement immediately after
implementing it:

1. Daemon restarted (fresh, empty `TrustStore`) while the phone still had
   an open "connected" session from before the restart. Trackpad drags:
   **0 kernel input events** on the daemon side — traffic from the
   now-untrusted address is silently dropped, exactly as designed.
2. Same phone, `Bağlan` on the saved host — which now always re-opens the
   PIN dialog rather than connecting straight through (§2.3) — PIN entered,
   `discovery.rs` logged `client paired successfully`, trackpad drags
   immediately after: **224 real kernel events**.
3. File-transfer rejection checked independently (not from the phone, to
   keep the check fast): a `FILE_META` handshake sent from `127.0.0.1` —
   genuinely untrusted, since only the phone's LAN address had paired —
   got back `FILE_REJECT { "cihaz eşleştirilmemiş" }` and no file was
   written to the downloads directory.

The fix needed a small Android-side UX change alongside the daemon change:
`DeviceListScreen`'s "Bağlan" button used to connect straight through for a
saved host; now that trust is in-memory on the daemon and doesn't survive
a restart, it always re-runs the PIN dialog instead (see the doc comment on
that `Button`'s `onClick` in `DeviceListScreen.kt`) — connecting straight
through would have silently produced exactly the "everything looks
connected but nothing moves" symptom step 1 above deliberately reproduced
to confirm the fix.

### QR pairing (§2.3.2), verified on real hardware — one real bug found

Compiling and unit-testing `qr.rs`/`UzakelCrypto` proved the *bytes* were
right (§2.3.1's KAT cross-check); it said nothing about whether the QR
panel actually *displayed* correctly, since that's pure rendering with no
unit-testable surface. Real verification needed the actual GLES render
path: a nested `bacak-compositor --features runtime` (winit backend)
instance running against the live session's own Wayland socket, so the
panel's real code path ran without touching the production `udev`-backed
session at all.

**Bug found: the QR was invisible, hidden behind its own white background
card.** `render_uzakel_panel` pushed the QR's opaque white backing card
*before* the QR bitmap itself — this codebase's render element list is
top-first (earliest push = frontmost, see `render_audio_panel`'s three-pass
convention), so the card ended up drawn *in front of* the QR, not behind
it. The panel opened and showed its title/status/Kapat button correctly
(all pushed at points that happened not to overlap the card), which is
exactly why this wasn't obvious from the code alone — only actually looking
at the rendered panel showed a plain white box where the QR should be.
Fixed by swapping the push order (`985a094`).

After the fix, the full chain was verified with a real phone: the nested
compositor's QR panel scanned successfully by the Uzakel Android app's new
camera screen, `discovery.rs` logged `client paired successfully`, and a
follow-up trackpad drag captured **329 `REL_X` + 315 `REL_Y`** real kernel
events off `/dev/input/eventN` — confirming the entire
scan → parse → ECDH handshake → confirm-tag check → encrypted-session chain
from §2.3.2 works end to end, not just that each piece compiles.

One methodology note for next time: capturing `/dev/input/eventN` via
`cat … > file &` under a `timeout`, then asking the user to perform the
action, produced **0 captured bytes twice** even though the user confirmed
real cursor movement both times — `timeout` sends `cat` a `SIGTERM`, whose
default disposition skips any pending buffered write, silently dropping
whatever `cat` had already read but not yet flushed to the output file. A
small Python capture loop (`os.read()` + immediate `write()` + `flush()`
per chunk, driven by `select()` on a non-blocking fd) fixed it on the third
attempt. Prefer that pattern over shelling out to `cat` for any future
kernel-event capture in this project.
