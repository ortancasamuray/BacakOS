//! winit glue: turns window/pointer/touch/keyboard events into
//! [`InputHandler`] calls and asks the [`Renderer`] to draw each frame.
//! All drawing/tool/gesture logic lives in `input_handler` — this file
//! stays thin on purpose.
//!
//! The one exception is the embedded browser panel (`webengine` +
//! `urlbar`): since it needs to hand wgpu-ready RGBA frames straight to
//! the [`Renderer`] and isn't drawn as our own vector geometry, its
//! engine/webview, address bar, and the "is this event over the panel"
//! routing live here instead of in `input_handler`, which stays
//! toolkit-agnostic on purpose.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::Window;

use crate::input_handler::InputHandler;
use crate::renderer::Renderer;
use crate::urlbar::{UrlBar, UrlBarHit};
use crate::webengine::{TouchPhase, WebEngine, WebPanel};

/// Home page for the embedded browser panel — Turkey's national education
/// platform, the expected default for a classroom whiteboard.
const BROWSER_HOME: &str = "https://www.eba.gov.tr";

/// Autosave once the board has been still this long after a change —
/// soon enough that little is ever lost, without writing mid-sentence.
const AUTOSAVE_QUIET_SECS: f64 = 2.0;

/// Where a screen-space point landed relative to the (open) browser
/// panel — decides whether an event goes to the address bar, the Servo
/// page, or (if outside the panel entirely) the canvas/toolbar as usual.
enum BrowserRegion {
    Bar,
    Content,
    Outside,
}

