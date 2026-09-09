# bacak-remote — PC screen/input bridge for Bacak OS

🌐 [Türkçe](README.tr.md) · **English**

A low-latency, bidirectional screen-streaming and input-redirection bridge:
a PC (Windows/Linux/macOS) streams its desktop to a Bacak OS client, and the
Bacak OS side forwards touch/pointer/keyboard input back to drive the PC.

This is the PC-facing sibling of [`uzakel`](../ARCHITECTURE.md) (which does
the same job in the opposite direction — a phone controlling a BacakOS
machine). Both share the same design instincts (relative pointer deltas,
UDP for anything latency-sensitive, "freshest wins" over buffering) but are
independent wire protocols and codebases — a PC desktop stream is a very
different payload than trackpad deltas.

> **v1 status: verified on same-machine loopback, not yet over real Wi-Fi.**
> Server + client were run against each other on one machine (Xvfb virtual
> X11 display, `127.0.0.1`) and the video pipeline was confirmed live end to
> end: `Hello`/`HelloAck` pairing, real 1280×800 capture, periodic
> `FrameInfo`/`FrameChunk` traffic matching the configured frame rate, and a
> `wgpu` (llvmpipe/Vulkan) render loop running for 80+ seconds with zero
> errors. Input injection was confirmed server-side by sending a real wire
> packet straight to the input port — `enigo` decoded and injected it via
> `libxdo` with no error — but the client's own winit→UDP send path wasn't
> exercised with a live mouse/touch in that pass (see "What's been tested"
> below). It has **not** been run over a real Wi-Fi link between two separate
> machines yet, and several pieces named in the original spec (hardware
> H.264/AV1 encoding, QUIC/WebRTC, zero-copy `dmabuf` into the compositor,
> real multi-touch injection) are deliberately **not** implemented — see
> "Honest scope" below before treating any of those as done.

---

## Honest scope: what's real vs. what's future work

Building a *production* hardware-accelerated (NVENC/VAAPI/DXGI) + QUIC/WebRTC
+ zero-copy-into-the-compositor pipeline is a multi-week effort with
per-platform system dependencies (CUDA/VAAPI drivers, a running
xdg-desktop-portal + PipeWire session, GStreamer/ffmpeg system libs) that
can't be verified in one pass without that hardware/OS matrix in front of it.
Shipping code that *claims* to do that but can't actually be exercised would
be worse than being explicit about the gap. So v1 instead ships a genuinely
working baseline with clean seams to grow into the full spec:

