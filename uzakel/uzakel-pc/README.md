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

> **v1 status: verified end to end on two separate machines over real
> LAN/Wi-Fi, AND on the real BacakOS desktop (not just Xvfb).**
> `bacak-remote-server` (Windows 10, a real VM on separate physical
> hardware) and `bacak-remote-client` (this Linux host) ran against each
> other over a real network link (`192.168.1.x`, not loopback): real
> pairing, real capture of the Windows machine's actual desktop
> (1400×1050), sustained streaming with zero errors, and real
> network-delivered pointer/click packets visibly moving and clicking the
> cursor on the Windows machine, confirmed by a person watching that screen.
> Separately, the client was run inside the *actual* `bacak-compositor`
> Wayland session on this machine (not a virtual display) — real GPU
> (`AMD Radeon Vega 6`, RADV), a real window on the real desktop, and, after
> fixing three real bugs real-hardware testing turned up (see "What real
> BacakOS desktop testing found" below), 911 real local mouse/touchpad
> events captured and delivered with zero injection errors. Several pieces
> named in the original spec (hardware H.264/AV1 encoding, QUIC/WebRTC,
> zero-copy `dmabuf` into the compositor, real multi-touch injection) remain
> deliberately **not** implemented — see "Honest scope" below before
> treating any of those as done.

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

### Same-machine loopback (Xvfb)

Server and client both pointed at an `Xvfb :99` virtual X11 display
(1280×800), `bacak-remote-client` connecting to `127.0.0.1`:

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
  virtual display).

### Two separate physical machines, real LAN/Wi-Fi (Windows 10 ↔ this Linux host)

`bacak-remote-server.exe` installed via the NSIS installer on a real
Windows 10 Pro machine (`192.168.1.55`), `bacak-remote-client` run on this
Linux host (`192.168.1.15`, `Xvfb` for the render surface) — a real,
separate-hardware, real-network test, not loopback:

- **Pairing + capture** — `Hello`/`HelloAck` succeeded over the real
  network; the server reported (and the client received) the Windows
  machine's *actual* desktop resolution, `1400x1050` — proof `scrap`'s DXGI
  backend is really capturing that screen, not a stub value.
- **Sustained streaming** — the server's process CPU time kept advancing
  over a 5-second sampling window (encode+send work happening continuously,
  not just at handshake), with both UDP socket pairs staying `ESTAB` on the
  Linux side throughout.
- **Input delivery** — a real wire-format `PointerMotion`/`PointerButton`
  burst sent from the Linux host's real IP reached the Windows server's
  input port, decoded correctly, and was handed to `enigo` with **zero**
  injection errors logged, every time, across several repeated sends.
- **Live, visually-confirmed injection** — a real wire-format mouse
  sweep + click sent from the Linux host was watched, live, on the Windows
  machine's actual screen: the cursor visibly moved and clicked. See
  "What real two-machine testing found" below for why the first attempts
  at confirming this gave misleading results, and for a genuine bug this
  process turned up along the way.

### Real BacakOS desktop (Wayland, not Xvfb), loopback

`bacak-remote-server` and `bacak-remote-client` both on this machine, but
the client run inside the **actual, production `bacak-compositor`
session** (`WAYLAND_DISPLAY=wayland-bacak-0`) instead of a virtual display —
the point of this pass was specifically to exercise the client's real local
input capture, which every earlier pass had only exercised via
directly-injected wire packets:

- **Pairing + real GPU rendering** — `PairRequest`/`PairResponse` succeeded
  against the real compositor's Wayland socket; `wgpu` found the machine's
  real hardware adapter (`AMD Radeon Vega 6 Graphics`, RADV/Vulkan) instead
  of a software rasterizer, and a real window appeared on the real desktop
  (confirmed via screenshots taken with `grim`).
- **Real local input, end to end** — after fixing the three bugs in "What
  real BacakOS desktop testing found" below, moving the mouse and clicking
  over the real window produced 911 real `PointerMotion`/`Touch*` packets,
  all reaching the server, decrypting correctly, and injecting via `enigo`
  with zero errors. This is the one path no other test pass exercised —
  every prior confirmation of the input path used a hand-crafted wire
  packet sent directly at the input port, never the client's own
  `winit` capture code.

