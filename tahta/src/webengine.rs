//! Embedded web engine — a genuine, JS-capable web view rendered entirely
//! off-screen into an RGBA buffer, which `app.rs` uploads as a plain wgpu
//! texture through the same textured-quad pipeline PDF pages already use
//! (`renderer.rs`). No native window, no X11/XWayland, no GTK.
//!
//! An earlier attempt embedded WebKitGTK via `wry` (see the abandoned
//! `browser.rs`/`main.rs` X11-forcing changes in project history) and hit
//! a wall of native-level problems in order: `wry`'s WebKitGTK backend
//! only embeds into Xlib windows (forcing this whole app onto XWayland,
//! against its own low-latency-native-Wayland design), a GLXBadWindow
//! race from the reparenting trick, and finally a hard segfault inside
//! WebKitGTK's own `bmalloc` allocator colliding with this process's Rust
//! allocator. Servo sidesteps every one of those: it's pure Rust (no
//! separate C allocator to collide with) and paints into a
//! `SoftwareRenderingContext` that never touches a window system at all —
//! see `project_tahta_whiteboard` memory for the full comparison.

use dpi::PhysicalSize;
use euclid::{Box2D, Point2D};
use servo::{
    Code as ServoCode, Cursor, InputEvent, Key as ServoKey, KeyState, KeyboardEvent, LoadStatus,
    Location as ServoLocation, Modifiers as ServoModifiers, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, NamedKey as ServoNamedKey, NavigationRequest,
    RenderingContext, Servo, ServoBuilder, ServoDelegate, ServoError, SoftwareRenderingContext,
    TouchEvent, TouchEventType, TouchId, TouchPointerType, WebView, WebViewBuilder,
    WebViewDelegate, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use url::Url;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

/// Process-global Servo engine + the one software rendering context every
/// webview paints into. Servo keeps its startup config in a process-wide
/// singleton — building a second engine panics ("Already initialized") —
/// so construct exactly one of these and keep it for the app's lifetime.
pub struct WebEngine {
    servo: Servo,
    rendering_context: Rc<SoftwareRenderingContext>,
}

impl WebEngine {
    pub fn new(initial_size: (u32, u32)) -> Result<Self, String> {
        let size = PhysicalSize::new(initial_size.0.max(1), initial_size.1.max(1));
        let rendering_context = SoftwareRenderingContext::new(size)
            .map_err(|e| format!("SoftwareRenderingContext::new failed: {e:?}"))?;
        let rendering_context = Rc::new(rendering_context);
        rendering_context
            .make_current()
            .map_err(|e| format!("SoftwareRenderingContext::make_current failed: {e:?}"))?;

        let servo = ServoBuilder::default().build();
        servo.set_delegate(Rc::new(EngineDelegate));

        Ok(Self { servo, rendering_context })
    }

    pub fn new_panel(&self, url: &str) -> WebPanel {
        let ctx: Rc<dyn RenderingContext> = Rc::clone(&self.rendering_context) as _;
        WebPanel::new(&self.servo, ctx, url)
    }

    /// Pumps Servo's internal event loop. Call once per app frame,
    /// regardless of whether any panel is visible — cheap when idle.
    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }
}

struct EngineDelegate;

impl ServoDelegate for EngineDelegate {
    fn notify_error(&self, error: ServoError) {
        log::warn!("Servo engine error: {error:?}");
    }
}

#[derive(Clone, Copy)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

struct PanelState {
    needs_paint: Cell<bool>,
    current_cursor: Cell<Cursor>,
    load_status: Cell<LoadStatus>,
}

struct PanelDelegate {
    state: Rc<PanelState>,
}

impl WebViewDelegate for PanelDelegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.state.needs_paint.set(true);
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        self.state.current_cursor.set(cursor);
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        self.state.load_status.set(status);
    }

    fn request_navigation(&self, _webview: WebView, navigation_request: NavigationRequest) {
        navigation_request.allow();
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, backtrace: Option<String>) {
        log::error!("Servo webview crashed: {reason} ({backtrace:?})");
    }
}

/// One browsable panel: a `servo::WebView` plus the plumbing to pull its
/// latest painted frame out as plain RGBA bytes.
pub struct WebPanel {
    webview: WebView,
    rendering_context: Rc<dyn RenderingContext>,
    state: Rc<PanelState>,
    /// Cached so `renderer.rs` can skip re-uploading the texture when
    /// nothing new painted this frame — see `tick()`.
    last_frame: RefCell<Option<(Vec<u8>, u32, u32)>>,
}

impl WebPanel {
    fn new(servo: &Servo, rendering_context: Rc<dyn RenderingContext>, url: &str) -> Self {
        let state = Rc::new(PanelState {
            needs_paint: Cell::new(false),
            current_cursor: Cell::new(Cursor::Default),
            load_status: Cell::new(LoadStatus::Started),
        });
        let delegate = Rc::new(PanelDelegate { state: Rc::clone(&state) });

        let mut builder = WebViewBuilder::new(servo, Rc::clone(&rendering_context)).delegate(delegate);
        if let Ok(parsed) = Url::parse(url) {
            builder = builder.url(parsed);
        }
        let webview = builder.build();

        Self { webview, rendering_context, state, last_frame: RefCell::new(None) }
    }

    pub fn navigate(&self, url: &str) {
        if let Ok(parsed) = Url::parse(url) {
            self.webview.load(parsed);
        }
    }

