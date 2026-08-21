//! Event-to-pixel glue: turns winit pointer/touch/keyboard events into
//! stroke geometry, drives per-pointer input prediction, and asks the
//! renderer to draw each frame.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec2;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::KeyEvent;
use winit::keyboard::{Key, NamedKey};
use winit::window::Window;

use crate::prediction::{TouchPredictor, TouchSample};
use crate::renderer::Renderer;
use crate::stroke::{Stroke, Vertex};

/// How far ahead (ms) we extrapolate pointer motion to hide touch-to-photon
/// latency. 10-25ms covers one to a few frames at 60-120Hz.
const LOOKAHEAD_MS: f32 = 16.0;

const DEFAULT_WIDTH: f32 = 4.0;
const PALETTE: [[f32; 4]; 5] = [
    [0.92, 0.92, 0.95, 1.0], // white/chalk
    [0.95, 0.25, 0.25, 1.0], // red
    [0.25, 0.55, 0.95, 1.0], // blue
    [0.30, 0.85, 0.35, 1.0], // green
    [0.98, 0.78, 0.15, 1.0], // yellow
];

/// Sentinel pointer id for the mouse, kept out of the touch id space.
const MOUSE_POINTER_ID: u64 = u64::MAX;

struct PointerSession {
    stroke_index: usize,
    predictor: TouchPredictor,
}

pub struct App {
    renderer: Renderer,
    window: Arc<Window>,
    start: Instant,

    strokes: Vec<Stroke>,
    sessions: HashMap<u64, PointerSession>,
    last_mouse_pos: Vec2,

    active_color: [f32; 4],
    active_width: f32,

    // Scratch buffers reused every frame to avoid per-frame heap churn.
    scratch_vertices: Vec<Vertex>,
    scratch_indices: Vec<u32>,
}

impl App {
    pub fn new(renderer: Renderer, window: Arc<Window>) -> Self {
        Self {
            renderer,
            window,
            start: Instant::now(),
            strokes: Vec::new(),
            sessions: HashMap::new(),
            last_mouse_pos: Vec2::ZERO,
            active_color: PALETTE[0],
            active_width: DEFAULT_WIDTH,
            scratch_vertices: Vec::with_capacity(4096),
            scratch_indices: Vec::with_capacity(8192),
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        self.renderer.resize(new_size);
        self.window.request_redraw();
    }

    fn now_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn begin_pointer(&mut self, id: u64, position: Vec2) {
        let stroke_index = self.strokes.len();
        let mut stroke = Stroke::new(self.active_color, self.active_width);
        stroke.push_point(position);
        self.strokes.push(stroke);

        let mut predictor = TouchPredictor::new(LOOKAHEAD_MS);
        predictor.push_sample(TouchSample {
            position,
            timestamp: self.now_secs(),
        });

        self.sessions.insert(id, PointerSession { stroke_index, predictor });
    }

    fn move_pointer(&mut self, id: u64, position: Vec2) {
        let timestamp = self.now_secs();
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        self.strokes[session.stroke_index].push_point(position);
        session.predictor.push_sample(TouchSample { position, timestamp });
    }

    fn end_pointer(&mut self, id: u64) {
        self.sessions.remove(&id);
    }

    // --- Mouse -----------------------------------------------------------

    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let pos = Vec2::new(position.x as f32, position.y as f32);
        if self.sessions.contains_key(&MOUSE_POINTER_ID) {
            self.move_pointer(MOUSE_POINTER_ID, pos);
        }
        self.last_mouse_pos = pos;
    }

    pub fn mouse_pressed(&mut self) {
        self.begin_pointer(MOUSE_POINTER_ID, self.last_mouse_pos);
    }

    pub fn mouse_released(&mut self) {
        self.end_pointer(MOUSE_POINTER_ID);
    }

    // --- Touch -------------------------------------------------------------

    pub fn touch_started(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        self.begin_pointer(id, pos);
    }

    pub fn touch_moved(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        self.move_pointer(id, pos);
    }

    pub fn touch_ended(&mut self, id: u64) {
        self.end_pointer(id);
    }

    // --- Keyboard ----------------------------------------------------------

    pub fn key_input(&mut self, event: &KeyEvent) {
        if !event.state.is_pressed() {
            return;
        }
        match &event.logical_key {
            Key::Character(s) => match s.as_str() {
                "1" => self.active_color = PALETTE[0],
                "2" => self.active_color = PALETTE[1],
                "3" => self.active_color = PALETTE[2],
                "4" => self.active_color = PALETTE[3],
                "5" => self.active_color = PALETTE[4],
                "c" | "C" => self.strokes.clear(),
                _ => {}
            },
            Key::Named(NamedKey::Backspace) => {
                self.strokes.pop();
            }
            _ => {}
        }
    }

    // --- Rendering -----------------------------------------------------------

    pub fn render(&mut self) {
        self.scratch_vertices.clear();
        self.scratch_indices.clear();

        for (index, stroke) in self.strokes.iter().enumerate() {
            if stroke.is_empty() {
                continue;
            }

            // Only the stroke currently being drawn gets predicted
            // (transient) lookahead points appended; finished strokes
            // tessellate from their committed points alone.
            let predicted: Vec<Vec2> = self
                .sessions
                .values()
                .find(|s| s.stroke_index == index)
                .map(|s| s.predictor.predict())
                .unwrap_or_default();

            stroke.tessellate(&predicted, &mut self.scratch_vertices, &mut self.scratch_indices);
        }

        match self.renderer.render(&self.scratch_vertices, &self.scratch_indices) {
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
