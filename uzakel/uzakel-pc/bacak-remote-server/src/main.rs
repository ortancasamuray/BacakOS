mod capture;
mod encode;
#[cfg(windows)]
mod encode_h264;
#[cfg(windows)]
mod gui;
mod input_inject;
mod network;

use bacak_remote_proto::{DEFAULT_INPUT_PORT, DEFAULT_VIDEO_PORT};
use clap::Parser;
use input_inject::Injector;
use tokio::net::UdpSocket;

/// bacak-remote-server: streams this PC's screen to a Bacak OS client and
/// injects the input events it sends back. The pairing PIN is generated on
/// the *Bacak OS side* and shown large on its screen the moment its "Uzak
/// Masaüstü" panel opens (see that crate's `remote_desktop.rs` module doc) —
/// this side doesn't generate one; on Windows it shows its own small
/// pairing window (see `gui.rs`) so the operator sitting at this PC can read
/// that PIN off the BacakOS screen, type it in, and watch pairing succeed
/// without ever needing a console.
#[derive(Parser, Debug, Clone)]
pub(crate) struct Args {
    /// UDP port carrying pairing/video frames.
    #[arg(long, default_value_t = DEFAULT_VIDEO_PORT)]
    pub(crate) video_port: u16,
    /// UDP port carrying inbound touch/pointer events.
    #[arg(long, default_value_t = DEFAULT_INPUT_PORT)]
    pub(crate) input_port: u16,
    /// Capture rate; encode/network time permitting. Kept modest (not 30/60)
    /// by default: this pipeline has no real video codec/delta-encoding —
    /// every frame is a full raw-BGRA zstd blob — so a higher target here
    /// asks for more encode CPU *and* more bandwidth per second than most
    /// real links/CPUs sustain, which showed up on real hardware as bursty
    /// "sometimes fast, sometimes stalls" video rather than a clean
    /// lower-but-steady frame rate.
    #[arg(long, default_value_t = 15)]
    pub(crate) fps: u32,
    /// zstd compression level: higher = smaller frames (less bandwidth per
    /// frame), more CPU per frame. Left at zstd's own fast default — a
    /// real-hardware test raising this to 9 (to trade CPU for bandwidth)
    /// made the video *more* erratic, not less, on a CPU that couldn't
    /// keep up with that level at any usable frame rate. Bandwidth is
    /// better addressed via `fps`/resolution than via a heavier codec
    /// level in this naive per-frame-blob pipeline.
    #[arg(long, default_value_t = 3)]
    pub(crate) zstd_level: i32,
    /// The PIN shown on the BacakOS screen, passed non-interactively instead
    /// of typing it into the pairing window/console prompt — for
    /// scripted/automated launches only, since it usually ends up in shell
    /// history/logs. Also skips the graphical pairing window on Windows,
    /// since there's nothing left to type in it.
    #[arg(long)]
    pub(crate) pin: Option<u32>,
    /// Use the console PIN prompt even on Windows, instead of the graphical
    /// pairing window.
    #[arg(long)]
    pub(crate) no_gui: bool,
    /// [SINAMA/GEÇİCİ — bkz. HARDWARE_ENCODE_PLAN.md madde 2] `h264_qsv`
    /// donanım kodlayıcısını bu makinede birkaç saniye gerçek ekran
    /// görüntüsüyle çalıştırıp sonucu konsola yazar, sonra çıkar. PIN,
    /// eşleştirme, ağ hiç devreye girmez — sadece encoder'ın gerçekten
    /// açılıp paket ürettiğini görmek için. Sadece Windows'ta anlamlı;
    /// protokole/pipeline'a henüz bağlanmadı.
    #[cfg(windows)]
    #[arg(long)]
    pub(crate) test_h264_encode: bool,
    /// [DENEYSEL — bkz. HARDWARE_ENCODE_PLAN.md] Gerçek oturumda `RawZstd`
    /// yerine donanım H.264 (nvenc/amf/qsv, yoksa yazılım libx264'e düşer —
    /// `encode_h264.rs`'e bakın) kullan. Varsayılan kapalı: BacakOS tarafı
    /// (`remote_desktop.rs`) henüz H.264 decode etmiyor (plan madde 5) —
    /// bu bayrakla açılan bir oturumda BacakOS ekranı boş/donmuş kalır,
    /// sadece encode tarafını gerçek bir oturumda sınamak içindir.
    #[cfg(windows)]
    #[arg(long)]
    pub(crate) hardware_encode: bool,
}