    /// If Servo painted a new frame since the last call, reads it out and
    /// returns `(rgba, width, height)`. Returns `None` when nothing new is
    /// ready, so the caller can skip re-uploading an unchanged texture.
    pub fn tick(&self) -> Option<(Vec<u8>, u32, u32)> {
        if !self.state.needs_paint.replace(false) {
            return None;
        }
        if self.rendering_context.make_current().is_err() {
            return None;
        }
        self.webview.paint();

        // Read BEFORE present — `present()` swaps buffers and leaves the
        // new back buffer's contents undefined.
        let size = self.rendering_context.size();
        let rect = Box2D::new(
            Point2D::new(0, 0),
            Point2D::new(size.width as i32, size.height as i32),
        );
        let frame = self.rendering_context.read_to_image(rect).map(|rgba| {
            let (w, h) = (rgba.width(), rgba.height());
            (rgba.into_raw(), w, h)
        });
        self.rendering_context.present();

        if let Some(f) = &frame {
            *self.last_frame.borrow_mut() = Some(f.clone());
        }
        frame
    }

    pub fn resize(&self, size: (u32, u32)) {
        self.webview.resize(PhysicalSize::new(size.0.max(1), size.1.max(1)));
    }

    pub fn mouse_moved(&self, pos: (f32, f32)) {
        self.webview
            .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(web_point(pos))));
    }

    pub fn mouse_button(&self, pos: (f32, f32), down: bool) {
        let action = if down { MouseButtonAction::Down } else { MouseButtonAction::Up };
        self.webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
            action,
            MouseButton::Left,
            web_point(pos),
        )));
    }

    pub fn touch(&self, id: u64, pos: (f32, f32), phase: TouchPhase) {
        let event_type = match phase {
            TouchPhase::Started => TouchEventType::Down,
            TouchPhase::Moved => TouchEventType::Move,
            TouchPhase::Ended => TouchEventType::Up,
            TouchPhase::Cancelled => TouchEventType::Cancel,
        };
        // `TouchId` is i32; truncating a u64 finger id is fine in
        // practice (see `input_handler`'s own touch ids, same story).
        self.webview.notify_input_event(InputEvent::Touch(TouchEvent::new(
            event_type,
            TouchId(id as i32),
            web_point(pos),
            TouchPointerType::Touch,
        )));
    }

    pub fn wheel(&self, pos: (f32, f32), dx: f32, dy: f32) {
        self.webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
            WheelDelta { x: dx as f64, y: dy as f64, z: 0.0, mode: WheelMode::DeltaPixel },
            web_point(pos),
        )));
    }

    /// Forwards a winit key event to the page. `modifiers` has to be
    /// tracked by the caller (winit delivers it as a separate
    /// `WindowEvent::ModifiersChanged`, not on the key event itself).
    pub fn key_input(&self, event: &KeyEvent, modifiers: ModifiersState) {
        let state = match event.state {
            ElementState::Pressed => KeyState::Down,
            ElementState::Released => KeyState::Up,
        };
        let key = winit_key_to_servo(&event.logical_key);
        let code = winit_physical_to_servo(&event.physical_key);
        let location = winit_location_to_servo(event.location);
        let servo_modifiers = winit_modifiers_to_servo(modifiers);
        self.webview.notify_input_event(InputEvent::Keyboard(KeyboardEvent::new_without_event(
            state,
            key,
            code,
            location,
            servo_modifiers,
            event.repeat,
            false,
        )));
    }

    pub fn cursor(&self) -> Cursor {
        self.state.current_cursor.get()
    }

    pub fn load_status(&self) -> LoadStatus {
        self.state.load_status.get()
    }
}

fn web_point((x, y): (f32, f32)) -> WebViewPoint {
    WebViewPoint::from(Point2D::<f32, servo::CSSPixel>::new(x, y))
}

/// winit's `Key`/`NamedKey` and Servo's (`keyboard_types`) `Key`/`NamedKey`
/// both name variants after the W3C UI Events spec, so — same trick
/// `iced_servo` uses for its own (winit-shaped) key enum — round-tripping
/// through `Debug`/`FromStr` maps them without a hand-written table.
fn winit_key_to_servo(key: &Key) -> ServoKey {
    match key {
        Key::Character(s) => ServoKey::Character(s.to_string()),
        // W3C spec: Space is Key::Character(" "), not a named key — winit
        // reports it as Named::Space, so special-case it before parsing.
        Key::Named(NamedKey::Space) => ServoKey::Character(" ".into()),
        Key::Named(named) => format!("{named:?}")
            .parse::<ServoNamedKey>()
            .map(ServoKey::Named)
            .unwrap_or(ServoKey::Named(ServoNamedKey::Unidentified)),
        _ => ServoKey::Named(ServoNamedKey::Unidentified),
    }
}

fn winit_physical_to_servo(physical: &PhysicalKey) -> ServoCode {
    match physical {
        PhysicalKey::Code(code) => format!("{code:?}").parse::<ServoCode>().unwrap_or(ServoCode::Unidentified),
        PhysicalKey::Unidentified(_) => ServoCode::Unidentified,
    }
}

fn winit_location_to_servo(location: KeyLocation) -> ServoLocation {
    match location {
        KeyLocation::Standard => ServoLocation::Standard,
        KeyLocation::Left => ServoLocation::Left,
        KeyLocation::Right => ServoLocation::Right,
        KeyLocation::Numpad => ServoLocation::Numpad,
    }
}

fn winit_modifiers_to_servo(mods: ModifiersState) -> ServoModifiers {
    let mut out = ServoModifiers::empty();
    if mods.shift_key() {
        out |= ServoModifiers::SHIFT;
    }
    if mods.control_key() {
        out |= ServoModifiers::CONTROL;
    }
    if mods.alt_key() {
        out |= ServoModifiers::ALT;
    }
    if mods.super_key() {
        out |= ServoModifiers::META;
    }
    out
}