**Not yet tested:** macOS (no Mac available in this pass); the same
PIN-paired security flow over a real Wi-Fi link between two separate
machines (tested on loopback and via the Windows pass above, but not both
combined in one run); behavior under real packet loss/jitter; sustained
multi-minute runs; real (non-Xvfb, non-blank) desktop content, which
compresses far larger than a blank/static screen and exercises the
chunking path much harder.

## What real two-machine testing found

Real hardware surfaced one genuine bug immediately, and one measurement
headache that took several tries to work through — both are worth
recording here, `uzakel`'s own §6-style, rather than glossed over.

**Bug found and worked around: `SendInput` returns `ERROR_ACCESS_DENIED`
(Win32 error 5) when called from a process launched over SSH, even though
`tasklist` reports it running in the interactive console session.** The
first attempt to test input injection ran `bacak-remote-server.exe`
directly over the SSH connection used to manage the test machine. Video
capture (DXGI) worked fine from there — but every `enigo` call silently
returned `Ok(())` while the cursor never moved, and a follow-up raw
`SendInput` call (bypassing `enigo` entirely) confirmed why: Windows'
OpenSSH server places a session's processes on their own, non-default
window station, and `SendInput` specifically requires access to the
*interactive* window station (`WinSta0\Default`) — a restriction Desktop
Duplication (used for capture) doesn't share. Session ID alone (what
`tasklist`/`query session` report) doesn't tell you which window station a
process is actually attached to. **Practical upshot: don't try to drive
`bacak-remote-server` itself over SSH for testing input — run it from an
actual interactive logon (console, RDP, or physically at the machine).**
This has no bearing on real deployments (nobody SSHes into their own PC to
run their own remote-control server), but it's a sharp edge for anyone
automating tests the way this one was tested.

**Confirmed working, in isolation: both raw `SendInput` and `enigo`'s
wrapper around it correctly move the real cursor on this machine**, when
run from an actual interactive logon. Two standalone diagnostics (one
calling `windows::Win32::UI::Input::KeyboardAndMouse::SendInput` directly,
one calling `enigo::Enigo::move_mouse`) each moved the cursor from a real
starting position to the exact expected result — clamped at the screen's
bottom-right corner (`(1399, 1049)` on the `1400x1050` display) after a
relative move sized to overshoot it, precisely the behavior a real
`MOUSEEVENTF_MOVE`/`SendInput` call produces at a screen edge. Both were
run twice with the same clean, unambiguous result.

**Resolved: live, visually-confirmed end-to-end injection through the
actual server pipeline.** The first few attempts to correlate "operator
sends a packet from the Linux side" with "person at the Windows machine
reads a cursor position" produced noisy, inconsistent deltas — traced to
the Windows machine being a VM, where the host's mouse-integration layer
was itself nudging the guest cursor between the two manual position
readings, on top of the usual chat round-trip timing slack. Switching from
a numeric before/after reading to a direct "watch the screen, confirm
visually" check (a large, fast sweep + click, easy to see instantly) gave
a clean, unambiguous result: **the cursor visibly moved and clicked**,
confirmed by the person at the machine. Combined with the isolated
`SendInput`/`enigo` diagnostics above and the server's error-free receipt
log, the full chain — real network → decode → `enigo` → visible cursor
movement on real Windows hardware — is now verified, not just inferred.

## What real BacakOS desktop testing found

Running `bacak-remote-client` inside the actual `bacak-compositor` Wayland
session (not `Xvfb`) surfaced **three** real bugs — none of them in
`bacak-compositor` itself, despite that being the first suspect. Found and
fixed in this order:

1. **Our own logging setup was silently discarding `RUST_LOG=debug`.**
   `main.rs` built its filter as
   `EnvFilter::from_default_env().add_directive("info")` — `EnvFilter`
   breaks a tie between two directives of equal specificity (both
   unscoped/global here) in favor of whichever was added *last*, so the
   hardcoded `"info"` silently overrode `RUST_LOG=debug` every time. Every
   earlier "no events at all" observation while debugging this was
   actually "no events *visible*," not "no events firing" — a real,
   costly, self-inflicted false signal. Fixed by only falling back to
   `"info"` when `RUST_LOG` is absent/invalid
   (`EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())`),
   so an explicit `RUST_LOG` is now fully respected.