| Spec item | v1 reality | Upgrade path |
|---|---|---|
| Capture | [`scrap`](https://docs.rs/scrap) — X11 (incl. XWayland) on Linux, DXGI on Windows, CoreGraphics on macOS | A `PipeWireCapturer` behind the same `run_capture_thread` contract, for native-Wayland hosts (`bacak-remote-server/src/capture.rs` module doc) |
| Video codec | Raw BGRA + `zstd` (lossless, no GPU/driver dependency) | Add a `Codec` variant + matching encode/decode backend (`gstreamer-rs`/`ffmpeg-sys-next`/NVENC/VAAPI bindings) — `bacak_remote_proto::Codec` and `encode.rs`/`decode.rs` are the only places that touch codec-specific bytes |
| Transport | Plain `tokio` UDP, manual chunking, no FEC/ack, "freshest frame wins" | `quinn` (QUIC) for congestion control + optional reliability, or RTP+FEC, behind the same `Message`/chunk API |
| Client rendering | Standalone `winit` + `wgpu` window, CPU-side texture upload | A `bacak-compositor` plugin surface sharing its EGL context for a zero-copy blit (`render.rs` module doc) — also resolves the "should this even be a separate app" tension with [the project's own rule](../../bacak/README.md) against standalone desktop apps |
| Touch forwarding | `winit::event::Touch`, single-finger 1:1 (client) → single-pointer `enigo` injection, one active finger (server) | Real `wl_touch`/libinput interception once this lives inside the compositor; a dedicated virtual multi-touch `/dev/uinput` device on the host for true concurrent-finger gestures (pinch/two-finger pan) — see `input_inject.rs` module doc |
| Input injection | `enigo` (relative mouse motion, buttons, scroll, absolute-position touch-as-click) | Same `enigo` crate covers all three target OSes; no change needed unless multi-touch injection (above) is tackled |

Latency numbers (touch-to-photon, <30 ms network) were a design *target* in
the spec, not something measured here — no two-machine Wi-Fi test has been
run. Treat the transport as "designed for low latency" (UDP, small chunks,
drop-stale-frame reassembly, `Immediate` present mode where supported), not
as a benchmarked guarantee.

---

## What's been tested

Same-machine loopback, server and client both pointed at an `Xvfb :99`
virtual X11 display (1280×800), `bacak-remote-client` connecting to
`127.0.0.1`:

- **Pairing** — client's `Hello` reached the server, `HelloAck` came back
  with the real captured resolution (`1280x800`); both UDP socket pairs
  (video `9910`, input `9911`) showed `ESTAB` in `ss -u -an`.
- **Video pipeline** — server ran 80+ seconds with no encode/send errors,
  its write-syscall rate matching the configured frame rate (2 sends/frame —
  `FrameInfo` + one `FrameChunk`, since a static Xvfb frame zstd-compresses
  to well under the 1200 B chunk budget). Client found a Vulkan adapter
  (`llvmpipe`, software), opened its window, and ran its render loop the
  same 80+ seconds with zero `wgpu::SurfaceError`s or panics.
- **Input injection (server side only)** — a raw wire-format `PointerMotion`
  + left-click packet sent directly at the server's input port was decoded
  and handed to `enigo` without error (real `libxdo` call against the
  virtual display). The client's own `winit` event → UDP send path was
  **not** exercised in this pass (no synthetic-input tool available in that
  environment to drive real mouse/touch events into the client's window).

**Not yet tested:** two physically separate machines over real Wi-Fi; the
full client-side input capture path with a live pointer/touch; behavior
under packet loss or jitter; sustained multi-minute runs; real (non-Xvfb)
desktop content, which will compress far larger than a blank virtual
screen and exercise the chunking path much harder.

---

## Workspace layout

```
uzakel-pc/
├── bacak-remote-proto/     # shared wire protocol (postcard-serialized)
│   └── src/lib.rs          # Message, InputEvent, FrameInfo/FrameChunk, encode()/decode()
├── bacak-remote-server/    # PC-side daemon: capture, encode, stream, inject
│   └── src/
│       ├── capture.rs      # scrap-based screen grabber, own OS thread
│       ├── encode.rs       # zstd compress + chunk split
│       ├── network.rs      # UDP video link (Hello/HelloAck/frames) + input listener
│       ├── input_inject.rs # enigo-based pointer/touch injection
│       └── main.rs         # wires capture -> encode -> network, + input -> inject
└── bacak-remote-client/    # Bacak OS-side receiver + input forwarder
    └── src/
        ├── decode.rs        # frame reassembly (chunks -> zstd decompress)
        ├── network.rs       # UDP video receiver + input sender
        ├── render.rs        # wgpu texture upload + fullscreen-quad present
        ├── input_capture.rs # winit events -> InputEvent
        └── main.rs          # winit event loop wiring it all together
```

## Wire protocol summary

One `Message` enum (`bacak-remote-proto`), postcard-encoded behind a fixed
`[MAGIC:4][VERSION:1]` header so a stray or version-skewed packet is rejected
before deserializing:

- `Hello` / `HelloAck` — client announces itself on the video port, server
  replies with the real screen size and remembers the client's address.
- `FrameInfo` — one per frame, precedes its chunks; carries width/height/
  codec/chunk count.
- `FrameChunk` — up to `MAX_CHUNK_BYTES` (1200 B) of the compressed frame
  payload; the client's `FrameReassembler` drops an incomplete frame outright
  the moment a newer `FrameInfo` arrives (never buffers stale frames).
- `Input` — `PointerMotion` (relative dx/dy), `PointerButton`, `PointerScroll`,
  `TouchDown`/`TouchMotion`/`TouchUp` (normalized 0.0–1.0 against the
  server's screen).
- `Heartbeat` / `Bye` — liveness and clean teardown.

Video traffic and input traffic use **separate UDP sockets/ports**
(`DEFAULT_VIDEO_PORT` 9910, `DEFAULT_INPUT_PORT` 9911) so a burst of frame
chunks can never queue behind — or delay — an input packet, matching the
per-purpose channel split `uzakel`'s Android bridge already uses.

## Build & run

Each crate needs its platform's usual Rust/graphics toolchain (a C linker,
and on Linux the X11 dev headers `scrap` and `enigo` link against —
`libxcb-randr0-dev libxdo-dev` on Debian/Ubuntu; `cargo check` succeeds
without them, but linking a real binary needs them installed). No PipeWire,
GStreamer, ffmpeg, or GPU vendor SDK is required for v1.

```sh
sudo apt-get install libxcb-randr0-dev libxdo-dev   # Debian/Ubuntu Linux host
```

```sh
cd uzakel-pc
cargo build --release --workspace

# On the PC to be streamed (Windows/Linux/macOS):
./target/release/bacak-remote-server --fps 60

# On the Bacak OS machine (or any test machine on the same LAN for now):
./target/release/bacak-remote-client <server-lan-ip>
```

```sh
cargo test -p bacak-remote-proto   # protocol round-trip + framing tests
cargo clippy --workspace --all-targets
```

Local loopback test (both binaries on the same machine, `127.0.0.1`) is the
fastest way to confirm the pipeline before trying it across two machines on
Wi-Fi.

## License

GPL-3.0-or-later (matches the rest of BacakOS).
