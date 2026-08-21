//! Tahta — ultra-low-latency GPU-accelerated whiteboard engine for
//! interactive flat panels.

mod app;
mod board;
mod brush;
mod input_handler;
mod palm;
mod prediction;
mod renderer;
mod stroke;
mod toolbar;
mod ui;

use std::sync::Arc;

use winit::event::{ElementState, Event, MouseButton, Touch, TouchPhase, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::platform::wayland::WindowBuilderExtWayland;
use winit::window::WindowBuilder;

use crate::app::App;
use crate::renderer::Renderer;

fn main() {
    env_logger::init();

    let event_loop = EventLoop::new().expect("failed to create event loop");
    // Poll continuously rather than waiting for OS events: paired with the
    // renderer's Mailbox/Immediate present mode, this keeps the pipeline
    // fed every frame instead of sleeping between input events, which is
    // what actually buys back touch-to-photon latency.
    event_loop.set_control_flow(ControlFlow::Poll);

    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Tahta — Dijital Beyaz Tahta")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            // Must match the `.desktop` file basename (tahta.desktop) so the
            // compositor's dock/app-menu app_id matching (dock_pinned) and
            // icon resolution find this window.
            .with_name("tahta", "tahta")
            .build(&event_loop)
            .expect("failed to create window"),
    );

    let renderer = pollster::block_on(Renderer::new(window.clone()));
    let mut app = App::new(renderer, window.clone());

    event_loop
        .run(move |event, elwt| {
            if let Event::WindowEvent { window_id, event } = event {
                if window_id != window.id() {
                    return;
                }
                match event {
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::Resized(physical_size) => app.resize(physical_size),
                    WindowEvent::ScaleFactorChanged { .. } => {
                        app.resize(window.inner_size());
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        app.mouse_moved(position);
                    }
                    WindowEvent::MouseInput { state, button, .. } => {
                        if button == MouseButton::Left {
                            match state {
                                ElementState::Pressed => app.mouse_pressed(),
                                ElementState::Released => app.mouse_released(),
                            }
                        }
                    }
                    WindowEvent::Touch(Touch { phase, location, id, .. }) => match phase {
                        TouchPhase::Started => app.touch_started(id, location),
                        TouchPhase::Moved => app.touch_moved(id, location),
                        TouchPhase::Ended | TouchPhase::Cancelled => app.touch_ended(id),
                    },
                    WindowEvent::KeyboardInput { event, .. } => app.key_input(&event),
                    WindowEvent::RedrawRequested => app.render(),
                    _ => {}
                }
            } else if let Event::AboutToWait = event {
                // Drive a new frame every loop iteration so predicted
                // (extrapolated) geometry stays fresh even between input
                // events.
                window.request_redraw();
            }
        })
        .expect("event loop error");
}
