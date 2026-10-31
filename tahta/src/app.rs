//! winit glue: turns window/pointer/touch/keyboard events into
//! [`InputHandler`] calls and asks the [`Renderer`] to draw each frame.
//! All drawing/tool/gesture logic lives in `input_handler` — this file
//! stays thin on purpose.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::KeyEvent;
use winit::window::Window;

use crate::input_handler::InputHandler;
use crate::renderer::Renderer;

/// Autosave once the board has been still this long after a change —
/// soon enough that little is ever lost, without writing mid-sentence.
const AUTOSAVE_QUIET_SECS: f64 = 2.0;

pub struct App {
    renderer: Renderer,
    window: Arc<Window>,
    start: Instant,
    input: InputHandler,
    /// An autosave is being written on its thread; the next one waits.
    saving: Arc<AtomicBool>,
    /// The last frame still had something moving — draw one more after it
    /// stops, so the final state (a laser mark gone, the toast hidden) shows.
    animating: bool,
}

impl App {
    pub fn new(renderer: Renderer, window: Arc<Window>, font_atlas: Arc<crate::font_atlas::FontAtlas>) -> Self {
        let size = window.inner_size();
        let screen_size = Vec2::new(size.width as f32, size.height as f32);
        Self {
            renderer,
            window,
            start: Instant::now(),
            input: InputHandler::new(screen_size, font_atlas),
            saving: Arc::new(AtomicBool::new(false)),
            animating: false,
        }
    }

    /// Work that runs every loop turn, frame or not (the autosave).
    pub fn background_tick(&mut self) {
        let now = self.now_secs();
        self.autosave(now);
    }

    /// Whether to keep drawing frames without waiting for input: something
    /// animates.
    pub fn needs_frame(&self) -> bool {
        self.animating || self.input.needs_animation(self.now_secs())
    }

    /// When the event loop should wake up by itself while idle — the
    /// pending autosave — so a board left alone still gets saved.
    pub fn next_wakeup(&self) -> Option<Instant> {
        let due = self.input.save_due_at(AUTOSAVE_QUIET_SECS)?;
        // Overdue but held back (a finger still drawing, a save in flight):
        // look again shortly instead of spinning.
        let due = due.max(self.now_secs() + 0.5);
        Some(self.start + Duration::from_secs_f64(due))
    }

    /// Load the board autosaved last time, if any. An unreadable save is
    /// set aside (`tahta.bin.bozuk`) rather than overwritten by the next
    /// autosave, so it can still be looked at.
    pub fn restore_autosave(&mut self) {
        let dir = crate::persist::autosave_dir();
        match crate::persist::load(&dir) {
            Ok(Some(board)) => {
                log::info!("önceki tahta yüklendi: {} sayfa", board.pages.len());
                self.input.restore_board(board);
            }
            Ok(None) => {}
            Err(e) => {
                log::error!("kayıtlı tahta açılamadı: {e:#}");
                let _ = std::fs::rename(dir.join("tahta.bin"), dir.join("tahta.bin.bozuk"));
            }
        }
    }

    /// Opening a PDF starts a new board; keep the autosaved one as the
    /// backup instead of letting the next autosave replace it.
    pub fn keep_autosave_as_backup(&self) {
        let (son, onceki) = (crate::persist::autosave_dir(), crate::persist::backup_dir());
        if son.join("tahta.bin").exists() {
            let _ = std::fs::remove_dir_all(&onceki);
            if let Err(e) = std::fs::rename(&son, &onceki) {
                log::warn!("önceki tahta yedeklenemedi: {e}");
            }
        }
    }

    /// Hand a changed board to a writer thread once it's been quiet a
    /// moment; and keep a board replaced by "New board" as the backup.
    fn autosave(&mut self, now: f64) {
        if let Some(old) = self.input.take_replaced_board() {
            std::thread::spawn(move || {
                if let Err(e) = crate::persist::save(&crate::persist::backup_dir(), &old) {
                    log::error!("önceki tahta yedeklenemedi: {e:#}");
                }
            });
        }
        if self.saving.load(Ordering::Acquire) {
            return;
        }
        if let Some(board) = self.input.take_save_request(now, AUTOSAVE_QUIET_SECS) {
            self.saving.store(true, Ordering::Release);
            let saving = self.saving.clone();
            std::thread::spawn(move || {
                if let Err(e) = crate::persist::save(&crate::persist::autosave_dir(), &board) {
                    log::error!("tahta kaydedilemedi: {e:#}");
                }
                saving.store(false, Ordering::Release);
            });
        }
    }

    /// Window closing: write any unsaved change now (after an in-flight
    /// autosave finishes, so the older board can't land last).
    pub fn save_before_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.saving.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if self.input.is_dirty() {
            if let Err(e) = crate::persist::save(&crate::persist::autosave_dir(), &self.input.board_data()) {
                log::error!("tahta kaydedilemedi: {e:#}");
            }
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
        let now = self.now_secs();
        self.input.mouse_released(now);
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
        let now = self.now_secs();
        self.input.touch_ended(id, now);
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
        self.animating = self.input.needs_animation(now);

        if self.input.take_minimize_request() {
            self.window.set_minimized(true);
        }

        let (normal_v, normal_i, highlight_v, highlight_i, page_image, content_index_count, glyph_v, glyph_i) = self.input.collect_geometry(now);

        match self.renderer.render(
            (&normal_v, &normal_i),
            (&highlight_v, &highlight_i),
            (&glyph_v, &glyph_i),
            content_index_count,
            page_image.as_deref(),
            self.input.magnifier_lens(),
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
