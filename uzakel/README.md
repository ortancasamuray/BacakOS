# Uzakel — BacakOS Remote Control & File Transfer

🌐 [Türkçe](README.tr.md) · **English**

> **Status: verified end-to-end on real hardware.** `daemon/` and `android/`
> have been run against each other for real — a physical Android phone
> (MIUI, Android 11) discovering, PIN-pairing with, and moving the real
> cursor on a live BacakOS session's `bacak-compositor` over real Wi-Fi, with
> the resulting kernel `REL_X`/`REL_Y` events captured directly from
> `/dev/input/eventN` to confirm it. File transfer was verified the same way
> (real SHA-256-checked file landing in `~/İndirilenler`). This found and
> fixed three real bugs no amount of code review would have caught — see
> ARCHITECTURE.md §6. Pairing still isn't enforced on the input/file
> sockets — see the security note in [ARCHITECTURE.md](ARCHITECTURE.md)
> §"open questions."

Uzakel ("remote hand" in Turkish) is a two-part remote-control and
file-transfer ecosystem for BacakOS: an Android app that turns a phone into
a trackpad/keyboard/file-transfer client for a BacakOS machine, and a Rust
daemon on the BacakOS side that simulates input and receives files.

## Components

| Component | Language | Role |
|---|---|---|
| **Daemon** (host, BacakOS) | Rust | Discoverable on the LAN; simulates mouse/keyboard via `/dev/uinput`; receives files over TCP; runs as a `systemd --user` service. |
| **Client** (Android) | Kotlin, Jetpack Compose | Trackpad + IME-bridged keyboard UI; sends input over UDP; sends files over TCP (receive direction not implemented on either side yet); discovers, pairs with, and remembers hosts. |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the wire protocol, packet layout,
and both components' internal module design.

## Why not just VNC/RDP?

Uzakel is not a screen-sharing protocol — it never encodes or transmits a
video stream. It's closer to a Bluetooth trackpad/keyboard plus a drag-and-
drop file transfer, purpose-built for "control this specific BacakOS
machine from my phone on the same network," which keeps latency and
bandwidth far below any remote-desktop protocol.

## Repository layout

```
uzakel/
├── daemon/            # Rust — the BacakOS-side service
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # daemon entry, binds all three sockets, opens /dev/uinput
│       ├── discovery.rs      # UDP broadcast responder + PIN pairing
│       ├── protocol.rs       # shared packet definitions (header, opcodes) + encode/decode + tests
│       ├── input_manager.rs  # /dev/uinput virtual mouse + keyboard
│       └── file_server.rs    # chunked TCP file receive + SHA-256 verify
└── android/           # Kotlin + Jetpack Compose — the phone-side client
    └── app/src/main/kotlin/org/anadolupanteri/uzakel/
        ├── protocol/          # Protocol.kt — byte-for-byte Kotlin mirror of daemon/src/protocol.rs
        ├── discovery/         # SavedHostsStore — persisted paired-host list
        ├── network/           # NetworkClient — discovery scan, PIN pairing, InputChannel (UDP), file send (TCP)
        ├── input/             # TrackpadView (multi-touch gesture surface), KeyCodes, IME→KEY_PRESS bridge
        ├── transfer/          # FileTransferManager — SAF file picking, SHA-256, progress StateFlow
        └── ui/                # UzakelApp (screen switching) + DeviceListScreen, ControlScreen, TransferScreen
```

## Build

```sh
# Daemon
cd daemon && cargo build --release && cargo test
UZAKEL_DOWNLOAD_DIR=~/İndirilenler ./target/release/uzakel-daemon   # or: systemctl --user enable --now uzakel-daemon (once packaged)

# Android
cd android && ./gradlew assembleDebug   # produces app/build/outputs/apk/debug/app-debug.apk
```

The daemon needs the running user in the `uinput` group to open
`/dev/uinput`; it logs a clear error naming that requirement if it can't. It
also shells out to `notify-send` for the pairing-PIN and file-received
notifications — install `libnotify-bin` (or equivalent) if notifications
don't appear.

The Android build needs `ANDROID_SDK_ROOT`/`local.properties` pointed at an
Android SDK with platform 34 + build-tools 34.0.0 installed (Android
Studio sets this up automatically); it has no other unusual requirements.

## License

GPL-3.0-or-later (matches the rest of BacakOS).
