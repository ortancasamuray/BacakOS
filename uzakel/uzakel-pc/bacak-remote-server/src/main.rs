mod capture;
mod encode;
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
#[derive(Parser, Debug)]
pub(crate) struct Args {
    /// UDP port carrying pairing/video frames.
    #[arg(long, default_value_t = DEFAULT_VIDEO_PORT)]
    pub(crate) video_port: u16,
    /// UDP port carrying inbound touch/pointer events.
    #[arg(long, default_value_t = DEFAULT_INPUT_PORT)]
    pub(crate) input_port: u16,
    /// Capture rate; encode/network time permitting.
    #[arg(long, default_value_t = 60)]
    pub(crate) fps: u32,
    /// zstd compression level: higher = smaller frames (less bandwidth per
    /// frame), more CPU per frame. Raised from zstd's own default (3) since
    /// this pipeline has no real video codec/delta-encoding — every frame is
    /// a full raw-BGRA zstd blob, so compression ratio is the only lever
    /// available for keeping bandwidth within what the link can actually
    /// drain (see `run_session`'s `watch`-channel doc comment for the other
    /// half of that fix: not sending already-stale frames at all).
    #[arg(long, default_value_t = 9)]
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
}

/// Delivers a [`SessionStatus`] to whoever is listening and, if that
/// listener needs an explicit wake-up, triggers it — the pairing window's
/// Win32 message loop does (its `wake` closure calls an `nwg::NoticeSender`,
/// see `gui.rs`), since nothing there runs the receiving end on a timer.
/// `run_session`'s `status_tx` is `None` on the console path, so nothing is
/// ever constructed or woken there.
pub(crate) struct StatusChannel {
    tx: std::sync::mpsc::Sender<SessionStatus>,
    wake: Box<dyn Fn() + Send>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl StatusChannel {
    pub(crate) fn new(tx: std::sync::mpsc::Sender<SessionStatus>, wake: impl Fn() + Send + 'static) -> Self {
        Self { tx, wake: Box::new(wake) }
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
    runtime.block_on(run_session(args, pin, None))
}

/// The full pairing/streaming session: starts screen capture, binds the
/// sockets, and answers `PairRequest`s against `pin` until the process
/// exits. `status_tx`, when given, receives pairing-progress notifications
/// (see [`SessionStatus`]) — the graphical pairing window forwards these to
/// its status label; the console path passes `None`.
pub(crate) async fn run_session(args: Args, pin: u32, status_tx: Option<StatusChannel>) -> anyhow::Result<()> {
    let (_capture_thread, mut frame_rx) = capture::run_capture_thread(args.fps)?;

    // Peek the first frame synchronously to learn the real screen size before
    // any client has connected, so HelloAck can answer immediately.
    let first_frame = frame_rx.recv().await.ok_or_else(|| anyhow::anyhow!("capture thread exited immediately"))?;
    let (screen_width, screen_height) = (first_frame.width, first_frame.height);
    tracing::info!("capturing {screen_width}x{screen_height} at up to {} fps", args.fps);

    // `watch`, not `mpsc`: an mpsc queue (even a small bounded one) still
    // forces the network task to send *every* encoded frame's chunks in
    // order, including ones that are already stale by the time their turn
    // comes up if the link can't keep draining as fast as frames are
    // produced — that backlog of "chunks still queued to go out" is exactly
    // what made the video fall further and further behind the longer a
    // session ran. `watch` only ever holds the *latest* encoded frame: if a
    // newer one lands before the network task has picked up the last one,
    // the superseded one is simply never sent — same "freshest wins" policy
    // this codebase already uses for capture (`scrap`'s next-new-frame
    // semantics) and decode (`FrameReassembler`/`RemoteDesktopSession::poll`
    // on the BacakOS side), just extended to the one place it was missing.
    let (encoded_tx, encoded_rx) = tokio::sync::watch::channel(None);
    let zstd_level = args.zstd_level;
    tokio::spawn(async move {
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
            match encode::encode_frame(&frame, frame_id, zstd_level) {
                Ok(encoded) => {
                    if encoded_tx.send(Some(encoded)).is_err() {
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

    let video_task = tokio::spawn(network::run_video_link(video_socket, pin, screen_width, screen_height, encoded_rx, session.clone(), status_tx));
    let input_task = tokio::spawn(network::run_input_listener(input_socket, injector, session));

    tokio::select! {
        res = video_task => res??,
        res = input_task => res??,
    }
    Ok(())
}