pub struct App {
    renderer: Renderer,
    window: Arc<Window>,
    start: Instant,
    input: InputHandler,
    web_engine: Option<WebEngine>,
    web_panel: Option<WebPanel>,
    url_bar: UrlBar,
    /// Tracks `input.browser_visible` across frames so we only create the
    /// (lazily-initialized) engine/panel on the on-transition, not every
    /// frame while it's already open.
    browser_was_visible: bool,
    /// When on, touches over the page draw ink (see `browser_region`)
    /// instead of reaching Servo — toggled from the address bar's pen
    /// button (`UrlBarHit::ToggleDraw`).
    browser_draw_mode: bool,
    last_mouse_pos: Vec2,
    /// winit delivers modifier state as a separate event from the key
    /// press/release itself — tracked here so `key_input` can attach it
    /// when forwarding to the browser panel.
    modifiers: ModifiersState,
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
            web_engine: None,
            web_panel: None,
            url_bar: UrlBar::new(BROWSER_HOME),
            browser_was_visible: false,
            browser_draw_mode: false,
            last_mouse_pos: Vec2::ZERO,
            modifiers: ModifiersState::empty(),
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
    /// animates, or the browser panel is open (Servo paints on its own).
    pub fn needs_frame(&self) -> bool {
        self.animating || self.input.browser_visible || self.input.needs_animation(self.now_secs())
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

    /// The Servo content area: the panel's full bounds minus the address
    /// bar at the top (a fixed height — editing it pops the compositor's
    /// own OSK, which doesn't live inside this window at all).
    fn browser_content_bounds(&self) -> (Vec2, Vec2) {
        let (top_left, size) = self.input.browser_bounds();
        let bar_h = self.url_bar.total_height();
        (top_left + Vec2::new(0.0, bar_h), Vec2::new(size.x, (size.y - bar_h).max(50.0)))
    }

    fn browser_region(&self, pos: Vec2) -> BrowserRegion {
        if !self.input.browser_visible {
            return BrowserRegion::Outside;
        }
        let (top_left, size) = self.input.browser_bounds();
        if self.url_bar.contains(top_left, size.x, pos) {
            return BrowserRegion::Bar;
        }
        let (c_top_left, c_size) = self.browser_content_bounds();
        if pos.x >= c_top_left.x
            && pos.y >= c_top_left.y
            && pos.x <= c_top_left.x + c_size.x
            && pos.y <= c_top_left.y + c_size.y
        {
            // Draw mode: the page area behaves like ordinary canvas (ink
            // lands in `input`'s current page/tool) instead of routing to
            // Servo — see `urlbar.rs`'s module docs.
            return if self.browser_draw_mode { BrowserRegion::Outside } else { BrowserRegion::Content };
        }
        BrowserRegion::Outside
    }

    /// Screen-space `pos` to Servo-content-local CSS-pixel coordinates.
    fn to_content_local(&self, pos: Vec2) -> (f32, f32) {
        let (top_left, _) = self.browser_content_bounds();
        (pos.x - top_left.x, pos.y - top_left.y)
    }

    fn handle_urlbar_hit(&mut self, hit: UrlBarHit) {
        match hit {
            UrlBarHit::Back => {
                self.window.set_ime_allowed(false);
                if let Some(panel) = &self.web_panel {
                    panel.go_back();
                }
            }
            UrlBarHit::Forward => {
                self.window.set_ime_allowed(false);
                if let Some(panel) = &self.web_panel {
                    panel.go_forward();
                }
            }
            UrlBarHit::FieldTapped => {
                let (top_left, size) = self.input.browser_bounds();
                let (field_pos, field_size) = crate::urlbar::UrlBar::field_rect(top_left, size.x);
                self.window.set_ime_cursor_area(
                    winit::dpi::PhysicalPosition::new(field_pos.x as i32, field_pos.y as i32),
                    winit::dpi::PhysicalSize::new(field_size.x as u32, field_size.y as u32),
                );
                self.window.set_ime_allowed(true);
            }
            UrlBarHit::Go(text) => {
                self.window.set_ime_allowed(false);
                if let Some(panel) = &self.web_panel {
                    panel.navigate(&resolve_input(&text));
                }
            }
            UrlBarHit::ToggleDraw => {
                self.browser_draw_mode = !self.browser_draw_mode;
            }
            UrlBarHit::None => {}
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        self.renderer.resize(new_size);
        self.input.resize(Vec2::new(new_size.width as f32, new_size.height as f32));
        self.window.request_redraw();
    }

    pub fn mouse_moved(&mut self, position: PhysicalPosition<f64>) {
        let pos = Vec2::new(position.x as f32, position.y as f32);
        self.last_mouse_pos = pos;
        match self.browser_region(pos) {
            BrowserRegion::Bar => return,
            BrowserRegion::Content => {
                if let Some(panel) = &self.web_panel {
                    panel.mouse_moved(self.to_content_local(pos));
                }
                return;
            }
            BrowserRegion::Outside => {}
        }
        let now = self.now_secs();
        self.input.mouse_moved(pos, now);
    }

    pub fn mouse_pressed(&mut self) {
        match self.browser_region(self.last_mouse_pos) {
            BrowserRegion::Bar => {
                let (top_left, size) = self.input.browser_bounds();
                let current_url = self.web_panel.as_ref().and_then(|p| p.url());
                let hit = self.url_bar.press_at(top_left, size.x, self.last_mouse_pos, current_url.as_deref());
                self.handle_urlbar_hit(hit);
                return;
            }
            BrowserRegion::Content => {
                if let Some(panel) = &self.web_panel {
                    panel.mouse_button(self.to_content_local(self.last_mouse_pos), true);
                }
                return;
            }
            BrowserRegion::Outside => {}
        }
        let now = self.now_secs();
        self.input.mouse_pressed(now);
    }

    pub fn mouse_released(&mut self) {
        if let BrowserRegion::Content = self.browser_region(self.last_mouse_pos) {
            if let Some(panel) = &self.web_panel {
                panel.mouse_button(self.to_content_local(self.last_mouse_pos), false);
            }
            return;
        }
        let now = self.now_secs();
        self.input.mouse_released(now);
    }

    pub fn touch_started(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        match self.browser_region(pos) {
            BrowserRegion::Bar => {
                let (top_left, size) = self.input.browser_bounds();
                let current_url = self.web_panel.as_ref().and_then(|p| p.url());
                let hit = self.url_bar.press_at(top_left, size.x, pos, current_url.as_deref());
                self.handle_urlbar_hit(hit);
                return;
            }
            BrowserRegion::Content => {
                if let Some(panel) = &self.web_panel {
                    panel.touch(id, self.to_content_local(pos), TouchPhase::Started);
                }
                return;
            }
            BrowserRegion::Outside => {}
        }
        let now = self.now_secs();
        self.input.touch_started(id, pos, now);
    }

    pub fn touch_moved(&mut self, id: u64, location: PhysicalPosition<f64>) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        match self.browser_region(pos) {
            BrowserRegion::Bar => return,
            BrowserRegion::Content => {
                if let Some(panel) = &self.web_panel {
                    panel.touch(id, self.to_content_local(pos), TouchPhase::Moved);
                }
                return;
            }
            BrowserRegion::Outside => {}
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
        let now = self.now_secs();
        self.input.touch_ended(id, now);
    }

    pub fn modifiers_changed(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    pub fn key_input(&mut self, event: &KeyEvent) {
        if self.input.browser_visible {
            if self.url_bar.editing {
                if event.state == ElementState::Pressed {
                    match &event.logical_key {
                        Key::Character(s) => {
                            for c in s.chars() {
                                self.url_bar.type_char(c);
                            }
                        }
                        Key::Named(NamedKey::Space) => self.url_bar.type_char(' '),
                        Key::Named(NamedKey::Backspace) => self.url_bar.backspace(),
                        Key::Named(NamedKey::Enter) => {
                            let text = self.url_bar.commit();
                            self.handle_urlbar_hit(UrlBarHit::Go(text)); // also hides the OSK
                        }
                        _ => {}
                    }
                }
                return;
            }
            // Keys go to the page instead of this app's own hotkeys
            // (pen/color/tool shortcuts) — otherwise typing "b" or "c"
            // into a web form would also cycle the brush or clear the
            // canvas.
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
    /// off, and keeps it sized to the panel's current content area.
    fn sync_browser(&mut self) {
        let visible = self.input.browser_visible;
        if visible && !self.browser_was_visible {
            if self.web_engine.is_none() {
                let (_, size) = self.browser_content_bounds();
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
        if !visible && self.browser_was_visible {
            if self.url_bar.editing {
                self.url_bar.cancel_editing();
                self.window.set_ime_allowed(false);
            }
            self.browser_draw_mode = false;
        }
        self.browser_was_visible = visible;

        if let (true, Some(panel)) = (visible, &self.web_panel) {
            let (_, size) = self.browser_content_bounds();
            panel.resize((size.x as u32, size.y as u32));
        }
    }

    pub fn render(&mut self) {
        let now = self.now_secs();
        self.input.tick(now);
        self.animating = self.input.needs_animation(now);
        self.sync_browser();

        if self.input.take_minimize_request() {
            self.window.set_minimized(true);
        }

        if let Some(engine) = &self.web_engine {
            engine.spin();
        }
        let web_frame = if self.input.browser_visible {
            self.web_panel.as_ref().and_then(|p| p.tick())
        } else {
            None
        };
        let web_bounds = self.input.browser_visible.then(|| self.browser_content_bounds());

        let (mut normal_v, mut normal_i, highlight_v, highlight_i, page_image, content_index_count, glyph_v, glyph_i) = self.input.collect_geometry(now);

        if self.input.browser_visible {
            // Passive sync: the field only pulled `panel.url()` on tap
            // before, so Back/Forward or an in-page link click left it
            // showing the stale address until the user tapped the field
            // again. Keep it live every frame instead — skipped while
            // `editing` so it doesn't clobber what the user is typing.
            if !self.url_bar.editing {
                if let Some(url) = self.web_panel.as_ref().and_then(|p| p.url()) {
                    let upper = url.to_uppercase();
                    if self.url_bar.text != upper {
                        self.url_bar.text = upper;
                    }
                }
            }
            let (panel_top_left, panel_size) = self.input.browser_bounds();
            let can_back = self.web_panel.as_ref().map(|p| p.can_go_back()).unwrap_or(false);
            let can_forward = self.web_panel.as_ref().map(|p| p.can_go_forward()).unwrap_or(false);
            self.url_bar.render(panel_top_left, panel_size.x, can_back, can_forward, self.browser_draw_mode, &mut normal_v, &mut normal_i);
        }

        match self.renderer.render(
            (&normal_v, &normal_i),
            (&highlight_v, &highlight_i),
            (&glyph_v, &glyph_i),
            content_index_count,
            page_image.as_deref(),
            web_frame.as_ref().map(|(rgba, w, h)| (rgba.as_slice(), *w, *h)),
            web_bounds,
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

/// Turns whatever the user typed into the address bar into a URL to
/// navigate to: passes through anything that already has a scheme,
/// treats a single dotted word as a bare domain, and otherwise treats it
/// as a search query (matches what every desktop browser's combined
/// address/search bar does).
fn resolve_input(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        return BROWSER_HOME.to_string();
    }
    let lower = t.to_lowercase();
    if lower.contains("://") {
        return lower;
    }
    if !lower.contains(' ') && lower.contains('.') {
        return format!("https://{lower}");
    }
    let q: String = url::form_urlencoded::byte_serialize(t.as_bytes()).collect();
    format!("https://www.google.com/search?q={q}")
}
