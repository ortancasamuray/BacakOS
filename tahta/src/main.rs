//! Tahta — ultra-low-latency GPU-accelerated whiteboard engine for
//! interactive flat panels.

mod app;
mod board;
mod brush;
mod calculator;
mod dice;
mod digits;
mod expr;
mod font_atlas;
mod geom;
mod graph;
mod input_handler;
mod magnifier;
mod masaustu;
mod opener;
mod palm;
mod pdf;
mod pdf_export;
mod persist;
mod prediction;
mod protractor;
mod renderer;
mod ruler;
mod setsquare;
mod spotlight;
mod stopwatch;
mod stroke;
mod textbox;
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
    // Frames are drawn on demand: on input, and continuously (Poll) only
    // while something moves — a finger drawing, with the renderer's
    // Mailbox/Immediate present mode for touch-to-photon latency, a fading
    // laser mark. An idle board sleeps (see `AboutToWait`).
    event_loop.set_control_flow(ControlFlow::Wait);

    let window = Arc::new(
        WindowBuilder::new()
            .with_title("Tahta — Dijital Beyaz Tahta")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0))
            // Opens straight into fullscreen — a classroom whiteboard is
            // meant to fill the panel, not sit in a resizable window.
            // Borderless (not Exclusive/Fullscreen mode switch) is the
            // right choice under Wayland: it's the only variant most
            // Wayland compositors, including bacak-compositor, actually
            // implement. `None` picks the monitor the window opens on.
            .with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)))
            // Must match the `.desktop` file basename (tahta.desktop) so the
            // compositor's dock/app-menu app_id matching (dock_pinned) and
            // icon resolution find this window.
            .with_name("tahta", "tahta")
            .build(&event_loop)
            .expect("failed to create window"),
    );

    // Built once up front (rasterization, not per-frame) and shared: the
    // renderer uploads its pixel buffer to the GPU once, `input_handler`
    // (via `App`) uses its CPU-side glyph metrics every frame to lay out
    // the text box's real-font display and virtual keyboard.
    let font_atlas = Arc::new(crate::font_atlas::FontAtlas::new());

    let renderer = pollster::block_on(Renderer::new(window.clone(), &font_atlas));
    let mut app = App::new(renderer, window.clone(), font_atlas);

    // Opened "with" tahta from the file manager (Exec=tahta %f), or run
    // directly from a terminal with a path — either way, load it as a
    // fresh set of annotatable pages.
    // Otherwise pick up the board from last time (autosave).
    if let Some(path) = std::env::args().nth(1) {
        app.keep_autosave_as_backup();
        if let Err(e) = app.load_pdf(&path) {
            log::error!("PDF açılamadı ({path}): {e:#}");
        }
    } else {
        app.restore_autosave();
    }

    event_loop
        .run(move |event, elwt| {
            if let Event::WindowEvent { window_id, event } = event {
                if window_id != window.id() {
                    return;
                }
                // Any input may change what's on screen.
                if !matches!(event, WindowEvent::RedrawRequested) {
                    window.request_redraw();
                }
                match event {
                    WindowEvent::CloseRequested => {
                        app.save_before_exit();
                        elwt.exit();
                    }
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
                app.background_tick();
                if app.needs_frame() {
                    // Keep frames coming so predicted (extrapolated) stroke
                    // geometry stays fresh even between input events.
                    window.request_redraw();
                    elwt.set_control_flow(ControlFlow::Poll);
                } else {
                    elwt.set_control_flow(match app.next_wakeup() {
                        Some(at) => ControlFlow::WaitUntil(at),
                        None => ControlFlow::Wait,
                    });
                }
            }
        })
        .expect("event loop error");
}
