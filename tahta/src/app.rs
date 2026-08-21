//! winit glue: turns window/pointer/touch/keyboard events into
//! [`InputHandler`] calls and asks the [`Renderer`] to draw each frame.
//! All drawing/tool/gesture logic lives in `input_handler` — this file
//! stays thin on purpose.

use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::KeyEvent;
use winit::window::Window;

use crate::input_handler::InputHandler;
use crate::renderer::Renderer;

pub struct App {
    renderer: Renderer,
    window: Arc<Window>,
    start: Instant,
    input: InputHandler,
}

impl App {
    pub fn new(renderer: Renderer, window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let screen_size = Vec2::new(size.width as f32, size.height as f32);
        Self {
            renderer,
            window,
            start: Instant::now(),
            input: InputHandler::new(screen_size),
        }
    }

    fn now_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        self.renderer.resize(new_size);
        self.input.resize(Vec2::new(new_size.width as f32, new_size.height as f32));
        self.window.request_redraw();
    }

    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let pos = Vec2::new(position.x as f32, position.y as f32);
        let now = self.now_secs();
        self.input.mouse_moved(pos, now);
    }

    pub fn mouse_pressed(&mut self) {
        let now = self.now_secs();
        self.input.mouse_pressed(now);
    }

    pub fn mouse_released(&mut self) {
        self.input.mouse_released();
    }

    pub fn touch_started(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        let now = self.now_secs();
        self.input.touch_started(id, pos, now);
    }

    pub fn touch_moved(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        let now = self.now_secs();
        self.input.touch_moved(id, pos, now);
    }

    pub fn touch_ended(&mut self, id: u64) {
        self.input.touch_ended(id);
    }

    pub fn key_input(&mut self, event: &KeyEvent) {
        let now = self.now_secs();
        self.input.key_input(event, now);
    }

    pub fn load_pdf(&mut self, path: &str) -> anyhow::Result<()> {
        self.input.load_pdf(path)?;
        self.window.request_redraw();
        Ok(())
    }

    pub fn render(&mut self) {
        let now = self.now_secs();
        self.input.tick(now);

        let (normal_v, normal_i, highlight_v, highlight_i, page_image) = self.input.collect_geometry(now);

        match self
            .renderer
            .render((&normal_v, &normal_i), (&highlight_v, &highlight_i), page_image.as_deref())
        {
            Ok(()) => {}
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                self.renderer.resize(self.window.inner_size());
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                log::error!("wgpu surface out of memory, exiting");
            }
            Err(e) => log::warn!("surface error: {e:?}"),
        }
    }
}
