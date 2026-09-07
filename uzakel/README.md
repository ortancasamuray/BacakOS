# Uzakel — BacakOS Remote Control & File Transfer

🌐 [Türkçe](README.tr.md) · **English**

> **Status: design phase.** No code has been written yet — this document and
> [ARCHITECTURE.md](ARCHITECTURE.md) describe the intended system. Nothing
> below should be read as "done."

Uzakel ("remote hand" in Turkish) is a two-part remote-control and
file-transfer ecosystem for BacakOS: an Android app that turns a phone into
a trackpad/keyboard/file-transfer client for a BacakOS machine, and a Rust
daemon on the BacakOS side that simulates input and receives files.

## Components

| Component | Language | Role |
|---|---|---|
| **Daemon** (host, BacakOS) | Rust | Discoverable on the LAN; simulates mouse/keyboard via `/dev/uinput`; receives files over TCP; runs as a `systemd --user` service. |
| **Client** (Android) | Kotlin, Jetpack Compose | Trackpad + virtual keyboard UI; sends input over UDP; sends/receives files over TCP; discovers and remembers hosts. |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the wire protocol, packet layout,
and both components' internal module design.

## Why not just VNC/RDP?

Uzakel is not a screen-sharing protocol — it never encodes or transmits a
video stream. It's closer to a Bluetooth trackpad/keyboard plus a drag-and-
drop file transfer, purpose-built for "control this specific BacakOS
machine from my phone on the same network," which keeps latency and
bandwidth far below any remote-desktop protocol.

## Planned repository layout

```
uzakel/
├── daemon/            # Rust — the BacakOS-side service
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs           # CLI/daemon entry, systemd integration
│       ├── discovery.rs      # mDNS / UDP broadcast responder
│       ├── protocol.rs       # shared packet definitions (header, opcodes)
│       ├── input_manager.rs  # /dev/uinput virtual mouse + keyboard
│       └── file_server.rs    # chunked TCP file receive/send + SHA-256 verify
└── android/           # Kotlin — the phone-side client
    └── app/src/main/kotlin/org/anadolupanteri/uzakel/
        ├── discovery/         # host discovery + saved-host list
        ├── network/           # NetworkClient (coroutines), UDP input socket, TCP file socket
        ├── input/             # TrackpadView delta capture, sensitivity/acceleration
        ├── transfer/          # FileTransferManager, SAF integration, progress state
        └── ui/                # Compose screens: trackpad, keyboard, device list, transfer panel
```

## Build (once code exists)

```sh
# Daemon
cd daemon && cargo build --release
systemctl --user enable --now uzakel-daemon

# Android
cd android && ./gradlew assembleDebug
```

## License

GPL-3.0-or-later (matches the rest of BacakOS).
