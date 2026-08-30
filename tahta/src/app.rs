//! winit glue: turns window/pointer/touch/keyboard events into
//! [`InputHandler`] calls and asks the [`Renderer`] to draw each frame.
//! All drawing/tool/gesture logic lives in `input_handler` — this file
//! stays thin on purpose.
//!
//! The one exception is the embedded browser panel (`webengine`): since
//! it needs to hand wgpu-ready RGBA frames straight to the [`Renderer`]
//! and isn't drawn as our own vector geometry, its engine/webview and the
//! "is this event over the panel" routing live here instead of in
//! `input_handler`, which stays toolkit-agnostic on purpose.

use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use winit::window::Window;

use crate::input_handler::InputHandler;
use crate::renderer::Renderer;
use crate::webengine::{TouchPhase, WebEngine, WebPanel};

/// Home page for the embedded browser panel.
const BROWSER_HOME: &str = "https://www.google.com";

pub struct App {
    renderer: Renderer,
    window: Arc<Window>,
    start: Instant,
    input: InputHandler,
    web_engine: Option<WebEngine>,
    web_panel: Option<WebPanel>,
    /// Tracks `input.browser_visible` across frames so we only create the
    /// (lazily-initialized) engine/panel on the on-transition, not every
    /// frame while it's already open.
    browser_was_visible: bool,
    last_mouse_pos: Vec2,
    /// winit delivers modifier state as a separate event from the key
    /// press/release itself — tracked here so `key_input` can attach it
    /// when forwarding to the browser panel.
    modifiers: ModifiersState,
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
            web_engine: None,
            web_panel: None,
            browser_was_visible: false,
            last_mouse_pos: Vec2::ZERO,
            modifiers: ModifiersState::empty(),
        }
    }

    fn now_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// True if the browser panel is open and `pos` (screen pixels) lands
    /// on it — used to decide whether an event goes to Servo instead of
    /// the canvas/toolbar.
    fn point_in_browser(&self, pos: Vec2) -> bool {
        if !self.input.browser_visible {
            return false;
        }
        let (top_left, size) = self.input.browser_bounds();
        pos.x >= top_left.x
            && pos.y >= top_left.y
            && pos.x <= top_left.x + size.x
            && pos.y <= top_left.y + size.y
    }

    /// Screen-space `pos` to panel-local CSS-pixel coordinates.
    fn to_panel_local(&self, pos: Vec2) -> (f32, f32) {
        let (top_left, _) = self.input.browser_bounds();
        (pos.x - top_left.x, pos.y - top_left.y)
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        self.renderer.resize(new_size);
        self.input.resize(Vec2::new(new_size.width as f32, new_size.height as f32));
        self.window.request_redraw();
    }

    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let pos = Vec2::new(position.x as f32, position.y as f32);
        self.last_mouse_pos = pos;
        if self.point_in_browser(pos) {
            if let Some(panel) = &self.web_panel {
                panel.mouse_moved(self.to_panel_local(pos));
            }
            return;
        }
        let now = self.now_secs();
        self.input.mouse_moved(pos, now);
    }

    pub fn mouse_pressed(&mut self) {
        if self.point_in_browser(self.last_mouse_pos) {
            if let Some(panel) = &self.web_panel {
                panel.mouse_button(self.to_panel_local(self.last_mouse_pos), true);
            }
            return;
        }
        let now = self.now_secs();
        self.input.mouse_pressed(now);
    }

    pub fn mouse_released(&mut self) {
        if self.point_in_browser(self.last_mouse_pos) {
            if let Some(panel) = &self.web_panel {
                panel.mouse_button(self.to_panel_local(self.last_mouse_pos), false);
            }
            return;
        }
        self.input.mouse_released();
    }

    pub fn touch_started(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        if self.point_in_browser(pos) {
            if let Some(panel) = &self.web_panel {
                panel.touch(id, self.to_panel_local(pos), TouchPhase::Started);
            }
            return;
        }
        let now = self.now_secs();
        self.input.touch_started(id, pos, now);
    }

    pub fn touch_moved(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        if self.point_in_browser(pos) {
            if let Some(panel) = &self.web_panel {
                panel.touch(id, self.to_panel_local(pos), TouchPhase::Moved);
            }
            return;
        }
        let now = self.now_secs();
        self.input.touch_moved(id, pos, now);
    }

    pub fn touch_ended(&mut self, id: u64) {
        // We don't track per-touch-id "was this over the panel" state, so
        // just forward the lift to both — the canvas side is a no-op if
        // `id` never started a stroke there (see `InputHandler::touch_ended`).
        if let Some(panel) = &self.web_panel {
            panel.touch(id, (0.0, 0.0), TouchPhase::Ended);
        }
        self.input.touch_ended(id);
    }

    pub fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    pub fn key_input(&mut self, event: &KeyEvent) {
        // While the browser panel is open, keys go to the page instead of
        // this app's own hotkeys (pen/color/tool shortcuts) — otherwise
        // typing "b" or "c" into a web form would also cycle the brush or
        // clear the canvas.
        if self.input.browser_visible {
            if let Some(panel) = &self.web_panel {
                panel.key_input(event, self.modifiers);
            }
            return;
        }
        let now = self.now_secs();
        self.input.key_input(event, now);
    }

    pub fn load_pdf(&mut self, path: &str) -> anyhow::Result<()> {
        self.input.load_pdf(path)?;
        self.window.request_redraw();
        Ok(())
    }

    /// Creates the Servo engine + this panel on first toggle-on, hides it
    /// (without destroying it — session/history stays alive) on toggle-
    /// off, and keeps it sized to the panel's current bounds.
    fn sync_browser(&mut self) {
        let visible = self.input.browser_visible;
        if visible && !self.browser_was_visible {
            if self.web_engine.is_none() {
                let (_, size) = self.input.browser_bounds();
                match WebEngine::new((size.x as u32, size.y as u32)) {
                    Ok(engine) => self.web_engine = Some(engine),
                    Err(e) => log::error!("web engine oluşturulamadı: {e}"),
                }
            }
            if self.web_panel.is_none() {
                if let Some(engine) = &self.web_engine {
                    self.web_panel = Some(engine.new_panel(BROWSER_HOME));
                }
            }
        }
        self.browser_was_visible = visible;

        if let (true, Some(panel)) = (visible, &self.web_panel) {
            let (_, size) = self.input.browser_bounds();
            panel.resize((size.x as u32, size.y as u32));
        }
    }

    pub fn render(&mut self) {
        let now = self.now_secs();
        self.input.tick(now);
        self.sync_browser();

        if let Some(engine) = &self.web_engine {
            engine.spin();
        }
        let web_frame = if self.input.browser_visible {
            self.web_panel.as_ref().and_then(|p| p.tick())
        } else {
            None
        };
        let web_bounds = self.input.browser_visible.then(|| self.input.browser_bounds());

        let (normal_v, normal_i, highlight_v, highlight_i, page_image) = self.input.collect_geometry(now);

        match self.renderer.render(
            (&normal_v, &normal_i),
            (&highlight_v, &highlight_i),
            page_image.as_deref(),
            web_frame.as_ref().map(|(rgba, w, h)| (rgba.as_slice(), *w, *h)),
            web_bounds,
        ) {
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
