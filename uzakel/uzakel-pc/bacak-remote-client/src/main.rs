mod decode;
mod input_capture;
mod network;
mod render;

use std::net::{IpAddr, SocketAddr};
use std::sync::mpsc;
use std::sync::Arc;

use clap::Parser;
use render::Renderer;
use winit::event::{DeviceEvent, Event, WindowEvent};
use winit::event_loop::EventLoop;
use winit::window::WindowBuilder;

use bacak_remote_proto::{DEFAULT_INPUT_PORT, DEFAULT_VIDEO_PORT};
use decode::DecodedFrame;

/// bacak-remote-client: renders a PC's streamed screen and forwards local
/// touch/pointer input back to it. Standalone winit window for now — see
/// `render.rs`'s module doc for the planned bacak-compositor plugin home.
#[derive(Parser, Debug)]
struct Args {
    /// IP address of the bacak-remote-server host.
    server_ip: IpAddr,
    /// Pairing PIN shown on the server's console when it started.
    pin: u32,
    #[arg(long, default_value_t = DEFAULT_VIDEO_PORT)]
    video_port: u16,
    #[arg(long, default_value_t = DEFAULT_INPUT_PORT)]
    input_port: u16,
    #[arg(long, default_value = "bacak-remote-client")]
    client_name: String,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();
    let args = Args::parse();

    let (frame_tx, frame_rx) = mpsc::channel::<DecodedFrame>();
    let server_video_addr = SocketAddr::new(args.server_ip, args.video_port);
    let client_name = args.client_name.clone();
    let input_cipher = network::new_shared_input_cipher();
    let pairing_input_cipher = input_cipher.clone();
    let pin = args.pin;
    std::thread::Builder::new().name("bacak-remote-net".into()).spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("build tokio runtime");
        if let Err(e) = runtime.block_on(network::run_video_receiver(server_video_addr, client_name, pin, frame_tx, pairing_input_cipher)) {
            tracing::error!("video receiver task ended: {e}");
        }
    })?;

    let input_socket = network::connect_input_socket(args.server_ip, args.input_port)?;

    let event_loop = EventLoop::new()?;
    let window = Arc::new(WindowBuilder::new().with_title("Bacak Remote").build(&event_loop)?);
    let mut renderer = pollster::block_on(Renderer::new(window.clone()))?;
    let mut window_size = window.inner_size();

    event_loop.run(move |event, elwt| match event {
        Event::WindowEvent { event, window_id } if window_id == window.id() => {
            tracing::debug!("window event: {event:?}");
            match event {
            WindowEvent::CloseRequested => elwt.exit(),
            WindowEvent::Resized(size) => {
                window_size = size;
                renderer.resize(size.width, size.height);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(ev) = input_capture::mouse_button_event(button, state) {
                    network::send_input(&input_socket, &input_cipher, ev);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                network::send_input(&input_socket, &input_cipher, input_capture::scroll_event(delta));
            }
            WindowEvent::Touch(touch) => {
                let ev = input_capture::touch_event(touch, window_size.width as f64, window_size.height as f64);
                network::send_input(&input_socket, &input_cipher, ev);
            }
            _ => {}
        }},
        Event::DeviceEvent { event: DeviceEvent::MouseMotion { delta: (dx, dy) }, .. } => {
            network::send_input(&input_socket, &input_cipher, bacak_remote_proto::InputEvent::PointerMotion { dx: dx as f32, dy: dy as f32 });
        }
        Event::DeviceEvent { event, .. } => {
            tracing::debug!("device event: {event:?}");
        }
        Event::AboutToWait => {
            let mut latest = None;
            while let Ok(frame) = frame_rx.try_recv() {
                latest = Some(frame); // coalesce: only the newest queued frame matters
            }
            if let Some(frame) = latest {
                renderer.update_frame(&frame);
            }
            match renderer.render() {
                Ok(()) => {}
                Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => renderer.resize(window_size.width, window_size.height),
                Err(e) => tracing::warn!("render error: {e}"),
            }
            window.request_redraw();
        }
        _ => {}
    })?;

    Ok(())
}