2. **`winit`'s Wayland backend never emits `DeviceEvent::MouseMotion`.**
   Once logging actually worked, real mouse movement over the real window
   showed up as `WindowEvent::CursorMoved` and low-level
   `DeviceEvent::Motion { axis, value }` pairs — never the
   `DeviceEvent::MouseMotion { delta }` variant `input_capture.rs` relied
   on for relative deltas (that variant is populated on X11 via raw
   XInput2, a path Wayland compositors don't have an equivalent for through
   `winit`'s current backend). Pointer motion was therefore silently dead
   on Wayland specifically, while working fine on the `Xvfb`/X11 passes
   earlier — the exact kind of gap a same-machine-only test plan can't
   catch. Fixed by computing relative deltas from consecutive
   `WindowEvent::CursorMoved` positions instead, which fires on every
   backend.
3. **The server compared full `SocketAddr`s (IP *and* port) across two
   different UDP sockets.** `PairedSession.addr` is captured from the video
   socket's `PairRequest` sender address; `run_input_listener` then
   rejected every real input packet because it arrived on a *different*
   UDP socket with a different (but legitimate) ephemeral source port,
   logging "paired with a different address" at debug level — invisible
   until bug #1 was fixed, which is what finally exposed this one. Fixed
   by comparing only the IP (`sess.addr.ip() != from.ip()`), since two
   sockets from the same client legitimately differ in port.

After all three fixes: a real mouse/touchpad session on the real BacakOS
desktop produced 911 input packets (`PointerMotion`, `Touch{Down,Motion,Up}`
— this laptop's touchpad drives `winit`'s touch path, not just pointer),
every one decrypted and handed to `enigo` with zero injection errors.

None of these three bugs would have been caught by the loopback/`Xvfb`
testing earlier — each needed the real compositor, a real GPU-backed
Wayland window, or a real second UDP socket to surface. Worth remembering
next time something "just isn't receiving any events": check whether your
own logging is lying to you before suspecting the platform.

---

## Workspace layout

```
uzakel-pc/
├── .cargo/config.toml      # Windows cross-compile linker + crt-static settings
├── vendor/scrap-0.5.0/     # locally patched `scrap` (see "Windows: prebuilt .exe" below)
├── bacak-remote-proto/     # shared wire protocol (postcard-serialized)
│   └── src/lib.rs          # Message, InputEvent, FrameInfo/FrameChunk, encode()/decode()
├── bacak-remote-server/    # PC-side daemon: capture, encode, stream, inject
│   ├── packaging/windows/  # build.sh + installer.nsi -> bacak-remote-server-setup.exe
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

- `PairRequest` / `PairResponse` — the only messages ever sent in the clear;
  everything else travels wrapped in `Encrypted` once paired (see "Security
  & pairing" below).
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

## Security & pairing

Pairing reuses `uzakel`'s exact scheme (`uzakel/daemon/src/crypto.rs`,
verified on real hardware there) rather than inventing a new one: a 6-digit
PIN, shown on the server's console at startup, combined with an ephemeral
**X25519 ECDH** key exchange. The PIN alone never touches the wire and is
never used as an encryption key — it only authenticates the key exchange
(via an HMAC-SHA256 `confirm_tag` the client checks before trusting the
server's public key), so a passive eavesdropper watching the exchange learns
nothing usable, and a man-in-the-middle without the PIN can't quietly
substitute their own keys. See `bacak-remote-proto/src/crypto.rs`'s module
doc for the exact derivation and honest caveats (it is not a full PAKE — an
attacker who already knows the PIN can still complete a valid-looking
handshake, same limitation `uzakel` documents for its own scheme).

One detail specific to this project (not present in `uzakel`, which only
has one encrypted channel): video and input travel on **separate UDP
sockets**, so each gets its **own independently-derived key pair** via
`SessionMaterial::channel_keys("video" | "input")` — reusing one key across
two independently-counted nonce sequences would have been a real
(key, nonce) reuse bug. `bacak_remote_proto::crypto`'s module doc spells
this out; it's the one place this scheme had to extend `uzakel`'s original
rather than copy it verbatim.

Tested (loopback, Xvfb): correct-PIN pairing succeeds and streams normally;
wrong-PIN pairing is cleanly rejected on both sides (server logs and refuses
to derive/store keys; client gets a clear "server rejected pairing" error
rather than hanging or retrying forever).

**Not yet done:** a real PAKE (SPAKE2/OPAQUE) for PIN-guess resistance
against an attacker who intercepts the exchange; PIN rotation after a
successful pairing (`uzakel`'s daemon does this, this project's server
doesn't yet — same PIN is valid for the whole process lifetime); a UI for
entering the PIN (v1 is a CLI positional argument — see "Build & run").

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
#   Pairing PIN: 123456   <- shown once at startup; type this into the client

# On the Bacak OS machine (or any test machine on the same LAN for now):
./target/release/bacak-remote-client <server-lan-ip> <pairing-pin>
```

```sh
cargo test -p bacak-remote-proto   # protocol round-trip + framing tests
cargo clippy --workspace --all-targets
```

Local loopback test (both binaries on the same machine, `127.0.0.1`) is the
fastest way to confirm the pipeline before trying it across two machines on
Wi-Fi.

## Windows: prebuilt `.exe` + installer

`bacak-remote-server` cross-compiles cleanly from Linux to a real, standalone
Windows `.exe` — no Windows machine or Visual Studio needed to *build* it
(running it is of course still a Windows-only step). `bacak-remote-client`
is not built for Windows: it's the Bacak OS-side receiver, meaningless on
the platform being streamed *from*.

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt-get install mingw-w64 nsis   # gcc-mingw-w64 linker + NSIS installer builder

bacak-remote-server/packaging/windows/build.sh
# -> bacak-remote-server/packaging/windows/bacak-remote-server-setup.exe
```

What `build.sh` does, and why each piece exists:

- **`.cargo/config.toml`** points the `x86_64-pc-windows-gnu` target at
  `x86_64-w64-mingw32-gcc-posix` specifically (not whatever
  `x86_64-w64-mingw32-gcc` defaults to via `update-alternatives` — the
  `win32` threading variant is missing pieces Rust's std needs) and sets
  `-C target-feature=+crt-static`, so the shipped `.exe` depends on nothing
  but stock Windows DLLs (`kernel32`, `user32`, `ws2_32`, `d3d11`, `dxgi`,
  `msvcrt`) — confirmed via `objdump -p`, no `libwinpthread-1.dll`/
  `libgcc_s_seh-1.dll`/`libstdc++-6.dll` to bundle or ask the user to install.
- **`vendor/scrap-0.5.0/`** is a locally patched copy of the `scrap` crate.
  Upstream's `build.rs` picks its capture backend with `cfg!(windows)` —
  which reflects the *host* a build script runs on, not the `--target`
  being cross-compiled for, so building from Linux always selected the X11
  backend and failed to link against Windows' DXGI. The vendored copy
  reads Cargo's `TARGET` env var instead (one-line fix; see the file's own
  comment). Pinned via `[patch.crates-io]` in the workspace `Cargo.toml`,
  so the native Linux build is unaffected — verified with
  `cargo check --workspace` after applying the patch.
- **`installer.nsi`** (built with `makensis`, from the `nsis` Debian
  package — works fine cross-platform, no Windows needed here either)
  installs to `Program Files`, adds Start Menu shortcuts, registers an
  uninstaller, and opens the inbound Windows Firewall UDP rule for ports
  9910–9911 — without that rule, Windows Defender Firewall silently drops
  the client's `Hello`, with no error on either side, which would otherwise
  be a very confusing first-run failure.

**Update: this has now been run on a real Windows 10 machine** — the
installer, its Firewall rule, DXGI capture (real desktop resolution
detected and streamed), and `enigo`'s `SendInput` injection (moving and
clicking the real cursor, watched live by a person at the machine) all
work. See "What real two-machine testing found" above for the full
account, including a real bug the process exposed (unrelated to Windows
itself — an SSH window-station restriction that only bites if you try to
test this way). `scrap`'s DXGI
backend (inherited third-party code, not ours) still uses
`mem::uninitialized()` in a few spots — deprecated and technically UB,
though the structs are populated immediately after by the DXGI/Direct3D
call, which is the pattern that made this acceptable when the crate was
written; it hasn't caused an observed failure, but it's inherited debt
worth knowing about.

## License

GPL-3.0-or-later (matches the rest of BacakOS).