/// Pairing progress reported by [`network::run_video_link`] as it happens —
/// consumed by the pairing window on Windows (see `gui.rs`); the console
/// path has no receiver, so these are simply never read there. (On a
/// non-Windows build nothing constructs a [`StatusChannel`] at all, so
/// these fields go unread there too — harmless, hence the blanket allow.)
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) enum SessionStatus {
    WaitingForClient,
    Paired { client_name: String, addr: String },
    Rejected { from: String },
    /// The session loop returned (operator hit "Eşleşmeyi Bitir", the link
    /// dropped, or the client disconnected) — the pairing window uses this
    /// to reset itself (re-enable the PIN field) instead of sitting on a
    /// stale "connected" status forever.
    Ended,
}

/// Delivers a [`SessionStatus`] to whoever is listening and, if that
/// listener needs an explicit wake-up, triggers it — the pairing window's
/// Win32 message loop does (its `wake` closure calls an `nwg::NoticeSender`,
/// see `gui.rs`), since nothing there runs the receiving end on a timer.
/// `run_session`'s `status_tx` is `None` on the console path, so nothing is
/// ever constructed or woken there. `Clone` (the `wake` closure is `Arc`,
/// not `Box`, for exactly this) so `run_session` can keep its own copy to
/// report [`SessionStatus::Ended`] after the one handed to
/// `network::run_video_link` has already been moved away.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone)]
pub(crate) struct StatusChannel {
    tx: std::sync::mpsc::Sender<SessionStatus>,
    // `Sync`, not just `Send`: an `Arc` (needed for `Clone`, see above) is
    // itself only `Send` if what it points to is `Send + Sync` — shared
    // ownership means another thread could in principle call through a
    // clone concurrently, even though nothing here actually does.
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl StatusChannel {
    pub(crate) fn new(tx: std::sync::mpsc::Sender<SessionStatus>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self { tx, wake: std::sync::Arc::new(wake) }
    }

    pub(crate) fn send(&self, status: SessionStatus) {
        let _ = self.tx.send(status);
        (self.wake)();
    }
}

/// Reads the PIN from the console — the console-path fallback used when
/// [`Args::pin`] wasn't given and either this isn't Windows or `--no-gui`
/// was passed.
fn prompt_pin() -> anyhow::Result<u32> {
    use std::io::Write;
    print!("\n  BacakOS ekranında gösterilen PIN'i girin: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    line.trim().parse::<u32>().map_err(|_| anyhow::anyhow!("geçersiz PIN girildi: {line:?}"))
}

/// [SINAMA/GEÇİCİ] `--test-h264-encode` yolu: capture thread'i başlatır,
/// `h264_qsv`'yi gerçek ekran boyutuyla açar, ~5 saniye gerçek kareyle
/// besler, sonucu konsola yazar ve çıkar. Senkron/blocking — bu sadece bir
/// donanım sınaması, tokio runtime'ı gerektiren gerçek oturuma hiç girmiyor.
/// Ekranın gerçekten Intel QSV üzerinden kodlandığını (yazılım fallback'e
/// düşmediğini) doğrulamak için Görev Yöneticisi'ndeki "Video Encode" GPU
/// sayacına bu 5 saniye boyunca bakın — bu araç kendi başına bunu ayırt
/// edemiyor (bkz. HARDWARE_ENCODE_PLAN.md madde 7).
#[cfg(windows)]
fn run_h264_encode_test(fps: u32) -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    println!("donanım H.264 kodlama sınaması başlıyor (nvenc/amf/qsv, sırayla denenir)...");
    let (_capture_thread, mut frame_rx) = capture::run_capture_thread(fps)?;
    let first = frame_rx.blocking_recv().ok_or_else(|| anyhow::anyhow!("capture thread hemen kapandı"))?;
    println!("ekran yakalandı: {}x{}", first.width, first.height);

    // 4 Mbps / 2s'de bir anahtar kare: bu sınama için makul, kalıcı olmayan
    // sabitler — gerçek CLI parametreleri madde 3'te eklenecek.
    let bitrate_bits_per_sec = 4_000_000;
    let keyframe_interval = fps.max(1) * 2;
    let mut encoder = encode_h264::H264Encoder::new(first.width, first.height, fps, bitrate_bits_per_sec, keyframe_interval)?;
    println!(
        "kodlayıcı açıldı: {} ({})",
        encoder.backend,
        if encoder.is_hardware { "DONANIM" } else { "yazılım, hiçbir donanım kodlayıcı açılamadı" }
    );

    let (mut frames_in, mut packets_out, mut keyframes, mut bytes_out) = (0u32, 0u32, 0u32, 0u64);
    let mut first_packet_is_keyframe: Option<bool> = None;
    let start = Instant::now();
    let mut pending = Some(first);

    while start.elapsed() < Duration::from_secs(5) {
        let frame = match pending.take() {
            Some(f) => f,
            None => match frame_rx.blocking_recv() {
                Some(f) => f,
                None => break,
            },
        };
        frames_in += 1;
        for packet in encoder.encode(&frame)? {
            if first_packet_is_keyframe.is_none() {
                first_packet_is_keyframe = Some(packet.is_keyframe);
            }
            if packet.is_keyframe {
                keyframes += 1;
            }
            bytes_out += packet.data.len() as u64;
            packets_out += 1;
        }
    }
    for packet in encoder.flush()? {
        bytes_out += packet.data.len() as u64;
        packets_out += 1;
        if packet.is_keyframe {
            keyframes += 1;
        }
    }
    let elapsed = start.elapsed().as_secs_f64().max(0.001);

    println!("--- sonuç ---");
    println!("girdi kare sayısı: {frames_in}");
    println!("çıktı paket sayısı: {packets_out}");
    println!("anahtar kare sayısı: {keyframes}");
    println!("ilk paket anahtar kare mi: {first_packet_is_keyframe:?}");
    println!("toplam çıktı: {bytes_out} bayt ({:.1} KB/s)", bytes_out as f64 / 1024.0 / elapsed);
    if packets_out == 0 {
        anyhow::bail!("encoder açıldı ama hiç paket üretmedi — beklenmeyen durum, araştırılmalı");
    }
    if encoder.is_hardware {
        println!("{} donanım kodlayıcısı çalışıyor. Bunun GERÇEKTEN donanımı kullandığını doğrulamak için Görev Yöneticisi > Performans > GPU > \"Video Encode\" sayacına bu çalışma sırasında bakmanız gerekiyor.", encoder.backend);
    } else {
        println!("Hiçbir donanım kodlayıcı (nvenc/amf/qsv) açılamadı, yazılım libx264'e düşüldü. Bu makinede donanım kodlama testi doğrulanamaz — GPU'su olan gerçek donanımda tekrar deneyin.");
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    // `.add_directive("info")` after `from_default_env()` would silently
    // override RUST_LOG's level (EnvFilter breaks ties between two equally
    // unscoped directives in favor of whichever was added last) — this
    // fallback only kicks in when RUST_LOG is unset/invalid, so RUST_LOG=debug
    // actually takes effect.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();

    #[cfg(windows)]
    if args.test_h264_encode {
        return run_h264_encode_test(args.fps);
    }

    // The graphical pairing window is the default on Windows: it owns the
    // PIN prompt itself and drives `run_session` on its own thread once
    // submitted (see `gui.rs`). `--pin`/`--no-gui` fall through to the
    // console path below instead — same one every non-Windows build always
    // uses, since `gui` doesn't exist there.
    #[cfg(windows)]
    if !args.no_gui && args.pin.is_none() {
        return gui::run(args);
    }

    let pin = match args.pin {
        Some(p) => p,
        None => prompt_pin()?,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_session(args, pin, None, None))
}

/// The encode task's other end of a [`network::FrameSource`] — see that
/// type's doc for why encoded frames need two different delivery policies
/// depending on the codec.
enum EncodeSink {
    Latest(tokio::sync::watch::Sender<Option<encode::EncodedFrame>>),
    Ordered(tokio::sync::mpsc::Sender<encode::EncodedFrame>),
}

impl EncodeSink {
    /// `false` means the network task's receiving end is gone (session
    /// over) — same meaning as the old `.send(..).is_err()` checks this
    /// replaced, just spelled the other way since `Ordered`'s `.await` is
    /// naturally a `bool`-returning check here rather than a `Result`.
    async fn send(&self, encoded: encode::EncodedFrame) -> bool {
        match self {
            EncodeSink::Latest(tx) => tx.send(Some(encoded)).is_ok(),
            EncodeSink::Ordered(tx) => tx.send(encoded).await.is_ok(),
        }
    }
}

/// The full pairing/streaming session: starts screen capture, binds the
/// sockets, and answers `PairRequest`s against `pin` until the process
/// exits, the link fails, or `shutdown_rx` (when given) is flipped to
/// `true` — the graphical pairing window's "Eşleşmeyi Bitir" button does
/// that to end a session without killing the whole process (see `gui.rs`).
/// `status_tx`, when given, receives pairing-progress notifications (see
/// [`SessionStatus`]) — the graphical pairing window forwards these to its
/// status label; the console path passes `None` for both.
pub(crate) async fn run_session(
    args: Args,
    pin: u32,
    status_tx: Option<StatusChannel>,
    shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> anyhow::Result<()> {
    let (_capture_thread, mut frame_rx) = capture::run_capture_thread(args.fps)?;

    // Peek the first frame synchronously to learn the real screen size before
    // any client has connected, so HelloAck can answer immediately.
    let first_frame = frame_rx.recv().await.ok_or_else(|| anyhow::anyhow!("capture thread exited immediately"))?;
    let (screen_width, screen_height) = (first_frame.width, first_frame.height);
    tracing::info!("capturing {screen_width}x{screen_height} at up to {} fps", args.fps);

    // `RawZstd` frames are independent, so a `watch` channel — "freshest
    // wins", the superseded frame is simply never sent if a newer one lands
    // before the network task got to the last one — is exactly right: it's
    // what stops the video from falling further and further behind the
    // longer a session runs, same "freshest wins" policy this codebase
    // already uses for capture (`scrap`'s next-new-frame semantics) and
    // decode (`FrameReassembler`/`RemoteDesktopSession::poll` on the BacakOS
    // side). H.264 delta frames are NOT independent — skipping one that way
    // breaks the next one's reference to it, which real-hardware testing
    // (2026-09-15) found causes multi-second freezes on ordinary scene
    // changes. So `--hardware-encode` uses a bounded `mpsc` instead (backs
    // the encode task off with real backpressure rather than silently
    // dropping) — see [`network::FrameSource`]'s doc for the full story.
    let zstd_level = args.zstd_level;
    #[cfg(windows)]
    let use_ordered_channel = args.hardware_encode;
    #[cfg(not(windows))]
    let use_ordered_channel = false;
    let (frame_source, encode_sink) = if use_ordered_channel {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        (network::FrameSource::Ordered(rx), EncodeSink::Ordered(tx))
    } else {
        let (tx, rx) = tokio::sync::watch::channel(None);
        (network::FrameSource::Latest(rx), EncodeSink::Latest(tx))
    };
    #[cfg(windows)]
    let hardware_encoder = if args.hardware_encode {
        // 4 Mbps / keyframe every ~1s: real-hardware end-to-end testing
        // (2026-09-15) found a big scene change (closing a window) could
        // stay frozen for ~10s — 5x the "~2s worst case" the original
        // `fps*2` GOP assumed. There's no NACK/retransmit (see
        // `FrameReassembler`'s `h264_awaiting_keyframe` doc in
        // `remote_desktop.rs`), so a lost keyframe access unit (bigger than
        // a delta frame, and thus more UDP chunks and more likely to lose
        // at least one) means waiting for the *next* one — a shorter GOP
        // halves that single-miss wait and, since losses are roughly
        // independent per attempt, makes several-in-a-row misses (which is
        // what actually produced the 10s freeze) considerably less likely.
        // Costs more bandwidth per second; not yet exposed as its own CLI
        // flag since this is a first real-hardware-informed adjustment, not
        // a tuned final value.
        let bitrate_bits_per_sec = 4_000_000;
        let keyframe_interval = args.fps.max(1);
        let encoder = encode_h264::H264Encoder::new(screen_width, screen_height, args.fps, bitrate_bits_per_sec, keyframe_interval)?;
        tracing::info!(
            "hardware encode: {} ({})",
            encoder.backend,
            if encoder.is_hardware { "hardware" } else { "software fallback — no hardware encoder opened" }
        );
        Some(encoder)
    } else {
        None
    };
    tokio::spawn(async move {
        #[cfg(windows)]
        let mut hardware_encoder = hardware_encoder;
        let mut frame_id: u32 = 0;
        let mut pending = Some(first_frame);
        loop {
            let frame = match pending.take() {
                Some(f) => f,
                None => match frame_rx.recv().await {
                    Some(f) => f,
                    None => {
                        tracing::info!("capture thread ended, stopping encoder");
                        return;
                    }
                },
            };

            #[cfg(windows)]
            if let Some(enc) = hardware_encoder.as_mut() {
                match enc.encode(&frame) {
                    Ok(packets) => {
                        for packet in packets {
                            let encoded = encode_h264::chunk_packet(packet, frame_id, frame.width, frame.height);
                            if !encode_sink.send(encoded).await {
                                return; // network task's receiver dropped
                            }
                            frame_id = frame_id.wrapping_add(1);
                        }
                    }
                    Err(e) => tracing::warn!("hardware encode failed: {e}"),
                }
                continue;
            }

            match encode::encode_frame(&frame, frame_id, zstd_level) {
                Ok(encoded) => {
                    if !encode_sink.send(encoded).await {
                        return; // network task's receiver dropped
                    }
                }
                Err(e) => tracing::warn!("encode failed for frame {frame_id}: {e}"),
            }
            frame_id = frame_id.wrapping_add(1);
        }
    });

    let session = network::new_shared_session();
    let video_socket = UdpSocket::bind(("0.0.0.0", args.video_port)).await?;
    let input_socket = UdpSocket::bind(("0.0.0.0", args.input_port)).await?;
    tracing::info!("listening: video={}, input={}", args.video_port, args.input_port);

    if let Some(tx) = &status_tx {
        let _ = tx.send(SessionStatus::WaitingForClient);
    }
    tracing::info!("pairing PIN accepted — waiting for the Bacak OS client to connect");

    let injector = Injector::new(screen_width, screen_height)?;

    // Kept separately from the clone handed to `run_video_link` below (which
    // owns *that* one for the rest of the session) so this function can
    // still report `Ended` once the session is over, however it ends.
    let status_tx_end = status_tx.clone();

    let mut video_task = tokio::spawn(network::run_video_link(video_socket, pin, screen_width, screen_height, frame_source, session.clone(), status_tx));
    let mut input_task = tokio::spawn(network::run_input_listener(input_socket, injector, session));

    let wait_for_shutdown = async move {
        let Some(mut rx) = shutdown_rx else {
            return std::future::pending::<()>().await;
        };
        while rx.changed().await.is_ok() {
            if *rx.borrow() {
                return;
            }
        }
    };

    tokio::select! {
        res = &mut video_task => res??,
        res = &mut input_task => res??,
        _ = wait_for_shutdown => tracing::info!("session ended: disconnect requested"),
    }
    // Whichever of the two tasks didn't win the select above is still
    // running (dropping its `JoinHandle` here wouldn't stop it) — abort it
    // so its socket/port is actually freed before this function returns.
    video_task.abort();
    input_task.abort();
    if let Some(tx) = &status_tx_end {
        tx.send(SessionStatus::Ended);
    }
    Ok(())
}
