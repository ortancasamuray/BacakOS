# Uzakel — BacakOS Remote Control & File Transfer

🌐 [Türkçe](README.tr.md) · **English**

> **Status: both `daemon/` and `android/` have a first pass implemented.**
> `daemon/` builds, passes its unit tests, and implements discovery/pairing,
> input replay, and file receive end-to-end. `android/` builds a debug APK
> and passes Android Lint (verified with a real Gradle + Android SDK build,
> not just written and hoped) — device discovery, PIN pairing, a trackpad +
> IME-bridged keyboard, and one-way file sending are wired up. Neither side
> has been run against the other on real hardware yet, and pairing still
> isn't enforced on the input/file sockets — see the security note in
> [ARCHITECTURE.md](ARCHITECTURE.md) §"open questions."

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
