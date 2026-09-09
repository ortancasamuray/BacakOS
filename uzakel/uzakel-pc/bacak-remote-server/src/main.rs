mod capture;
mod encode;
mod input_inject;
mod network;

use bacak_remote_proto::crypto::generate_pin;
use bacak_remote_proto::{DEFAULT_INPUT_PORT, DEFAULT_VIDEO_PORT};
use clap::Parser;
use input_inject::Injector;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

/// bacak-remote-server: streams this PC's screen to a Bacak OS client and
/// injects the input events it sends back. Requires the client to pair with
/// a PIN shown here before it accepts anything (see `bacak_remote_proto::crypto`).
#[derive(Parser, Debug)]
struct Args {
    /// UDP port carrying pairing/video frames.
    #[arg(long, default_value_t = DEFAULT_VIDEO_PORT)]
    video_port: u16,
    /// UDP port carrying inbound touch/pointer events.
    #[arg(long, default_value_t = DEFAULT_INPUT_PORT)]
    input_port: u16,
    /// Capture rate; encode/network time permitting.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// zstd compression level: higher = smaller frames, more CPU per frame.
    #[arg(long, default_value_t = 3)]
    zstd_level: i32,
    /// Fix the pairing PIN instead of generating a random one — for
    /// scripted testing only, never for a real session (a known PIN is no
    /// PIN at all).
    #[arg(long)]
    fixed_pin_for_testing: Option<u32>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();

    let (_capture_thread, mut frame_rx) = capture::run_capture_thread(args.fps)?;

    // Peek the first frame synchronously to learn the real screen size before
    // any client has connected, so HelloAck can answer immediately.
    let first_frame = frame_rx.recv().await.ok_or_else(|| anyhow::anyhow!("capture thread exited immediately"))?;
    let (screen_width, screen_height) = (first_frame.width, first_frame.height);
    tracing::info!("capturing {screen_width}x{screen_height} at up to {} fps", args.fps);

    let (encoded_tx, encoded_rx) = mpsc::channel(2);
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
                    if encoded_tx.send(encoded).await.is_err() {
                        return;
                    }
                }
                Err(e) => tracing::warn!("encode failed for frame {frame_id}: {e}"),
            }
            frame_id = frame_id.wrapping_add(1);
        }
    });

    let pin = args.fixed_pin_for_testing.unwrap_or_else(generate_pin);
    println!("\n  Pairing PIN: {pin:06}\n");
    tracing::info!("pairing PIN generated (shown above) — enter it on the Bacak OS client to connect");

    let session = network::new_shared_session();
    let video_socket = UdpSocket::bind(("0.0.0.0", args.video_port)).await?;
    let input_socket = UdpSocket::bind(("0.0.0.0", args.input_port)).await?;
    tracing::info!("listening: video={}, input={}", args.video_port, args.input_port);

    let injector = Injector::new(screen_width, screen_height)?;

    let video_task = tokio::spawn(network::run_video_link(video_socket, pin, screen_width, screen_height, encoded_rx, session.clone()));
    let input_task = tokio::spawn(network::run_input_listener(input_socket, injector, session));

    tokio::select! {
        res = video_task => res??,
        res = input_task => res??,
    }
    Ok(())
}
