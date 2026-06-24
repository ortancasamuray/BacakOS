//! Composition pipeline: assemble a Z-ordered, styled element list for the
//! current frame.
//!
//! The job of this module is to turn the WM's logical state into something
//! the renderer can draw. Both the winit dev backend and the udev native
//! backend feed their `GlesRenderer` instance through here, so the visual
//! result is identical regardless of how the pixels reach the screen.
//!
//! # Layering rules (front → back in the returned `Vec`)
//!
//! Smithay's render-element convention is **front-first**: index `0` ends up
//! on top of the output, later indices fall behind. The list we hand back
//! is laid out this way:
//!
//! 1. **Snap preview overlay** — translucent landing-pad rectangle drawn
//!    over the screen so the user can see where a dragged window will snap
//!    to. Only present while a [`crate::grab::MoveGrab`] is active.
//! 2. **Active window** — every surface in the focused toplevel's tree, at
//!    `alpha = 1.0`.
//! 3. **Active window drop shadow** — a single solid quad slightly larger
//!    than the active rect, low alpha, painted just behind the active
//!    surface. A real Gaussian shadow would need its own blur pass; this
//!    stand-in gives enough depth cue for v1 (a follow-up milestone wires
//!    an offscreen blur shader).
//! 4. **Passive windows** — every other window, in descending z-order,
//!    at `alpha = 0.85`. The reduced alpha is the v1 stand-in for the
//!    full glassmorphism "blur behind". Real backdrop blur (a two-pass
//!    Gaussian kernel against the framebuffer behind the surface) needs a
//!    render-to-texture step that's not yet wired into the pipeline; see
//!    [`build_styled_elements`] for the TODO.

#![cfg(feature = "runtime")]

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::CropRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::element::PixelShaderElement;
use smithay::backend::renderer::gles::{
    GlesPixelProgram, GlesRenderer, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::{Bind, Color32F, Offscreen, Renderer};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::texture::TextureBuffer;
use smithay::utils::{Buffer as BufferCoord, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use std::time::Instant;

use crate::state::BacakState;
use crate::wm::{OutputId, Rect, WinState, WindowId, WorkspaceId};

/// Alpha applied to non-focused windows. 0.85 is the value the user asked
/// for; it's the alpha-only approximation of glassmorphism described at
/// the top of the module.
const PASSIVE_WINDOW_ALPHA: f32 = 0.85;

/// Drop-shadow geometry (in pixels) and tint. We render a single solid
/// rect; a future milestone replaces this with an offscreen blur.
const SHADOW_INSET: i32 = -12; // negative because we *expand* outward
const SHADOW_OFFSET_Y: i32 = 6;
const SHADOW_COLOR: Color32F = Color32F::new(0.0, 0.0, 0.0, 0.28);

/// Snap preview rectangle tint. We pick a saturated accent so the overlay
/// reads as "intent indicator" rather than "real surface".
const SNAP_PREVIEW_COLOR: Color32F = Color32F::new(0.20, 0.55, 0.95, 0.30);

// -- Task-switcher overlay tuning --------------------------------------------
//
// All values are in WM-logical pixels at scale 1; physical conversion goes
// through `to_physical_rect_offset` like every other overlay rect.

/// Tile dimensions for the alt+tab switcher. Squat-ish to read as a
/// thumbnail without burning vertical space.
const SWITCHER_TILE_W: f32 = 200.0;
const SWITCHER_TILE_H: f32 = 130.0;
/// Gap between adjacent tiles.
const SWITCHER_TILE_GAP: f32 = 16.0;
/// Padding around the tile row inside the backdrop.
const SWITCHER_PAD: f32 = 24.0;
/// How far the selection ring extends outside the active tile.
const SWITCHER_RING: f32 = 6.0;
/// Dark translucent panel behind the row.
const SWITCHER_BACKDROP_COLOR: Color32F = Color32F::new(0.04, 0.08, 0.12, 0.78);
/// Bright accent that frames the active tile.
const SWITCHER_RING_COLOR: Color32F = Color32F::new(0.36, 0.72, 1.0, 0.95);
/// Alpha applied to non-selected tiles.
const SWITCHER_TILE_ALPHA_DIM: f32 = 0.55;
/// Inner padding between the tile edge and the live thumbnail.
const SWITCHER_TILE_INNER_PAD: f32 = 6.0;
/// Height of the bottom band that carries the label, leaving the rest
/// of the tile for the thumbnail.
const SWITCHER_LABEL_BAND: f32 = 26.0;
/// Glyph height (px) for tile labels.
const SWITCHER_LABEL_PX: f32 = 18.0;
/// Inset of the label from the tile's left edge.
const SWITCHER_LABEL_INSET: f32 = 12.0;
/// Square display size of the app icon inside the label band.
const SWITCHER_ICON_SIZE: f32 = 18.0;
/// Gap between the icon and the start of the label text.
const SWITCHER_ICON_GAP: f32 = 8.0;
/// Near-white label colour; readable on every palette entry.
const SWITCHER_LABEL_RGB: [u8; 3] = [240, 244, 250];
/// Six-colour palette for tile fill. Each candidate hashes to one of
/// these so adjacent tiles read as distinct without text.
const SWITCHER_PALETTE: [(f32, f32, f32); 6] = [
    (0.92, 0.42, 0.42),
    (0.92, 0.74, 0.42),
    (0.74, 0.92, 0.42),
    (0.42, 0.92, 0.74),
    (0.42, 0.74, 0.92),
    (0.74, 0.42, 0.92),
];

/// Compositor dock: a translucent rounded bar pinned to the primary
/// output's bottom edge, one app-icon tile per window on its active
/// workspace. The panel is sized to the icon row plus uniform padding;
/// the reserved bottom strut covers the full `config.dock_height`
/// band so window snap math excludes the bar.
const DOCK_PANEL_COLOR: Color32F = Color32F::new(0.04, 0.08, 0.12, 0.72);
/// Softer radius than the switcher tiles so the long bar reads calm.
const DOCK_PANEL_RADIUS: f32 = 18.0;
/// Height (px) of the accent bar under the focused window's tile —
/// the classic "this one is foregrounded" running indicator.
const DOCK_INDICATOR_H: f32 = 3.0;
/// Fraction of a tile's width the indicator spans, centred.
const DOCK_INDICATOR_W_FRAC: f32 = 0.5;
/// Icon alpha for a minimised window — present but visibly dimmed so
/// the bar doubles as a "what's hidden" view.
const DOCK_MINIMIZED_FADE: f32 = 0.4;
/// Breathing-pulse period (s) for a pinned tile whose launch is in
/// flight, and the alpha range the icon oscillates between.
const DOCK_PULSE_SECS: f32 = 1.1;
const DOCK_PULSE_MIN: f32 = 0.35;
const DOCK_PULSE_MAX: f32 = 0.9;
/// Faint light chip drawn behind the hovered tile's icon.
const DOCK_HOVER_COLOR: Color32F = Color32F::new(1.0, 1.0, 1.0, 0.14);
const DOCK_HOVER_RADIUS: f32 = 8.0;
/// Hover tooltip: dark rounded plaque floating just above the bar.
const DOCK_TOOLTIP_BG: Color32F = Color32F::new(0.04, 0.08, 0.12, 0.92);
const DOCK_TOOLTIP_RADIUS: f32 = 8.0;

// Tier A text panel palette.
const SEL_PANEL_BG: Color32F = Color32F::new(0.10, 0.12, 0.16, 0.97);
const SEL_PANEL_RADIUS: f32 = 14.0;
const SEL_HIGHLIGHT: Color32F = Color32F::new(0.30, 0.55, 0.95, 0.42);
const SEL_HANDLE_COLOR: Color32F = Color32F::new(0.36, 0.60, 0.98, 1.0);
/// Inner padding (px) between the tooltip text and its plaque edge.
const DOCK_TOOLTIP_PAD: f32 = 8.0;
/// Vertical gap (px) between the plaque and the panel's top edge.
const DOCK_TOOLTIP_GAP: f32 = 8.0;
/// Multi-window count dots: one small square per window (capped),
/// drawn in the panel's *top* padding strip so they never collide
/// with the focus running-indicator at the bottom.
const DOCK_DOT_SIZE: f32 = 3.0;
const DOCK_DOT_GAP: f32 = 4.0;
const DOCK_MAX_DOTS: usize = 4;
const DOCK_DOT_COLOR: Color32F = Color32F::new(0.85, 0.9, 0.95, 0.9);
/// Attention/urgency cue colour — warm orange so it reads as an alert
/// distinct from the cool-blue focus accent.
const DOCK_URGENT_COLOR: Color32F = Color32F::new(1.0, 0.55, 0.15, 0.95);
/// Breathing-pulse period (s) for the urgency indicator.
const DOCK_URGENT_SECS: f32 = 0.9;
/// Drag-reorder insertion marker width (px).
const DOCK_INSERT_MARK_W: f32 = 3.0;
/// Hover-dwell window thumbnail: outer plaque dimensions and inner
/// padding around the surface preview. The plaque uses the same dark
/// tint + radius as the text tooltip for visual continuity.
const DOCK_THUMB_W: f32 = 240.0;
const DOCK_THUMB_H: f32 = 160.0;
const DOCK_THUMB_PAD: f32 = 8.0;

// `render_elements!` builds a tagged-union enum that satisfies the
// `RenderElement<R>` trait for every variant. That lets the renderer accept
// both client surfaces and our compositor-side decorations in one slice.
// Bound to `GlesRenderer` concretely (`<=GlesRenderer>`) rather than
// generic over `R`: `PixelShaderElement` only implements
// `RenderElement<GlesRenderer>`, and a generic-`R` enum would demand
// the bound for every `R`. Both backends only ever use the GLES
// renderer, so nothing is lost.
smithay::render_elements! {
    pub BacakElements<=GlesRenderer>;
    Surface = WaylandSurfaceRenderElement<GlesRenderer>,
    Solid = SolidColorRenderElement,
    Memory = MemoryRenderBufferRenderElement<GlesRenderer>,
    Pixel = PixelShaderElement,
    Texture = TextureRenderElement<GlesTexture>,
    // A surface element hard-clipped to a rectangle — used to confine an
    // Overview preview's surface tree (root + subsurfaces) to its card frame
    // so nothing can paint outside the frame or onto a neighbour card.
    CroppedSurface = CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    // The same hard-clip for our own (non-surface) overlay elements, so the
    // apps-menu grid can scroll by sub-row pixel amounts and still confine
    // every icon / label / hover chip to the grid viewport.
    CroppedMemory = CropRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>,
    CroppedSolid = CropRenderElement<SolidColorRenderElement>,
    CroppedPixel = CropRenderElement<PixelShaderElement>,
    CroppedTexture = CropRenderElement<TextureRenderElement<GlesTexture>>,
}

/// Hard-clip one of our overlay elements to the physical `crop`
/// rectangle. Used by the apps-menu grid so a partially-scrolled row is
/// trimmed at the viewport edge instead of spilling over the search bar
/// / panel. Returns `None` when `crop` excludes the element entirely
/// (smithay drops it). Non-grid variants pass through uncropped.
fn crop_element(el: BacakElements, scale: i32, crop: Rectangle<i32, Physical>) -> Option<BacakElements> {
    let s = smithay::utils::Scale::from(scale as f64);
    match el {
        BacakElements::Memory(e) => {
            CropRenderElement::from_element(e, s, crop).map(BacakElements::CroppedMemory)
        }
        BacakElements::Solid(e) => {
            CropRenderElement::from_element(e, s, crop).map(BacakElements::CroppedSolid)
        }
        BacakElements::Pixel(e) => {
            CropRenderElement::from_element(e, s, crop).map(BacakElements::CroppedPixel)
        }
        BacakElements::Texture(e) => {
            CropRenderElement::from_element(e, s, crop).map(BacakElements::CroppedTexture)
        }
        other => Some(other),
    }
}

// -- Rounded-card pixel shader -----------------------------------------------

/// Corner radius (px) of switcher cards.
const CARD_RADIUS: f32 = 12.0;
/// Drop-shadow softness (px) — the smoothstep falloff width.
const CARD_SHADOW_SOFT: f32 = 16.0;
/// Shadow offset (px). Light-from-above, so the card casts downward.
const CARD_SHADOW_OFF: [f32; 2] = [0.0, 6.0];
/// Symmetric margin baked around the card so the shader has room to
/// draw the offset, softened shadow without clipping.
const CARD_MARGIN: f32 = 24.0;
/// Selection accent border thickness (px); 0 disables it.
const CARD_BORDER: f32 = 3.0;
const CARD_SHADOW_COL: [f32; 4] = [0.0, 0.0, 0.0, 0.45];
/// The backdrop panel reads better with a softer, larger radius than
/// the individual tiles.
const BACKDROP_RADIUS: f32 = 18.0;
/// Corner radius of the thumbnail "window" punched into the card.
const THUMB_RADIUS: f32 = 8.0;
/// Soft-shadow corner radius for real (rectangular) windows.
const WINDOW_SHADOW_RADIUS: f32 = 6.0;

/// GLSL ES 1.00 fragment shader: a rounded rectangle with a soft drop
/// shadow and an optional selection border, all in one pass. `size`
/// and `alpha` are provided by Smithay; the rest are our uniforms.
/// Coordinates: `v_coords` is normalised [0,1] across `area`, so
/// `v_coords * size` is the pixel position within the element.
const CARD_SHADER_SRC: &str = r#"
precision mediump float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
// Output (HiDPI) scale. `size` is physical pixels but every geometric
// uniform below (card_min/max, radius, …) is in LOGICAL pixels, so we
// divide the fragment position by the scale and do all the rounded-rect
// maths in logical space — keeping the shader resolution-independent.
uniform float u_scale;
uniform vec2 card_min;
uniform vec2 card_max;
uniform float radius;
uniform float shadow_soft;
uniform vec2 shadow_off;
uniform vec4 card_col;
uniform vec4 shadow_col;
uniform float border;
uniform vec4 border_col;
// Optional rounded "window" punched out of the card. When
// `punch_radius` is negative the punch is disabled and the card
// behaves exactly as before. When enabled, fragments inside the
// punched rounded-rect are forced fully transparent so the element
// drawn behind (a window thumbnail) shows through with rounded
// corners — pixel-identical to the card everywhere else.
uniform vec2 punch_min;
uniform vec2 punch_max;
uniform float punch_radius;

// Signed distance from p to a rounded rect [c-b, c+b] with radius r.
float sd_round_rect(vec2 p, vec2 c, vec2 b, float r) {
    vec2 q = abs(p - c) - b + vec2(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2(0.0))) - r;
}

void main() {
    vec2 px = v_coords * size / max(u_scale, 1.0);
    vec2 c = (card_min + card_max) * 0.5;
    vec2 b = (card_max - card_min) * 0.5;

    // Shadow: same rounded rect, translated, with a wide soft edge.
    float sd_sh = sd_round_rect(px, c + shadow_off, b, radius);
    float sh_a = (1.0 - smoothstep(0.0, shadow_soft, sd_sh)) * shadow_col.a;

    // Card body: 1px anti-aliased edge.
    float sd_c = sd_round_rect(px, c, b, radius);
    float card_a = (1.0 - smoothstep(-1.0, 1.0, sd_c)) * card_col.a;

    // Composite shadow first, then the card over it.
    vec3 rgb = shadow_col.rgb;
    float a = sh_a;
    rgb = mix(rgb, card_col.rgb, card_a);
    a = card_a + sh_a * (1.0 - card_a);

    // Selection border: an inner ring along the card edge.
    if (border > 0.0) {
        float ring = step(-border, sd_c) * (1.0 - step(0.0, sd_c));
        rgb = mix(rgb, border_col.rgb, ring * border_col.a);
        a = max(a, ring * border_col.a);
    }

    // Punch a rounded window: inside it, this element is transparent
    // so the thumbnail behind shows with matching rounded corners.
    if (punch_radius >= 0.0) {
        vec2 pc = (punch_min + punch_max) * 0.5;
        vec2 pb = (punch_max - punch_min) * 0.5;
        float sd_p = sd_round_rect(px, pc, pb, punch_radius);
        float inside = 1.0 - smoothstep(-1.0, 1.0, sd_p);
        a *= (1.0 - inside);
    }

    gl_FragColor = vec4(rgb, a * alpha);
}
"#;

// Lazily-compiled card program, cached for the process lifetime.
// Outer `None` = not attempted; `Some(None)` = compile failed (don't
// retry every frame); `Some(Some(p))` = ready. The GLES context is
// the single render thread's, so a thread-local is sound and avoids
// threading the program through `BacakState`.
thread_local! {
    static CARD_PROGRAM: std::cell::RefCell<Option<Option<GlesPixelProgram>>> =
        const { std::cell::RefCell::new(None) };
}

/// Compile (once) and return the card shader program, or `None` if
/// compilation failed — callers then fall back to flat solid tiles.
fn card_program(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    CARD_PROGRAM.with(|cell| {
        if cell.borrow().is_none() {
            let uniforms = [
                UniformName::new("u_scale", UniformType::_1f),
                UniformName::new("card_min", UniformType::_2f),
                UniformName::new("card_max", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("shadow_soft", UniformType::_1f),
                UniformName::new("shadow_off", UniformType::_2f),
                UniformName::new("card_col", UniformType::_4f),
                UniformName::new("shadow_col", UniformType::_4f),
                UniformName::new("border", UniformType::_1f),
                UniformName::new("border_col", UniformType::_4f),
                UniformName::new("punch_min", UniformType::_2f),
                UniformName::new("punch_max", UniformType::_2f),
                UniformName::new("punch_radius", UniformType::_1f),
            ];
            let compiled = renderer
                .compile_custom_pixel_shader(CARD_SHADER_SRC, &uniforms)
                .map_err(|e| tracing::warn!(?e, "card shader compile failed; flat tiles"))
                .ok();
            *cell.borrow_mut() = Some(compiled);
        }
        cell.borrow().as_ref().and_then(|o| o.clone())
    })
}

// -- Software mouse cursor ---------------------------------------------------

/// Arrow-cursor silhouette, tip (hotspot) at the top-left `(0, 0)` pixel.
/// `X` is the white fill; every transparent pixel touching the fill gets a
/// 1px black outline (derived in [`build_cursor_rgba`]) so the cursor stays
/// visible over any background.
const CURSOR_FILL: &[&str] = &[
    "X...........",
    "XX..........",
    "XXX.........",
    "XXXX........",
    "XXXXX.......",
    "XXXXXX......",
    "XXXXXXX.....",
    "XXXXXXXX....",
    "XXXXXXXXX...",
    "XXXXXXXXXX..",
    "XXXXXXXXXXX.",
    "XXXXXXXXXXXX",
    "XXXXXXXX....",
    "XXXXXXXX....",
    "XXX..XXX....",
    "XX....XXX...",
    ".......XXX..",
    ".......XXX..",
    "........X...",
];

/// Rasterise [`CURSOR_FILL`] into RGBA8 in `Abgr8888` byte order (same
/// convention the icon path uses): white fill, derived black outline,
/// transparent everywhere else.
fn build_cursor_rgba() -> (Vec<u8>, i32, i32) {
    let h = CURSOR_FILL.len();
    let w = CURSOR_FILL.iter().map(|r| r.len()).max().unwrap_or(0);
    let fill = |x: i32, y: i32| -> bool {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return false;
        }
        CURSOR_FILL[y as usize].as_bytes().get(x as usize) == Some(&b'X')
    };
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let i = ((y as usize * w) + x as usize) * 4;
            if fill(x, y) {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]); // fill
            } else {
                // Outline: a transparent pixel adjacent to any fill pixel.
                let touches = (-1..=1).any(|dy| {
                    (-1..=1).any(|dx| (dx != 0 || dy != 0) && fill(x + dx, y + dy))
                });
                if touches {
                    rgba[i..i + 4].copy_from_slice(&[0, 0, 0, 255]); // border
                }
            }
        }
    }
    (rgba, w as i32, h as i32)
}

// Built once; the imported GPU texture is cached inside the buffer, so
// reusing the same instance across frames skips the per-frame upload.
// Single render thread → a thread-local is sound (same rationale as
// `CARD_PROGRAM`).
thread_local! {
    static CURSOR_BUFFER: std::cell::RefCell<Option<(MemoryRenderBuffer, i32, i32)>> =
        const { std::cell::RefCell::new(None) };
}

/// Build the cursor render elements for this frame, honouring the focused
/// client's `cursor_image` request:
/// * `Hidden` → nothing (e.g. a video player hiding the pointer).
/// * `Surface` → the client's own cursor surface, offset by its hotspot
///   so the active pixel lands exactly on the pointer position. This is
///   what lines the tip up with where clicks register and gives I-beam /
///   hand / resize cursors their correct shapes.
/// * `Named`/default → our built-in arrow.
///
/// Returned front-to-back; the caller pushes them first so they sit on top.
fn cursor_elements(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Vec<BacakElements> {
    let (gx, gy) = state.pointer_position;
    match &state.cursor_status {
        CursorImageStatus::Hidden => Vec::new(),
        CursorImageStatus::Surface(surface) => {
            let scale = output_scale as f64;
            // Hotspot = the surface pixel that should sit under the
            // pointer. Subtracting it is what fixes the tip-vs-click
            // offset for client-drawn cursors.
            let hotspot = smithay::wayland::compositor::with_states(surface, |states| {
                states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|m| m.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            let loc = smithay::utils::Point::<i32, smithay::utils::Physical>::from((
                (((gx - off_x as f64) - hotspot.x as f64) * scale).round() as i32,
                (((gy - off_y as f64) - hotspot.y as f64) * scale).round() as i32,
            ));
            render_elements_from_surface_tree(
                renderer,
                surface,
                loc,
                smithay::utils::Scale::from(scale),
                1.0,
                Kind::Cursor,
            )
        }
        CursorImageStatus::Named(_) => {
            arrow_cursor_element(state, renderer, output_scale, off_x, off_y)
                .into_iter()
                .collect()
        }
    }
}

/// Our built-in arrow cursor element, used when the focused client hasn't
/// set its own cursor surface (and over compositor chrome like the dock).
/// Tip/hotspot at the top-left `(0, 0)` pixel, drawn at the pointer
/// position in `output`-local physical pixels. `None` only if the GPU
/// import fails; off-output positions are clipped by the renderer.
fn arrow_cursor_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    let scale = output_scale as f32;
    let (gx, gy) = state.pointer_position;
    CURSOR_BUFFER.with(|cell| {
        if cell.borrow().is_none() {
            let (rgba, w, h) = build_cursor_rgba();
            let buffer = MemoryRenderBuffer::from_slice(
                &rgba,
                Fourcc::Abgr8888,
                (w, h),
                1,
                Transform::Normal,
                None,
            );
            *cell.borrow_mut() = Some((buffer, w, h));
        }
        let b = cell.borrow();
        let (buffer, w, h) = b.as_ref().unwrap();
        let phys = Point::<f64, Physical>::from((
            ((gx as f32 - off_x as f32) * scale) as f64,
            ((gy as f32 - off_y as f32) * scale) as f64,
        ));
        // Logical size override (= the buffer's native px). Smithay
        // multiplies it by the output scale, so on a HiDPI output the
        // cursor grows with everything else instead of staying tiny.
        // Pre-scaling here would square the scale (4× at scale 2).
        let dw = (*w).max(1) as i32;
        let dh = (*h).max(1) as i32;
        let elem = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            phys,
            buffer,
            None,
            None,
            Some(Size::<i32, smithay::utils::Logical>::from((dw, dh))),
            Kind::Unspecified,
        )
        .ok()?;
        Some(BacakElements::Memory(elem))
    })
}

/// Build the full frame element list for the active workspace on the
/// **primary** output, in front-to-back order. Single-output sessions
/// (the winit dev backend, headless tests) use this; the multi-output
/// udev backend calls [`build_styled_elements_for_output`] per target.
///
/// `output_scale` is the integer scale of the output we're rendering to —
/// `SolidColorRenderElement` expects geometry in physical pixels, so we
/// multiply through here.
pub fn build_styled_elements(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
) -> Vec<BacakElements> {
    let Some(primary) = state.wm.primary_output() else {
        return Vec::new();
    };
    build_styled_elements_for_output(state, renderer, output_scale, primary)
}

/// Per-output element list. All window positions are translated by
/// `-output.bounds.{x,y}` so the resulting elements live in the output's
/// own surface-local coordinate space — that's what `DrmCompositor`
/// expects. Surfaces that fall outside the output's bounds end up
/// off-screen and are clipped by the renderer.
///
/// Only windows on the workspace currently active on this output are
/// included; windows on inactive workspaces (or on other outputs)
/// don't reach this output's framebuffer.
///
/// If a workspace-switch slide is in progress for this output, *both*
/// the incoming and outgoing workspaces are rendered, offset on the
/// X axis by the slide's current `t` so the user sees them glide past
/// each other.
pub fn build_styled_elements_for_output(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    output: OutputId,
) -> Vec<BacakElements> {
    // Default path: full frame, no blur backdrop. Untouched by the
    // experimental blur pipeline.
    build_output_frame(state, renderer, output_scale, output, true, None)
}

/// Core frame builder. `include_switcher` lets the caller produce a
/// *scene-only* list (the offscreen source the blur samples from)
/// vs. the final list with the overlay on top. `blur_tex`, when
/// present, is a pre-blurred full-output texture the switcher backdrop
/// samples instead of drawing a flat/card panel — real frosted glass.
pub fn build_output_frame(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    output: OutputId,
    include_switcher: bool,
    blur_tex: Option<&GlesTexture>,
) -> Vec<BacakElements> {
    let mut out: Vec<BacakElements> = Vec::new();

    let Some(o) = state.wm.output(output) else { return out };
    let off_x = o.bounds.x as i32;
    let off_y = o.bounds.y as i32;

    let Some(active_ws) = state.wm.active_workspace_on(output) else {
        return out;
    };

    let slide = state.workspace_slides.get(&output);

    // Software mouse cursor. Pushed first so it lands at index 0 — the
    // topmost element — floating over windows, the dock and even the task
    // switcher. Only on the final frame (`include_switcher`); the blur
    // source is built without it. Off-output positions fall outside
    // `o.bounds` and get clipped, so adding it to every output is safe.
    // Post-screenshot camera flash: a white overlay over the whole output that
    // fades out fast. Pushed first → topmost (even over the cursor). Never in a
    // capture: `flash` is set *after* the offscreen capture renders, so this
    // output's capture frame saw `flash == None`. Skipped on the blur source.
    if include_switcher {
        if let Some(a) = state.flash_alpha(output) {
            out.push(solid_element(
                o.bounds,
                Color32F::new(1.0, 1.0, 1.0, a),
                output_scale,
                off_x,
                off_y,
            ));
        }
    }

    if include_switcher {
        for cursor in cursor_elements(state, renderer, output_scale, off_x, off_y) {
            out.push(cursor);
        }
    }

    // Interactive region-screenshot selection rectangle (Shift+PrintScreen):
    // a bright border + faint fill over the drag area. Drawn just under the
    // cursor (above brightness/windows) so it stays crisp while selecting.
    // Only present while `region_shot` is live — the capture happens after
    // release (region_shot cleared), so the saved PNG never shows this overlay.
    if include_switcher {
        if let Some(rs) = state.region_shot {
            if let Some(r) = rs.rect() {
                let bt = 2.0_f32;
                let border = SWITCHER_RING_COLOR;
                // Borders first (topmost), translucent fill beneath them.
                out.push(solid_element(Rect::new(r.x, r.y, r.w, bt), border, output_scale, off_x, off_y));
                out.push(solid_element(Rect::new(r.x, r.y + r.h - bt, r.w, bt), border, output_scale, off_x, off_y));
                out.push(solid_element(Rect::new(r.x, r.y, bt, r.h), border, output_scale, off_x, off_y));
                out.push(solid_element(Rect::new(r.x + r.w - bt, r.y, bt, r.h), border, output_scale, off_x, off_y));
                out.push(solid_element(r, Color32F::new(0.20, 0.55, 0.95, 0.18), output_scale, off_x, off_y));
            }
        }
    }

    // Window-pick screenshot (Alt+PrintScreen): highlight the window under the
    // pointer so the user sees what a click will capture. Same border style as
    // the region overlay; cleared before the capture so the PNG stays clean.
    if include_switcher && state.window_pick {
        let (px, py) = (state.pointer_position.0 as f32, state.pointer_position.1 as f32);
        if let Some(r) = state.wm.hit_test(px, py).and_then(|id| state.window_shot_rect(id)) {
            let bt = 2.0_f32;
            let border = SWITCHER_RING_COLOR;
            out.push(solid_element(Rect::new(r.x, r.y, r.w, bt), border, output_scale, off_x, off_y));
            out.push(solid_element(Rect::new(r.x, r.y + r.h - bt, r.w, bt), border, output_scale, off_x, off_y));
            out.push(solid_element(Rect::new(r.x, r.y, bt, r.h), border, output_scale, off_x, off_y));
            out.push(solid_element(Rect::new(r.x + r.w - bt, r.y, bt, r.h), border, output_scale, off_x, off_y));
            out.push(solid_element(r, Color32F::new(0.20, 0.55, 0.95, 0.18), output_scale, off_x, off_y));
        }
    }

    // Software display brightness. There's no hardware backlight here, so
    // dim the whole output with a translucent black overlay scaled by how
    // far below full brightness we are. Pushed right after the cursor so
    // the pointer stays readable while everything else (including the
    // Control Center) dims, like a real backlight. Skipped on the blur
    // source pass and at full brightness.
    if include_switcher && state.brightness < 0.999 {
        let dim = (1.0 - state.brightness).clamp(0.0, 1.0 - crate::state::MIN_BRIGHTNESS);
        out.push(solid_element(
            o.bounds,
            Color32F::new(0.0, 0.0, 0.0, dim),
            output_scale,
            off_x,
            off_y,
        ));
    }

    // Text-selection + IME overlays (floating menu / native panel / AT-SPI /
    // IME popups) are now the `SelectionPlugin` (top-most overlay `z`), and the
    // Overview ("recents") is `OverviewPlugin` — both drawn by `render_overlays`
    // below, above the windows + dock.

    // 0. Task-switcher overlay sits on top of everything else, including
    // the snap preview, so the user can always see which window will
    // gain focus on alt-release. It's pinned to whichever output the
    // pointer was on when the cycle started (captured in `start_cycle`),
    // so it stays put even if the pointer drifts while Alt is held.
    // Skipped when building the blur source — we want to blur what's
    // *behind* the overlay, not the overlay itself.
    if include_switcher && state.switcher_output() == Some(output) {
        render_task_switcher(
            state,
            renderer,
            output_scale,
            o.bounds,
            off_x,
            off_y,
            blur_tex,
            &mut out,
        );
    }

    // 0b. Shell overlays — dock, on-screen keyboard, apps-menu, Control Center,
    // screenshot dialog/toast — are drawn by the **plugin registry**, layered
    // top-most-first by each plugin's `z()` (modals above the keyboard above the
    // dock, so the OSK's bottom keys overlap the dock cleanly). Above the
    // windows, below the task switcher; skipped on the blur source pass. See
    // `crate::plugins`.
    if include_switcher {
        crate::plugins::render_overlays(
            state, renderer, output, output_scale, off_x, off_y, &mut out,
        );
    }

    // 1. Snap preview overlay. Suppressed during a slide — the overlay
    // would otherwise leak onto the incoming workspace where no drag
    // is happening.
    if slide.is_none() {
        if let Some((_zone, rect)) = state.snap_preview {
            out.push(BacakElements::Solid(SolidColorRenderElement::new(
                Id::new(),
                to_physical_rect_offset(rect, output_scale, off_x, off_y),
                0usize, // commit counter — any monotonic value works for solids
                SNAP_PREVIEW_COLOR,
                Kind::Unspecified,
            )));
        }
    }

    // 1b. Override-redirect X11 surfaces (menus, tooltips, combo
    // dropdowns). They position themselves in global logical space and
    // bypass the WM, so they're drawn here — above the app windows, below
    // the dock / overlays — at their own absolute geometry.
    render_x11_override(state, renderer, output_scale, off_x, off_y, &mut out);

    // 1c. wlr-layer-shell Overlay + Top layers (panels, docks, notifiers).
    // Above the app windows, below the compositor's own chrome (dock/overview/
    // switcher pushed earlier). Overlay before Top so it stacks higher.
    render_layer_set(
        state,
        renderer,
        output,
        output_scale,
        &[
            smithay::wayland::shell::wlr_layer::Layer::Overlay,
            smithay::wayland::shell::wlr_layer::Layer::Top,
        ],
        &mut out,
    );

    // Server-side title bars are drawn per-window inside
    // `render_workspace_windows` (interleaved with content for correct z-order,
    // and sliding with the workspace).

    // 2-4. Windows. If a slide is in flight, render both the outgoing
    // and incoming workspaces with their respective pixel offsets; the
    // springs settle to (0, 1) so the seam closes naturally. Otherwise,
    // just paint the active workspace at its normal position.
    if let Some(slide) = slide {
        let t = slide.t() as f32;
        let w = o.bounds.w;
        // `dx` here is the *extra* pixel offset applied per-workspace
        // (added to the window's render x). At `t = 0` the previous
        // workspace sits at home (`dx = 0`) and the new one is offscreen
        // (`dx = ±w`); at `t = 1` they've swapped.
        let prev_dx = (-slide.direction * t * w) as i32;
        let new_dx = (slide.direction * (1.0 - t) * w) as i32;
        // Suppress the frosted backdrop during a slide: the windows are
        // translating but the blur texture is static, so the frost
        // wouldn't track. The transition is brief; crisp is fine.
        render_workspace_windows(
            state,
            renderer,
            output_scale,
            slide.prev_ws,
            off_x - prev_dx,
            off_y,
            None,
            &mut out,
        );
        render_workspace_windows(
            state,
            renderer,
            output_scale,
            active_ws,
            off_x - new_dx,
            off_y,
            None,
            &mut out,
        );
    } else {
        // Edge rubber-band: when a 3-finger swipe pushes past the first /
        // last workspace there's no neighbour to slide in, so the active
        // workspace shifts a little in the swipe direction and springs back.
        let bounce = state.ws_bounce_offset(output);
        render_workspace_windows(
            state,
            renderer,
            output_scale,
            active_ws,
            off_x + bounce,
            off_y,
            blur_tex,
            &mut out,
        );
    }

    // 5. wlr-layer-shell Bottom + Background layers (wallpaper, below-window
    // panels). Pushed last so they sit behind every window. Background last of
    // all → backmost.
    render_layer_set(
        state,
        renderer,
        output,
        output_scale,
        &[
            smithay::wayland::shell::wlr_layer::Layer::Bottom,
            smithay::wayland::shell::wlr_layer::Layer::Background,
        ],
        &mut out,
    );

    // Compositor-native wallpaper: a solid colour pushed after every layer-shell
    // Compositor-native wallpaper — rendered when no Background layer-shell
    // client (swaybg etc.) has registered a surface on this output.
    let has_bg_client = {
        use smithay::wayland::shell::wlr_layer::Layer;
        state.outputs.get(&output).is_some_and(|sout| {
            smithay::desktop::layer_map_for_output(sout)
                .layers_on(Layer::Background)
                .next()
                .is_some()
        })
    };
    if !has_bg_client {
        // In Bacak's element list first-pushed = topmost, last-pushed = furthest
        // back. The wallpaper sits behind every window and layer-shell surface
        // (all pushed earlier in this function), so we push it last.
        // Image renders on top of the solid-colour fallback → push image first,
        // then solid colour.

        // Image wallpaper (optional): blitted from the pre-scaled cache.
        let ow = o.bounds.w as u32;
        let oh = o.bounds.h as u32;
        if let Some(buf) = state.wallpaper_cache.get(&(ow, oh)) {
            use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
            use smithay::backend::renderer::element::Kind;
            use smithay::utils::Point;
            use smithay::utils::Physical;
            let phys = Point::<f64, Physical>::from((off_x as f64, off_y as f64));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
            ) {
                out.push(BacakElements::Memory(el));
            }
        }

        // Solid colour fallback (furthest back, always present).
        let [r, g, b] = state.config.wallpaper_color;
        out.push(solid_element(
            o.bounds,
            Color32F::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0),
            output_scale,
            off_x,
            off_y,
        ));
    }

    out
}

/// Draw the compositor dock: an app-icon tile per dock slot —
/// pinned launchers first, then running windows on the primary's
/// active workspace — sitting on a translucent rounded panel pinned to
/// the bottom edge. The slots come from [`BacakState::dock_tiles`], the
/// single source of truth shared with click routing and the
/// minimise/restore animation so the bar and the "fly to dock" target
/// always agree.
///
/// Push order is front-first (earlier = on top), like the switcher
/// backdrop: icons + the focus indicator first, then the panel last so
/// it sits *behind* them. A window with no resolvable icon still gets a
/// coloured chip so its slot stays visible and clickable.
///
/// Phase 3 — tile state feedback: the focused window gets an accent
/// running-indicator bar under its tile, and a minimised window's icon
/// is drawn dimmed so the bar reads as both a switcher and a "what's
/// hidden" view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_dock(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    // Auto-hidden and fully tucked away → nothing to draw on this
    // output's dock.
    if state.dock_fully_hidden_for(output) {
        return;
    }
    // Right-click context menu floats above everything else on the
    // bar (and above tooltip/thumbnail), so push it first.
    render_dock_menu(state, renderer, output, output_scale, off_x, off_y, out);

    // Auto-hide reveal: shift every dock element away from the
    // configured edge by *this* output's spring offset. Elements are
    // placed at `(coord - off)`, so:
    //   • Bottom edge → smaller `off_y` moves elements down.
    //   • Top edge    → larger  `off_y` moves them up.
    //   • Left  edge  → larger  `off_x` moves them left.
    //   • Right edge  → smaller `off_x` moves them right.
    // Subtract the offset once here and the whole bar slides together.
    let reveal = state.dock_reveal_pos(output).round() as i32;
    let (off_x, off_y) = match state.config.dock_edge {
        crate::config::DockEdge::Bottom => (off_x, off_y - reveal),
        crate::config::DockEdge::Top => (off_x, off_y + reveal),
        crate::config::DockEdge::Left => (off_x + reveal, off_y),
        crate::config::DockEdge::Right => (off_x - reveal, off_y),
    };

    let tiles = state.dock_tiles_for(output);
    let (Some(first), Some(last)) = (
        tiles.first().map(|e| e.rect),
        tiles.last().map(|e| e.rect),
    ) else {
        return;
    };

    // Panel = the icon row grown by the same padding the layout used.
    let pad = (state.config.dock_height * 0.18).max(2.0);
    let panel = Rect::new(
        first.x - pad,
        first.y - pad,
        (last.x + last.w - first.x) + 2.0 * pad,
        first.h + 2.0 * pad,
    );

    // Which tile is under the pointer? Same hit-test as `dock_click`
    // (pointer position is WM-global, matching the tile rects).
    let (px, py) = state.pointer_position;
    let (px, py) = (px as f32, py as f32);
    let hover = tiles.iter().position(|e| {
        let r = e.rect;
        px >= r.x && px <= r.x + r.w && py >= r.y && py <= r.y + r.h
    });

    // Tooltip / thumbnail first → either floats on top of the bar
    // (front-first push). Once the dwell timer elapses on a running
    // window's tile, the text plaque upgrades to a live preview.
    if let Some(hi) = hover {
        let e = &tiles[hi];
        let (anchor_x, anchor_y) =
            (e.rect.x + e.rect.w / 2.0, e.rect.y + e.rect.h / 2.0);
        let thumb_ready = state.dock_thumbnail_ready() == Some((output, hi));
        if let (true, Some(id)) = (thumb_ready, e.window) {
            dock_thumb_elements(
                state,
                renderer,
                id,
                anchor_x,
                anchor_y,
                panel,
                output_scale,
                off_x,
                off_y,
                out,
            );
        } else {
            let label = match e.window {
                Some(id) => window_title(state, id),
                None if e.app == crate::state::APPS_BUTTON_APP => "Uygulamalar".to_string(),
                None if e.app == crate::state::RECENTS_BUTTON_APP => "Genel Bakış".to_string(),
                None if e.app == crate::state::SETTINGS_BUTTON_APP => "Ayarlar".to_string(),
                // A pinned shortcut: show its `.desktop` Name (Turkish if the
                // file has `Name[tr]=`), falling back to the raw app id.
                None => crate::icons::resolve_app_name(&e.app).unwrap_or_else(|| e.app.clone()),
            };
            dock_tooltip_elements(
                state,
                renderer,
                &label,
                anchor_x,
                anchor_y,
                panel,
                output_scale,
                off_x,
                off_y,
                out,
            );
        }
    }

    // In-flight drag on *this* output's bar, post-threshold. Borrowed
    // (not consumed) so the renderer can keep `&state`.
    let drag = state
        .dock_drag
        .as_ref()
        .filter(|d| d.started && d.output == output);

    // App ids of every window on *this output's* active workspace —
    // the same set `dock_tiles_for(output)` drew from, so the per-app
    // window count is consistent with what's on the bar.
    let ws_apps: Vec<String> = state
        .wm
        .active_workspace_on(output)
        .map(|ws| {
            state
                .wm
                .windows_on_workspace(ws)
                .into_iter()
                .map(|w| w.app)
                .collect()
        })
        .unwrap_or_default();

    for (i, entry) in tiles.iter().enumerate() {
        // Drag source slot renders empty — the ghost (pushed below)
        // shows the picked-up tile tracking the pointer instead.
        if drag.is_some_and(|d| i == d.from_idx) {
            continue;
        }
        let tile = entry.rect;
        let win = entry.window.and_then(|id| state.wm.get(id).ok());
        let minimized =
            matches!(win.as_ref().map(|w| &w.state), Some(WinState::Minimized));
        let focused = win.as_ref().map(|w| w.focused).unwrap_or(false);
        // A pinned launcher mid-spawn breathes so the click clearly
        // "took" while the app starts; settles back once its window
        // maps (then the slot is a running window, not this branch).
        let launching =
            entry.window.is_none() && state.launch_pending(&entry.app);
        let fade = if minimized {
            DOCK_MINIMIZED_FADE
        } else if launching {
            let t = state.start_time.elapsed().as_secs_f32();
            let phase = (t * std::f32::consts::TAU / DOCK_PULSE_SECS).sin();
            DOCK_PULSE_MIN + (DOCK_PULSE_MAX - DOCK_PULSE_MIN) * (0.5 + 0.5 * phase)
        } else {
            1.0
        };

        if entry.app == crate::state::APPS_BUTTON_APP {
            push_apps_glyph(tile, fade, output_scale, off_x, off_y, out);
        } else if entry.app == crate::state::SETTINGS_BUTTON_APP {
            push_settings_glyph(tile, fade, output_scale, off_x, off_y, out);
        } else if entry.app == crate::state::RECENTS_BUTTON_APP {
            push_recents_glyph(tile, fade, output_scale, off_x, off_y, out);
        } else if let Some(el) = app_icon_element(
            state, renderer, &entry.app, tile, fade, output_scale, off_x, off_y,
        ) {
            out.push(el);
        } else {
            out.push(solid_element(
                tile,
                tile_color(&entry.app, 0.9 * fade),
                output_scale,
                off_x,
                off_y,
            ));
        }

        // Hover highlight: a faint rounded chip behind this icon
        // (pushed after the icon → it sits below the glyph but above
        // the panel). Square-corner solid is the no-shader fallback.
        if Some(i) == hover {
            let g = pad * 0.5;
            let chip = Rect::new(
                tile.x - g,
                tile.y - g,
                tile.w + 2.0 * g,
                tile.h + 2.0 * g,
            );
            match card_program(renderer) {
                Some(p) => out.push(switcher_card_element(
                    p,
                    chip,
                    DOCK_HOVER_COLOR,
                    DOCK_HOVER_RADIUS,
                    false,
                    None,
                    1.0,
                    output_scale,
                    off_x,
                    off_y,
                )),
                None => out.push(solid_element(
                    chip,
                    DOCK_HOVER_COLOR,
                    output_scale,
                    off_x,
                    off_y,
                )),
            }
        }

        // Running indicator: a short accent bar at the tile's *inner*
        // edge (toward the screen centre), only under the focused
        // window's tile. A pinned-but-not-running launcher has no bar
        // — its absence is exactly the "not open yet" cue. An
        // *unfocused* urgent window gets the same bar geometry in an
        // alert colour, pulsing, so a request-for-attention is
        // impossible to miss.
        let urgent = !focused && win.as_ref().map(|w| w.urgent).unwrap_or(false);
        if focused || urgent {
            let bar = dock_indicator_bar(state.config.dock_edge, tile, pad);
            let color = if urgent {
                let t = state.start_time.elapsed().as_secs_f32();
                let phase = (t * std::f32::consts::TAU / DOCK_URGENT_SECS).sin();
                fade_color(DOCK_URGENT_COLOR, 0.5 + 0.5 * phase)
            } else {
                SWITCHER_RING_COLOR
            };
            out.push(solid_element(bar, color, output_scale, off_x, off_y));
        }

        // Multi-window count: one dot per window of this app on the
        // active workspace, in the panel's *outer* padding strip
        // (opposite the indicator). Shown only on a pinned slot (the
        // app-representative tile) and only when ≥2 — a single window
        // needs no dot, and the per-window sibling tiles of a
        // multi-window pinned app would otherwise each repeat the
        // same count. Dots arrange along the bar's row axis.
        let count = ws_apps.iter().filter(|a| **a == entry.app).count();
        if entry.pinned && count >= 2 {
            let n = count.min(DOCK_MAX_DOTS);
            for dot in
                dock_count_dot_rects(state.config.dock_edge, tile, pad, n)
            {
                out.push(solid_element(
                    dot,
                    DOCK_DOT_COLOR,
                    output_scale,
                    off_x,
                    off_y,
                ));
            }
        }
    }

    // Drag preview: the picked-up icon tracking the pointer (front)
    // plus a thin accent insertion bar at the snap target (behind the
    // ghost). Pushed before the panel so both sit *above* the bar.
    if let Some(d) = drag {
        if let Some(src) = tiles.get(d.from_idx) {
            let side = src.rect.w;
            let ghost = Rect::new(
                d.current_x - side / 2.0,
                d.current_y - side / 2.0,
                side,
                side,
            );
            if let Some(el) = app_icon_element(
                state, renderer, &src.app, ghost, 0.85, output_scale, off_x, off_y,
            ) {
                out.push(el);
            } else {
                out.push(solid_element(
                    ghost,
                    tile_color(&src.app, 0.75),
                    output_scale,
                    off_x,
                    off_y,
                ));
            }
        }
        // The insertion marker only makes sense when the pointer is
        // over the bar: off-panel drops mean unpin / no-op, not
        // "insert here".
        let on_panel = d.current_x >= panel.x
            && d.current_x <= panel.x + panel.w
            && d.current_y >= panel.y
            && d.current_y <= panel.y + panel.h;
        let pinned_len = state.config.dock_pinned.len();
        if on_panel {
            if let Some(target) =
                state.dock_drag_target_idx_at(output, d.current_x, d.current_y)
            {
                if let Some(bar) = dock_insertion_marker_rect(
                    state.config.dock_edge,
                    &tiles,
                    panel,
                    pinned_len,
                    target,
                    pad,
                ) {
                    out.push(solid_element(
                        bar,
                        SWITCHER_RING_COLOR,
                        output_scale,
                        off_x,
                        off_y,
                    ));
                }
            }
        }
    }

    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            panel,
            DOCK_PANEL_COLOR,
            DOCK_PANEL_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        // No shader → an honest flat translucent bar (square corners).
        None => out.push(solid_element(panel, DOCK_PANEL_COLOR, output_scale, off_x, off_y)),
    }
}

/// Draw the always-visible floating launcher button — a rounded square pinned
/// to the dock's leading corner (bottom-left for a bottom dock) that toggles
/// the window-presence auto-hidden dock. Anchored to the screen edge, so it
/// stays visible and hittable while the bar is tucked away. A grid glyph marks
/// it as a launcher; the panel brightens to an accent while the dock is
/// summoned so the toggle reads "on". No-op unless the dock can hide.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_dock_floating_button(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(r) = state.dock_floating_button_rect(output) else { return };
    // Front-first: glyph on top, panel behind (mirrors `render_dock`).
    push_apps_glyph(r, 1.0, output_scale, off_x, off_y, out);
    let bg = if state.dock_force_shown {
        Color32F::new(0.22, 0.45, 0.85, 0.96) // accent — dock summoned
    } else {
        Color32F::new(0.14, 0.16, 0.20, 0.92)
    };
    cc_card(out, renderer, r, bg, r.w * 0.28, output_scale, off_x, off_y);
}

/// Draw the dock's applications-menu button: a 3×3 grid of small light
/// squares filling the tile, in place of an app icon.
fn push_apps_glyph(
    tile: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    const N: usize = 3;
    let gap = tile.w * 0.08;
    let cell = (tile.w - (N as f32 + 1.0) * gap) / N as f32;
    let color = Color32F::new(0.92, 0.95, 1.0, 1.0 * fade);
    for r in 0..N {
        for c in 0..N {
            let x = tile.x + gap + c as f32 * (cell + gap);
            let y = tile.y + gap + r as f32 * (cell + gap);
            out.push(solid_element(
                Rect::new(x, y, cell, cell),
                color,
                output_scale,
                off_x,
                off_y,
            ));
        }
    }
}

/// Control-Center (gear) button glyph: two stacked horizontal sliders
/// with offset knobs — the same "toggles" motif macOS uses for its
/// Control Center icon. Drawn from flat rects so it needs no shader.
fn push_settings_glyph(
    tile: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let color = Color32F::new(0.85, 0.9, 0.95, 0.9 * fade);
    let bar_w = tile.w * 0.60;
    let bar_h = (tile.h * 0.12).max(2.0);
    let bx = tile.x + (tile.w - bar_w) / 2.0;
    let knob = bar_h * 2.0;
    // Top slider — knob on the right.
    let ty = tile.y + tile.h * 0.32;
    out.push(solid_element(Rect::new(bx, ty, bar_w, bar_h), color, output_scale, off_x, off_y));
    out.push(solid_element(
        Rect::new(bx + bar_w - knob, ty + bar_h / 2.0 - knob / 2.0, knob, knob),
        color,
        output_scale,
        off_x,
        off_y,
    ));
    // Bottom slider — knob on the left.
    let by2 = tile.y + tile.h * 0.58;
    out.push(solid_element(Rect::new(bx, by2, bar_w, bar_h), color, output_scale, off_x, off_y));
    out.push(solid_element(
        Rect::new(bx, by2 + bar_h / 2.0 - knob / 2.0, knob, knob),
        color,
        output_scale,
        off_x,
        off_y,
    ));
}

/// Recents/Overview button glyph: two overlapping cards suggesting a
/// stack of recent apps. Front card (brighter) pushed first so it lands
/// on top; the dimmer back card peeks out at the upper-right.
fn push_recents_glyph(
    tile: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let w = tile.w * 0.46;
    let h = tile.h * 0.56;
    let cx = tile.x + tile.w / 2.0;
    let cy = tile.y + tile.h / 2.0;
    let off = tile.w * 0.09;
    let bright = Color32F::new(0.85, 0.9, 0.95, 0.92 * fade);
    let dim = Color32F::new(0.85, 0.9, 0.95, 0.42 * fade);
    // Front card (lower-left), on top.
    out.push(solid_element(
        Rect::new(cx - w / 2.0 - off, cy - h / 2.0 + off, w, h),
        bright,
        output_scale,
        off_x,
        off_y,
    ));
    // Back card (upper-right), behind.
    out.push(solid_element(
        Rect::new(cx - w / 2.0 + off, cy - h / 2.0 - off, w, h),
        dim,
        output_scale,
        off_x,
        off_y,
    ));
}

/// Render the applications grid menu (Launchpad-style) when open on this
/// output. Front-to-back push order: each cell's icon + label first, then
/// a hover chip behind the cell under the pointer, then the panel
/// background last (furthest back).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_apps_menu(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(menu) = state.apps_menu.as_ref() else { return };
    if menu.output != output {
        return;
    }
    let (px, py) = state.pointer_position;
    let (px, py) = (px as f32, py as f32);

    // Grid cells — every cell at least partly in the viewport. The grid
    // scrolls by sub-row pixel amounts, so each element is hard-clipped to
    // the viewport rect (`grid_crop`); a partially-scrolled row is trimmed
    // at the edge instead of spilling over the search bar / panel.
    let grid_crop = to_physical_rect_offset(menu.grid_rect(), output_scale, off_x, off_y);
    let push_cropped = |out: &mut Vec<BacakElements>, el: BacakElements| {
        if let Some(c) = crop_element(el, output_scale, grid_crop) {
            out.push(c);
        }
    };
    for (i, item) in menu.items.iter().enumerate() {
        if !menu.cell_visible(i) {
            continue;
        }
        let cell = menu.item_rect(i);
        let side = (cell.w.min(cell.h) * 0.52).max(8.0);
        let icon = Rect::new(cell.x + (cell.w - side) / 2.0, cell.y + cell.h * 0.12, side, side);
        match app_icon_element(state, renderer, &item.app, icon, 1.0, output_scale, off_x, off_y) {
            Some(el) => push_cropped(out, el),
            None => push_cropped(out, solid_element(icon, tile_color(&item.app, 0.9), output_scale, off_x, off_y)),
        }
        if let Some((buf, w, h)) = &item.label {
            let scale = output_scale as f32;
            let lx = cell.x + (cell.w - *w as f32) / 2.0;
            let ly = cell.y + cell.h - *h as f32 - 8.0;
            let phys = Point::<f64, Physical>::from((
                ((lx - off_x as f32) * scale) as f64,
                ((ly - off_y as f32) * scale) as f64,
            ));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
            ) {
                push_cropped(out, BacakElements::Memory(el));
            }
        }
    }

    // Hover chip behind the visible cell under the pointer (clipped too,
    // so it never bleeds past the viewport on a partially-scrolled row).
    for (i, _item) in menu.items.iter().enumerate() {
        if menu.cell_visible(i) && menu.cell_hit(i, px, py) {
            let r = menu.item_rect(i);
            push_cropped(out, solid_element(r, DOCK_HOVER_COLOR, output_scale, off_x, off_y));
            break;
        }
    }

    // Search bar: query (or placeholder) text over a rounded field.
    if let Some((buf, w, h)) = &menu.query_label {
        let _ = w;
        let scale = output_scale as f32;
        let lx = menu.search.x + 12.0;
        let ly = menu.search.y + (menu.search.h - *h as f32) / 2.0;
        let phys = Point::<f64, Physical>::from((
            ((lx - off_x as f32) * scale) as f64,
            ((ly - off_y as f32) * scale) as f64,
        ));
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
        ) {
            out.push(BacakElements::Memory(el));
        }
    }
    let search_bg = Color32F::new(0.06, 0.07, 0.10, 0.98);
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p, menu.search, search_bg, 10.0, false, None, 1.0, output_scale, off_x, off_y,
        )),
        None => out.push(solid_element(menu.search, search_bg, output_scale, off_x, off_y)),
    }

    // Sidebar: "Kategoriler" heading (foreground) over each category
    // label, then the selected/hover chip behind them, then the sidebar
    // column itself, then the panel.
    if let Some((buf, _w, _h)) = &menu.header {
        let scale = output_scale as f32;
        let lx = menu.sidebar.x + crate::state::APPS_MENU_PAD;
        let ly = menu.sidebar.y + crate::state::APPS_MENU_PAD + 2.0;
        let phys = Point::<f64, Physical>::from((
            ((lx - off_x as f32) * scale) as f64,
            ((ly - off_y as f32) * scale) as f64,
        ));
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
        ) {
            out.push(BacakElements::Memory(el));
        }
    }
    for tab in &menu.cats {
        if let Some((buf, _w, h)) = &tab.label {
            let scale = output_scale as f32;
            let lx = tab.rect.x + 14.0;
            let ly = tab.rect.y + (tab.rect.h - *h as f32) / 2.0;
            let phys = Point::<f64, Physical>::from((
                ((lx - off_x as f32) * scale) as f64,
                ((ly - off_y as f32) * scale) as f64,
            ));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
            ) {
                out.push(BacakElements::Memory(el));
            }
        }
    }
    // Chips: a brighter pill behind the selected tab, a faint one under
    // the pointer.
    let sel_chip = Color32F::new(1.0, 1.0, 1.0, 0.12);
    let hover_chip = Color32F::new(1.0, 1.0, 1.0, 0.06);
    for (i, tab) in menu.cats.iter().enumerate() {
        let chip = if i == menu.selected {
            Some(sel_chip)
        } else if tab.rect.contains(px, py) {
            Some(hover_chip)
        } else {
            None
        };
        if let Some(c) = chip {
            match card_program(renderer) {
                Some(p) => out.push(switcher_card_element(
                    p, tab.rect, c, 9.0, false, None, 1.0, output_scale, off_x, off_y,
                )),
                None => out.push(solid_element(tab.rect, c, output_scale, off_x, off_y)),
            }
        }
    }
    // Sidebar column: a slightly lighter shade than the panel so the
    // category list reads as a distinct rail.
    let sidebar_bg = Color32F::new(0.07, 0.11, 0.16, 0.96);
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p, menu.sidebar, sidebar_bg, 18.0, false, None, 1.0, output_scale, off_x, off_y,
        )),
        None => out.push(solid_element(menu.sidebar, sidebar_bg, output_scale, off_x, off_y)),
    }

    // Panel background last → furthest back.
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            menu.panel,
            DOCK_TOOLTIP_BG,
            18.0,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(menu.panel, DOCK_TOOLTIP_BG, output_scale, off_x, off_y)),
    }
}

/// Push a rounded card (shader path) or a flat rect (fallback) — the
/// shared primitive behind every Control-Center tile and the panel.
#[allow(clippy::too_many_arguments)]
fn cc_card(
    out: &mut Vec<BacakElements>,
    renderer: &mut GlesRenderer,
    rect: Rect,
    color: Color32F,
    radius: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) {
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p, rect, color, radius, false, None, 1.0, output_scale, off_x, off_y,
        )),
        None => out.push(solid_element(rect, color, output_scale, off_x, off_y)),
    }
}

/// Physical px the emoji PNG is decoded to, and the supersample factor used
/// for its buffer (so the on-screen size is `OSK_EMOJI_PHYS / OSK_EMOJI_SS`).
const OSK_EMOJI_PHYS: u32 = 64;
const OSK_EMOJI_SS: i32 = 2;

/// Colour-emoji glyph buffer for the key label (a scalar or a ZWJ / skin-tone
/// sequence), decoded + cached once per process (emoji keys are static).
/// Returns the `MemoryRenderBuffer` and its logical size, or `None` when no
/// emoji font / glyph is available (caller falls back to fontdue).
fn osk_emoji_buffer(state: &BacakState, label: &str) -> Option<(MemoryRenderBuffer, f32)> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<String, Option<MemoryRenderBuffer>>> =
            RefCell::new(HashMap::new());
    }
    let logical = OSK_EMOJI_PHYS as f32 / OSK_EMOJI_SS as f32;
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(label) {
            return hit.clone().map(|b| (b, logical));
        }
        let buf = state
            .emoji_font
            .as_ref()
            .and_then(|f| f.glyph(label, OSK_EMOJI_PHYS))
            .map(|b| {
                MemoryRenderBuffer::from_slice(
                    &b.rgba,
                    Fourcc::Abgr8888,
                    (b.w as i32, b.h as i32),
                    OSK_EMOJI_SS,
                    Transform::Normal,
                    None,
                )
            });
        c.borrow_mut().insert(label.to_string(), buf.clone());
        buf.map(|b| (b, logical))
    })
}

/// The Anadolu Panteri logo (embedded PNG), decoded once and scaled to `px`
/// physical pixels, cached per process. Drawn on the Win/Super key instead of a
/// text label. Returns the buffer + its logical size.
fn win_logo_buffer(px: u32) -> Option<(MemoryRenderBuffer, f32)> {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<u32, Option<MemoryRenderBuffer>>> =
            RefCell::new(HashMap::new());
    }
    const LOGO: &[u8] = include_bytes!("../assets/anadolupanteri.png");
    let logical = px as f32 / OSK_EMOJI_SS as f32;
    CACHE.with(|c| {
        if let Some(hit) = c.borrow().get(&px) {
            return hit.clone().map(|b| (b, logical));
        }
        let buf = image::load_from_memory(LOGO).ok().map(|img| {
            // Logo as-is (glossy white circle + black panther line-art).
            let resized = image::imageops::resize(
                &img.to_rgba8(),
                px,
                px,
                image::imageops::FilterType::Triangle,
            );
            MemoryRenderBuffer::from_slice(
                &resized.into_raw(),
                Fourcc::Abgr8888,
                (px as i32, px as i32),
                OSK_EMOJI_SS,
                Transform::Normal,
                None,
            )
        });
        c.borrow_mut().insert(px, buf.clone());
        buf.map(|b| (b, logical))
    })
}

/// Blit a pre-rasterised label buffer at logical top-left `(lx, ly)`.
#[allow(clippy::too_many_arguments)]
fn cc_blit_label(
    out: &mut Vec<BacakElements>,
    renderer: &mut GlesRenderer,
    lbl: &Option<(MemoryRenderBuffer, usize, usize)>,
    lx: f32,
    ly: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) {
    let Some((buf, _w, _h)) = lbl else { return };
    let scale = output_scale as f32;
    let phys = Point::<f64, Physical>::from((
        ((lx - off_x as f32) * scale) as f64,
        ((ly - off_y as f32) * scale) as f64,
    ));
    if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys,
        buf,
        Some(1.0),
        None,
        None,
        Kind::Unspecified,
    ) {
        out.push(BacakElements::Memory(el));
    }
}

/// A slider tile's track geometry: `(x, y, width, height)` of the groove
/// inside the tile. Shared by the fill (pass 1) and the track (pass 2) so
/// they line up exactly, and matched to the hit-test in `state.rs`.
fn cc_slider_geom(r: Rect) -> (f32, f32, f32, f32) {
    let th = 8.0;
    let x = r.x + crate::state::CC_SLIDER_INSET;
    let w = (r.w - 2.0 * crate::state::CC_SLIDER_INSET).max(1.0);
    let y = r.y + r.h - 20.0;
    (x, y, w, th)
}

/// Render the macOS-style Control Center when open on this output.
/// Front-to-back push order (earliest = topmost): tile foreground
/// (labels, slider fills, knobs) → tile cards / slider tracks → panel
/// background last.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_control_center(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(cc) = state.control_center.as_ref() else { return };
    if cc.output != output {
        return;
    }

    let accent = SWITCHER_RING_COLOR;
    let warm = Color32F::new(1.0, 0.78, 0.30, 0.95);
    let off_card = Color32F::new(1.0, 1.0, 1.0, 0.10);
    let track_col = Color32F::new(1.0, 1.0, 1.0, 0.13);
    let knob_col = Color32F::new(1.0, 1.0, 1.0, 0.98);
    let danger_col = Color32F::new(0.92, 0.30, 0.30, 0.92);

    let on_for = |action: crate::state::CcAction| match action {
        crate::state::CcAction::WifiToggle => cc.wifi_on,
        crate::state::CcAction::BtToggle => cc.bt_on,
        crate::state::CcAction::DarkToggle => state.dark_mode,
        _ => false,
    };

    // --- pass 1: foreground (labels, slider fills + knobs, clock) ---
    cc_blit_label(
        out,
        renderer,
        &cc.clock,
        cc.panel.x + 18.0,
        cc.panel.y + 13.0,
        output_scale,
        off_x,
        off_y,
    );

    for tile in &cc.tiles {
        let r = tile.rect;
        match tile.kind {
            crate::state::CcKind::Slider => {
                let level = match tile.action {
                    crate::state::CcAction::Brightness => {
                        (state.brightness - crate::state::MIN_BRIGHTNESS)
                            / (1.0 - crate::state::MIN_BRIGHTNESS)
                    }
                    crate::state::CcAction::MicVolume => cc.mic_volume,
                    _ => cc.volume,
                }
                .clamp(0.0, 1.0);
                let (tx, ty, tw, th) = cc_slider_geom(r);
                let fill_col = if matches!(tile.action, crate::state::CcAction::Brightness) {
                    warm
                } else if matches!(tile.action, crate::state::CcAction::MicVolume) {
                    Color32F::new(0.78, 0.42, 0.42, 0.95) // kırmızımsı, mikrofonu ayırt etmek için
                } else {
                    accent
                };
                let fw = (tw * level).max(2.0);
                cc_card(out, renderer, Rect::new(tx, ty, fw, th), fill_col, th / 2.0, output_scale, off_x, off_y);
                let knob = 18.0;
                cc_card(
                    out,
                    renderer,
                    Rect::new(tx + fw - knob / 2.0, ty + th / 2.0 - knob / 2.0, knob, knob),
                    knob_col,
                    knob / 2.0,
                    output_scale,
                    off_x,
                    off_y,
                );
                // Label sits above the track, top-left.
                cc_blit_label(out, renderer, &tile.label, tx, r.y + 7.0, output_scale, off_x, off_y);
            }
            crate::state::CcKind::Button { .. } => {
                if let Some((_, w, h)) = &tile.label {
                    let lx = r.x + (r.w - *w as f32) / 2.0;
                    let ly = r.y + (r.h - *h as f32) / 2.0;
                    cc_blit_label(out, renderer, &tile.label, lx, ly, output_scale, off_x, off_y);
                }
            }
            crate::state::CcKind::Toggle => {
                // Tall toggles (wifi/bt) stack label + sub at the bottom;
                // the short full-width dark tile centres its label.
                let label_h = tile.label.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0);
                let sub_h = tile.sub.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0);
                if r.h >= 80.0 {
                    let lx = r.x + 14.0;
                    if tile.sub.is_some() {
                        let sub_y = r.y + r.h - 12.0 - sub_h;
                        let label_y = sub_y - label_h - 2.0;
                        cc_blit_label(out, renderer, &tile.label, lx, label_y, output_scale, off_x, off_y);
                        cc_blit_label(out, renderer, &tile.sub, lx, sub_y, output_scale, off_x, off_y);
                    } else {
                        let label_y = r.y + r.h - 12.0 - label_h;
                        cc_blit_label(out, renderer, &tile.label, lx, label_y, output_scale, off_x, off_y);
                    }
                } else {
                    let lx = r.x + 16.0;
                    let label_y = r.y + (r.h - label_h) / 2.0;
                    cc_blit_label(out, renderer, &tile.label, lx, label_y, output_scale, off_x, off_y);
                }
            }
        }
    }

    // --- pass 2: tile cards / slider tracks ---
    for tile in &cc.tiles {
        let r = tile.rect;
        match tile.kind {
            crate::state::CcKind::Slider => {
                let (tx, ty, tw, th) = cc_slider_geom(r);
                cc_card(out, renderer, Rect::new(tx, ty, tw, th), track_col, th / 2.0, output_scale, off_x, off_y);
            }
            crate::state::CcKind::Button { danger } => {
                let col = if danger { danger_col } else { off_card };
                cc_card(out, renderer, r, col, 12.0, output_scale, off_x, off_y);
            }
            crate::state::CcKind::Toggle => {
                let col = if on_for(tile.action) { accent } else { off_card };
                cc_card(out, renderer, r, col, 16.0, output_scale, off_x, off_y);
            }
        }
    }

    // --- pass 3: panel background, furthest back ---
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.94)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.94)
    };
    cc_card(out, renderer, cc.panel, panel_col, 22.0, output_scale, off_x, off_y);
}

/// Render the native Wi-Fi picker when open: a top-right card with a title, a
/// status line, one card per scanned network (accent-tinted for the connected
/// one), and a red "turn off" button. Same three-pass front-to-back order as
/// the Control Center.
pub(crate) fn render_wifi_panel(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(p) = state.wifi_panel.as_ref() else { return };
    if p.output != output {
        return;
    }

    let row_col = Color32F::new(1.0, 1.0, 1.0, 0.08);
    let row_active = Color32F::new(0.30, 0.50, 0.78, 0.34);
    let off_col = Color32F::new(0.92, 0.30, 0.30, 0.55);

    // --- pass 1: labels (front) ---
    cc_blit_label(out, renderer, &p.title, p.panel.x + 18.0, p.panel.y + 14.0, output_scale, off_x, off_y);
    cc_blit_label(out, renderer, &p.status, p.panel.x + 18.0, p.panel.y + 46.0, output_scale, off_x, off_y);
    // Switch knobs (front); the track is drawn behind them in pass 2. The
    // header on/off switch shows in list mode; the auto-reconnect switch in
    // details mode.
    let knob_at = |out: &mut Vec<BacakElements>, renderer: &mut GlesRenderer, s: Rect, on: bool| {
        let knob = s.h - 6.0;
        let kx = if on { s.x + s.w - knob - 3.0 } else { s.x + 3.0 };
        cc_card(
            out,
            renderer,
            Rect::new(kx, s.y + 3.0, knob, knob),
            Color32F::new(1.0, 1.0, 1.0, 0.98),
            knob / 2.0,
            output_scale,
            off_x,
            off_y,
        );
    };
    if p.pw_for.is_none() && p.details.is_none() {
        knob_at(out, renderer, p.switch_rect, p.wifi_on);
    }
    if let Some(d) = p.details.as_ref() {
        // No toggle row on a read-only (externally-managed) Ethernet panel.
        if !p.eth || d.managed {
            knob_at(out, renderer, p.ac_switch_rect, if p.eth { d.connected } else { d.autoconnect });
        }
    }
    // Password box contents (front): the text, then the reveal (eye) icon.
    if let Some(f) = p.pw_field {
        if let Some((_, _, h)) = &p.pw_text {
            let ty = f.y + (f.h - *h as f32) / 2.0;
            cc_blit_label(out, renderer, &p.pw_text, f.x + 14.0, ty, output_scale, off_x, off_y);
        }
        let eye_w = crate::state::WIFI_EYE_W;
        let isz = 22.0;
        let eye = Rect::new(
            f.x + f.w - eye_w + (eye_w - isz) / 2.0,
            f.y + (f.h - isz) / 2.0,
            isz,
            isz,
        );
        // The reveal eye is for the password box only — static-IP fields are
        // plain text.
        if p.pw_for.is_some() {
            let name = if p.pw_show { "view-hidden" } else { "view-visible" };
            // Prefer breeze-dark's light glyph so the eye is visible on the dark
            // panel; fall back to the bare name if that theme isn't installed.
            let icon = crate::icons::themed_icon_path("breeze-dark", name)
                .unwrap_or_else(|| name.to_string());
            if let Some(el) = app_icon_element(state, renderer, &icon, eye, 1.0, output_scale, off_x, off_y) {
                out.push(el);
            }
        }
    }
    for row in &p.rows {
        let r = row.rect;
        // The connected network carries a gear (settings) icon on the right.
        let gear = matches!(row.action, crate::state::WifiAction::OpenDetails);
        let meta_pad = if gear { 14.0 + crate::state::WIFI_GEAR_W } else { 14.0 };
        if let Some((_, _, h)) = &row.label {
            let ly = r.y + (r.h - *h as f32) / 2.0;
            cc_blit_label(out, renderer, &row.label, r.x + 14.0, ly, output_scale, off_x, off_y);
        }
        if let Some((_, w, h)) = &row.meta {
            let mx = r.x + r.w - *w as f32 - meta_pad;
            let my = r.y + (r.h - *h as f32) / 2.0;
            cc_blit_label(out, renderer, &row.meta, mx, my, output_scale, off_x, off_y);
        }
        if gear {
            let isz = 22.0;
            let grect = Rect::new(
                r.x + r.w - crate::state::WIFI_GEAR_W + (crate::state::WIFI_GEAR_W - isz) / 2.0,
                r.y + (r.h - isz) / 2.0,
                isz,
                isz,
            );
            let icon = crate::icons::themed_icon_path("breeze-dark", "configure")
                .unwrap_or_else(|| "configure".to_string());
            if let Some(el) = app_icon_element(state, renderer, &icon, grect, 1.0, output_scale, off_x, off_y) {
                out.push(el);
            }
        }
    }

    // --- pass 2: row cards ---
    // The connected network row is a bright turquoise pill (with dark text set
    // in `build_wifi_panel`); other active rows keep the muted accent tint.
    let connected = Color32F::new(0.42, 0.78, 0.80, 0.96);
    for row in &p.rows {
        let col = if matches!(row.action, crate::state::WifiAction::PwCancel) {
            off_col
        } else if matches!(row.action, crate::state::WifiAction::OpenDetails) {
            connected
        } else if row.active {
            row_active
        } else {
            row_col
        };
        cc_card(out, renderer, row.rect, col, 12.0, output_scale, off_x, off_y);
    }
    // Switch tracks (behind the knobs): accent when on, grey when off.
    let track_at = |out: &mut Vec<BacakElements>, renderer: &mut GlesRenderer, s: Rect, on: bool| {
        let col = if on {
            Color32F::new(0.30, 0.50, 0.78, 0.95)
        } else {
            Color32F::new(1.0, 1.0, 1.0, 0.16)
        };
        cc_card(out, renderer, s, col, s.h / 2.0, output_scale, off_x, off_y);
    };
    if p.pw_for.is_none() && p.details.is_none() {
        track_at(out, renderer, p.switch_rect, p.wifi_on);
    }
    if let Some(d) = p.details.as_ref() {
        if !p.eth || d.managed {
            track_at(out, renderer, p.ac_switch_rect, if p.eth { d.connected } else { d.autoconnect });
        }
    }
    // Password input box (a touch darker than a row, like a text field).
    if let Some(f) = p.pw_field {
        cc_card(out, renderer, f, Color32F::new(1.0, 1.0, 1.0, 0.13), 12.0, output_scale, off_x, off_y);
    }

    // --- pass 3: panel backdrop ---
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.95)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.95)
    };
    cc_card(out, renderer, p.panel, panel_col, 22.0, output_scale, off_x, off_y);
}

/// Render the native Bluetooth picker: header + on/off switch, device list, and
/// (when active) a modal pairing dialog. Same three-pass front-to-back order.
pub(crate) fn render_bt_panel(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(p) = state.bt_panel.as_ref() else { return };
    if p.output != output {
        return;
    }
    let row_col = Color32F::new(1.0, 1.0, 1.0, 0.08);
    let connected = Color32F::new(0.42, 0.78, 0.80, 0.96);
    let off_col = Color32F::new(0.92, 0.30, 0.30, 0.30);
    let accent = Color32F::new(0.30, 0.62, 0.78, 0.95);

    // --- pass 1: labels (front) ---
    cc_blit_label(out, renderer, &p.title, p.panel.x + 18.0, p.panel.y + 14.0, output_scale, off_x, off_y);
    cc_blit_label(out, renderer, &p.status, p.panel.x + 18.0, p.panel.y + 46.0, output_scale, off_x, off_y);
    // Header switch knob.
    {
        let s = p.switch_rect;
        let knob = s.h - 6.0;
        let kx = if p.powered { s.x + s.w - knob - 3.0 } else { s.x + 3.0 };
        cc_card(out, renderer, Rect::new(kx, s.y + 3.0, knob, knob), Color32F::new(1.0, 1.0, 1.0, 0.98), knob / 2.0, output_scale, off_x, off_y);
    }
    for row in &p.rows {
        let r = row.rect;
        let is_header = matches!(row.action, crate::state::BtAction::Header);
        // Header label sits at the left, small; rows stack name + meta.
        if let Some((_, _, h)) = &row.label {
            let ly = r.y + (r.h - *h as f32) / 2.0;
            let lx = if is_header { r.x + 4.0 } else { r.x + 14.0 };
            cc_blit_label(out, renderer, &row.label, lx, ly, output_scale, off_x, off_y);
        }
        if let Some((_, _, h)) = &row.meta {
            // Meta as a small second line under the name (left-aligned).
            let my = r.y + r.h - *h as f32 - 7.0;
            cc_blit_label(out, renderer, &row.meta, r.x + 14.0, my, output_scale, off_x, off_y);
        }
        // "Unut" button label on a paired row.
        if let Some((fr, flbl)) = &row.forget {
            if let Some((_, w, h)) = flbl {
                let lx = fr.x + (fr.w - *w as f32) / 2.0;
                let ly = fr.y + (fr.h - *h as f32) / 2.0;
                cc_blit_label(out, renderer, flbl, lx, ly, output_scale, off_x, off_y);
            }
        }
    }

    // --- pass 2: cards (headers get no card) ---
    for row in &p.rows {
        if matches!(row.action, crate::state::BtAction::Header) {
            continue;
        }
        let col = if row.connected { connected } else { row_col };
        cc_card(out, renderer, row.rect, col, 12.0, output_scale, off_x, off_y);
        // The "Unut" sub-button gets a faint divider tint so it reads as tappable.
        if let Some((fr, _)) = &row.forget {
            cc_card(out, renderer, *fr, Color32F::new(0.0, 0.0, 0.0, 0.18), 12.0, output_scale, off_x, off_y);
        }
    }
    {
        let s = p.switch_rect;
        let track = if p.powered { accent } else { Color32F::new(1.0, 1.0, 1.0, 0.16) };
        cc_card(out, renderer, s, track, s.h / 2.0, output_scale, off_x, off_y);
    }

    // --- pass 3: panel backdrop ---
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.95)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.95)
    };
    cc_card(out, renderer, p.panel, panel_col, 22.0, output_scale, off_x, off_y);

    // --- pairing dialog (modal overlay, drawn on top of everything) ---
    if p.dialog.is_some() {
        // Geometry computed in `bt_open_dialog` so render and hit-test agree.
        let dlg = p.dlg_rect;
        // front: labels + buttons
        cc_blit_label(out, renderer, &p.dialog_title, dlg.x + 16.0, dlg.y + 16.0, output_scale, off_x, off_y);
        cc_blit_label(out, renderer, &p.dialog_body, dlg.x + 16.0, dlg.y + 52.0, output_scale, off_x, off_y);
        // centred button labels
        for (lbl, r) in [(&p.dlg_ok_label, p.dlg_ok_rect), (&p.dlg_cancel_label, p.dlg_cancel_rect)] {
            if let Some((_, w, h)) = lbl {
                let lx = r.x + (r.w - *w as f32) / 2.0;
                let ly = r.y + (r.h - *h as f32) / 2.0;
                cc_blit_label(out, renderer, lbl, lx, ly, output_scale, off_x, off_y);
            }
        }
        // button cards
        cc_card(out, renderer, p.dlg_ok_rect, accent, 12.0, output_scale, off_x, off_y);
        cc_card(out, renderer, p.dlg_cancel_rect, off_col, 12.0, output_scale, off_x, off_y);
        // dialog backdrop
        let dbg = if state.dark_mode {
            Color32F::new(0.10, 0.13, 0.18, 0.99)
        } else {
            Color32F::new(0.16, 0.18, 0.23, 0.99)
        };
        cc_card(out, renderer, dlg, dbg, 18.0, output_scale, off_x, off_y);
    }
}

/// Render the audio output device picker when open. Simple vertical list of
/// sinks; the default sink gets an accent tint. Front-to-back: labels, row
/// cards, then the panel backdrop.
pub(crate) fn render_audio_panel(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(p) = state.audio_panel.as_ref() else { return };
    if p.output != output {
        return;
    }
    let row_col = Color32F::new(1.0, 1.0, 1.0, 0.08);
    let accent = Color32F::new(0.30, 0.62, 0.78, 0.30);

    // --- pass 1: labels (front) ---
    cc_blit_label(out, renderer, &p.title, p.panel.x + 18.0, p.panel.y + 12.0, output_scale, off_x, off_y);
    cc_blit_label(out, renderer, &p.status, p.panel.x + 18.0, p.panel.y + 52.0, output_scale, off_x, off_y);
    for row in &p.rows {
        let r = row.rect;
        if let Some((_, w, h)) = &row.label {
            let ly = r.y + (r.h - *h as f32) / 2.0;
            // Close button: centre-aligned; sink rows: left-aligned.
            let lx = if matches!(row.action, crate::state::AudioAction::Close) {
                r.x + (r.w - *w as f32) / 2.0
            } else {
                r.x + 14.0
            };
            cc_blit_label(out, renderer, &row.label, lx, ly, output_scale, off_x, off_y);
        }
        // Checkmark for the current default sink.
        if row.is_default {
            let ck_size = 16.0;
            let ck_x = r.x + r.w - ck_size - 14.0;
            let ck_y = r.y + (r.h - ck_size) / 2.0;
            cc_card(out, renderer, Rect::new(ck_x, ck_y, ck_size, ck_size), Color32F::new(0.30, 0.78, 0.50, 0.95), ck_size / 2.0, output_scale, off_x, off_y);
        }
    }

    // --- pass 2: row cards ---
    let close_col = Color32F::new(1.0, 1.0, 1.0, 0.12);
    for row in &p.rows {
        let col = if matches!(row.action, crate::state::AudioAction::Close) {
            close_col
        } else if row.is_default {
            accent
        } else {
            row_col
        };
        cc_card(out, renderer, row.rect, col, 12.0, output_scale, off_x, off_y);
    }

    // --- pass 3: panel backdrop ---
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.95)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.95)
    };
    cc_card(out, renderer, p.panel, panel_col, 22.0, output_scale, off_x, off_y);
}

/// Render the microphone input device picker when open.
pub(crate) fn render_mic_panel(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(p) = state.mic_panel.as_ref() else { return };
    if p.output != output { return; }
    let row_col = Color32F::new(1.0, 1.0, 1.0, 0.08);
    let accent = Color32F::new(0.78, 0.42, 0.42, 0.30); // kırmızımsı — mikrofon rengi
    let close_col = Color32F::new(1.0, 1.0, 1.0, 0.12);

    cc_blit_label(out, renderer, &p.title, p.panel.x + 18.0, p.panel.y + 12.0, output_scale, off_x, off_y);
    cc_blit_label(out, renderer, &p.status, p.panel.x + 18.0, p.panel.y + 52.0, output_scale, off_x, off_y);
    for row in &p.rows {
        let r = row.rect;
        if let Some((_, w, h)) = &row.label {
            let ly = r.y + (r.h - *h as f32) / 2.0;
            let lx = if matches!(row.action, crate::state::AudioAction::Close) {
                r.x + (r.w - *w as f32) / 2.0
            } else {
                r.x + 14.0
            };
            cc_blit_label(out, renderer, &row.label, lx, ly, output_scale, off_x, off_y);
        }
        if row.is_default {
            let ck_size = 16.0;
            let ck_x = r.x + r.w - ck_size - 14.0;
            let ck_y = r.y + (r.h - ck_size) / 2.0;
            cc_card(out, renderer, Rect::new(ck_x, ck_y, ck_size, ck_size), Color32F::new(0.30, 0.78, 0.50, 0.95), ck_size / 2.0, output_scale, off_x, off_y);
        }
    }
    for row in &p.rows {
        let col = if matches!(row.action, crate::state::AudioAction::Close) {
            close_col
        } else if row.is_default {
            accent
        } else {
            row_col
        };
        cc_card(out, renderer, row.rect, col, 12.0, output_scale, off_x, off_y);
    }
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.95)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.95)
    };
    cc_card(out, renderer, p.panel, panel_col, 22.0, output_scale, off_x, off_y);
}

/// Render the screenshot options dialog (scope + delay) when open. Modal-style
/// panel, centred. Front-to-back: labels, then button cards (selected scope /
/// delay tinted accent, capture green), then the panel backdrop.
pub(crate) fn render_shot_dialog(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(d) = state.shot_dialog.as_ref() else { return };
    if d.output != output {
        return;
    }
    let accent = SWITCHER_RING_COLOR;
    let off_card = Color32F::new(1.0, 1.0, 1.0, 0.10);
    let go = Color32F::new(0.28, 0.62, 0.40, 0.96);

    // Centre a label inside a rect (front-most pass).
    let centered = |out: &mut Vec<BacakElements>,
                    renderer: &mut GlesRenderer,
                    lbl: &crate::state::Label,
                    r: Rect| {
        if let Some((_, w, h)) = lbl {
            let lx = r.x + (r.w - *w as f32) / 2.0;
            let ly = r.y + (r.h - *h as f32) / 2.0;
            cc_blit_label(out, renderer, lbl, lx, ly, output_scale, off_x, off_y);
        }
    };

    // --- pass 1: labels (front) ---
    if let Some((_, w, _)) = &d.title {
        let lx = d.panel.x + (d.panel.w - *w as f32) / 2.0;
        cc_blit_label(out, renderer, &d.title, lx, d.panel.y + 16.0, output_scale, off_x, off_y);
    }
    centered(out, renderer, &d.l_whole, d.mode_whole);
    centered(out, renderer, &d.l_window, d.mode_window);
    // "Gecikme" label left-aligned just above the first delay pill.
    if let Some(first) = d.delays.first() {
        cc_blit_label(
            out,
            renderer,
            &d.l_delay,
            d.panel.x + 18.0,
            first.0.y - 18.0,
            output_scale,
            off_x,
            off_y,
        );
    }
    for ((r, _), lbl) in d.delays.iter().zip(d.delay_lbls.iter()) {
        centered(out, renderer, lbl, *r);
    }
    centered(out, renderer, &d.l_cancel, d.cancel);
    centered(out, renderer, &d.l_capture, d.capture);

    // --- pass 2: button cards (selection highlight) ---
    let mode_w_col = if d.sel_window { off_card } else { accent };
    let mode_n_col = if d.sel_window { accent } else { off_card };
    cc_card(out, renderer, d.mode_whole, mode_w_col, 14.0, output_scale, off_x, off_y);
    cc_card(out, renderer, d.mode_window, mode_n_col, 14.0, output_scale, off_x, off_y);
    for (r, s) in &d.delays {
        let col = if *s == d.sel_delay { accent } else { off_card };
        cc_card(out, renderer, *r, col, 12.0, output_scale, off_x, off_y);
    }
    cc_card(out, renderer, d.cancel, off_card, 14.0, output_scale, off_x, off_y);
    cc_card(out, renderer, d.capture, go, 14.0, output_scale, off_x, off_y);

    // --- pass 3: panel backdrop (back) ---
    let panel_col = if state.dark_mode {
        Color32F::new(0.05, 0.08, 0.13, 0.96)
    } else {
        Color32F::new(0.20, 0.22, 0.27, 0.96)
    };
    cc_card(out, renderer, d.panel, panel_col, 22.0, output_scale, off_x, off_y);
}

/// Render the transient notification banner (toast) top-centre, if active on
/// this output. Two lines (title + path), panel fades over its last stretch.
pub(crate) fn render_toast(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(t) = state.toast.as_ref() else { return };
    if t.output != output {
        return;
    }
    let Some(o) = state.wm.output(output) else { return };
    let b = o.bounds;
    let elapsed = t.start.elapsed().as_millis() as u64;
    if elapsed >= crate::state::TOAST_MS {
        return;
    }
    let fade = if elapsed + crate::state::TOAST_FADE_MS > crate::state::TOAST_MS {
        ((crate::state::TOAST_MS - elapsed) as f32 / crate::state::TOAST_FADE_MS as f32)
            .clamp(0.0, 1.0)
    } else {
        1.0
    };

    let px = b.x + (b.w - t.w) / 2.0;
    let py = b.y + 40.0;
    let panel = Rect::new(px, py, t.w, t.h);

    // Labels (front): title then path, vertically centred as a block.
    let lx = px + 18.0;
    let th = t.title.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0);
    let sh = t.sub.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0);
    let total = th + 4.0 + sh;
    let mut ly = py + (t.h - total) / 2.0;
    if t.title.is_some() {
        cc_blit_label(out, renderer, &t.title, lx, ly, output_scale, off_x, off_y);
        ly += th + 4.0;
    }
    if t.sub.is_some() {
        cc_blit_label(out, renderer, &t.sub, lx, ly, output_scale, off_x, off_y);
    }

    let bg = Color32F::new(0.06, 0.09, 0.14, 0.96 * fade);
    cc_card(out, renderer, panel, bg, 16.0, output_scale, off_x, off_y);
}

/// Render the Android-style Overview when open on this output: a grid of
/// live window cards over a dimmed backdrop. Front-to-back push order
/// (earliest = topmost): per-card label + icon + live thumbnail → card
/// background → the full-screen backdrop last. The card currently being
/// flicked is offset by the drag delta and fades as it travels up.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_overview(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    use crate::carousel::{self, DragAxis};

    let Some(ov) = state.overview.as_ref() else { return };
    if ov.output != output {
        return;
    }
    let Some(o) = state.wm.output(output) else { return };
    let bounds = o.bounds;
    let scroll = ov.scroll.pos;
    let n = ov.cards.len();
    let sel = carousel::selected(scroll, n, carousel::spacing(bounds));
    let drag = state.overview_drag;

    // "Close all" pill — pushed first so it stays frontmost.
    {
        let r = ov.close_all;
        let (lw, lh) = ov
            .close_all_label
            .as_ref()
            .map(|(_, w, h)| (*w as f32, *h as f32))
            .unwrap_or((0.0, 0.0));
        cc_blit_label(
            out,
            renderer,
            &ov.close_all_label,
            r.x + (r.w - lw) / 2.0,
            r.y + (r.h - lh) / 2.0,
            output_scale,
            off_x,
            off_y,
        );
        cc_card(
            out,
            renderer,
            r,
            Color32F::new(0.16, 0.18, 0.22, 0.95),
            r.h / 2.0,
            output_scale,
            off_x,
            off_y,
        );
    }

    // Cards, centre-first (front-first push order = nearest-to-centre on top).
    // Off-screen cards (dist ≳ 2.5) are skipped entirely.
    let mut order: Vec<(usize, carousel::CardT)> = (0..n)
        .map(|i| (i, carousel::card_transform(i, scroll, bounds)))
        .filter(|(_, t)| t.dist < 2.5)
        .collect();
    order.sort_by(|a, b| a.1.dist.partial_cmp(&b.1.dist).unwrap_or(std::cmp::Ordering::Equal));

    for (i, t) in order {
        let card = &ov.cards[i];
        let mut r = t.rect;
        let mut fade = t.opacity;
        // Vertical lift + fade — from a live swipe-up drag, or an in-flight
        // dismiss-exit / snap-back animation (Android-style). Fades to nothing
        // as the card flies off the top; recovers as a snap-back returns to 0.
        let lift = if let Some(d) = drag.filter(|d| d.axis == DragAxis::Vertical && d.card == card.id) {
            d.dismiss_dy
        } else if let Some(da) = ov.dismiss.as_ref().filter(|da| da.id == card.id) {
            da.dy.pos as f32
        } else {
            0.0
        };
        if lift > 0.0 {
            r.y -= lift;
            let f = 1.0 - lift / (crate::state::OVERVIEW_CLOSE_DIST * 1.4);
            fade *= f.clamp(0.0, 1.0);
        }

        let label_band = 30.0 * t.scale;
        let pad = 6.0 * t.scale;
        let icon = 18.0 * t.scale;

        // Title label + app icon, centred in the bottom band.
        let label_y_mid = r.y + r.h - label_band / 2.0;
        let (lw, lh) = card
            .label
            .as_ref()
            .map(|(_, w, h)| (*w as f32, *h as f32))
            .unwrap_or((0.0, 0.0));
        let group_w = icon + 6.0 + lw;
        let gx = r.x + (r.w - group_w) / 2.0;
        if let Some(el) = app_icon_element(
            state,
            renderer,
            &card.app,
            Rect::new(gx, label_y_mid - icon / 2.0, icon, icon),
            fade,
            output_scale,
            off_x,
            off_y,
        ) {
            out.push(el);
        }
        cc_blit_label(
            out,
            renderer,
            &card.label,
            gx + icon + 6.0,
            label_y_mid - lh / 2.0,
            output_scale,
            off_x,
            off_y,
        );

        // Live thumbnail above the label band (falls back to a big app icon).
        let thumb = Rect::new(
            r.x + pad,
            r.y + pad,
            (r.w - 2.0 * pad).max(1.0),
            (r.h - label_band - 2.0 * pad).max(1.0),
        );
        // Thumbnail source. PERF: only the centred (focused) card re-renders
        // its live surface tree each frame — for side cards we blit the cached
        // snapshot quad instead, so a carousel of many windows (each possibly a
        // browser with dozens of subsurfaces) doesn't re-walk every tree per
        // frame. Each path falls back through the other source, then the icon.
        let mut placed = false;
        if i == sel {
            // Centred: live first (crisp + current), snapshot if unmapped.
            let live =
                surface_fit_into_area(state, renderer, card.id, thumb, fade, output_scale, off_x, off_y);
            if !live.is_empty() {
                out.extend(live);
                placed = true;
            } else if let Some(el) =
                snapshot_element(state, renderer, card.id, thumb, fade, output_scale, off_x, off_y)
            {
                out.push(el);
                placed = true;
            }
        } else {
            // Side: snapshot first (cheap), live only if there's no snapshot yet.
            if let Some(el) =
                snapshot_element(state, renderer, card.id, thumb, fade, output_scale, off_x, off_y)
            {
                out.push(el);
                placed = true;
            } else {
                let live = surface_fit_into_area(
                    state, renderer, card.id, thumb, fade, output_scale, off_x, off_y,
                );
                if !live.is_empty() {
                    out.extend(live);
                    placed = true;
                }
            }
        }
        if !placed {
            // No surface, no snapshot → app icon.
            let side = (thumb.w.min(thumb.h) * 0.5).max(8.0);
            if let Some(el) = app_icon_element(
                state,
                renderer,
                &card.app,
                Rect::new(
                    thumb.x + (thumb.w - side) / 2.0,
                    thumb.y + (thumb.h - side) / 2.0,
                    side,
                    side,
                ),
                fade,
                output_scale,
                off_x,
                off_y,
            ) {
                out.push(el);
            }
        }

        // Card background.
        let col = Color32F::new(0.10, 0.12, 0.16, 0.97 * fade);
        cc_card(out, renderer, r, col, 16.0, output_scale, off_x, off_y);

        // "Couldn't close" pulse: a red ring on a card whose window refused to
        // close, fading over DISMISS_ERROR_MS. Drawn in front of the accent
        // ring so it reads as the dominant signal.
        if let Some((eid, t)) = ov.error {
            if eid == card.id {
                let e = (t.elapsed().as_millis() as f32 / crate::state::DISMISS_ERROR_MS as f32)
                    .clamp(0.0, 1.0);
                let a = (1.0 - e) * 0.9;
                const ERING: f32 = 5.0;
                let ring = Rect::new(r.x - ERING, r.y - ERING, r.w + 2.0 * ERING, r.h + 2.0 * ERING);
                cc_card(out, renderer, ring, Color32F::new(0.86, 0.22, 0.22, a), 18.0, output_scale, off_x, off_y);
            }
        }

        // Accent ring on the centred card — pushed right after its own group
        // so it sits behind this card but in front of the neighbours drawn
        // later, peeking as a border.
        if i == sel {
            const RING: f32 = 4.0;
            let ring = Rect::new(r.x - RING, r.y - RING, r.w + 2.0 * RING, r.h + 2.0 * RING);
            cc_card(out, renderer, ring, SWITCHER_RING_COLOR, 18.0, output_scale, off_x, off_y);
        }
    }

    // Dimmed backdrop, furthest back.
    out.push(solid_element(
        bounds,
        Color32F::new(0.0, 0.0, 0.0, 0.55),
        output_scale,
        off_x,
        off_y,
    ));
}

/// Push the hover tooltip: a rounded dark plaque carrying `text`,
/// floating just above the panel and horizontally centred on the
/// hovered tile (clamped to stay over the bar). The rasterised line is
/// cached in `state.dock_tooltip` keyed by the string, so a motionless
/// hover doesn't re-upload the glyph buffer every frame. No-op when
/// there's no font or the text is empty — the highlight chip alone
/// still signals the hover.
#[allow(clippy::too_many_arguments)]
fn dock_tooltip_elements(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    text: &str,
    anchor_cx: f32,
    anchor_cy: f32,
    panel: Rect,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(font) = state.text.as_ref() else { return };
    // Cap text width to the longer panel dimension — horizontal docks
    // get the row width, vertical docks the column height.
    let max_run = if state.config.dock_edge.is_horizontal() {
        panel.w
    } else {
        panel.h
    };
    let max_w = (max_run - 2.0 * DOCK_TOOLTIP_PAD).max(1.0) as usize;

    let mut tip = state.dock_tooltip.lock();
    let stale = match &*tip {
        Some(t) => t.text != text,
        None => true,
    };
    if stale {
        let Some((buffer, w, h)) =
            rasterize_label(font, text, SWITCHER_LABEL_PX, SWITCHER_LABEL_RGB, max_w)
        else {
            return;
        };
        *tip = Some(crate::state::DockTooltip {
            text: text.to_string(),
            buffer,
            w,
            h,
        });
    }
    let Some(entry) = tip.as_ref() else { return };

    let bg_w = entry.w as f32 + 2.0 * DOCK_TOOLTIP_PAD;
    let bg_h = entry.h as f32 + 2.0 * DOCK_TOOLTIP_PAD;
    // Place the plaque on the panel's *inner* side (toward screen
    // centre). For a horizontal dock it goes above/below the bar and
    // is centred on the tile's column; for a vertical dock it goes
    // left/right of the bar and is centred on the tile's row. Either
    // way it's clamped so the whole plaque stays alongside the bar.
    let (bg_x, bg_y) = match state.config.dock_edge {
        crate::config::DockEdge::Bottom => {
            let lo = panel.x;
            let hi = (panel.x + panel.w - bg_w).max(lo);
            (
                (anchor_cx - bg_w / 2.0).clamp(lo, hi),
                panel.y - DOCK_TOOLTIP_GAP - bg_h,
            )
        }
        crate::config::DockEdge::Top => {
            let lo = panel.x;
            let hi = (panel.x + panel.w - bg_w).max(lo);
            (
                (anchor_cx - bg_w / 2.0).clamp(lo, hi),
                panel.y + panel.h + DOCK_TOOLTIP_GAP,
            )
        }
        crate::config::DockEdge::Left => {
            let lo = panel.y;
            let hi = (panel.y + panel.h - bg_h).max(lo);
            (
                panel.x + panel.w + DOCK_TOOLTIP_GAP,
                (anchor_cy - bg_h / 2.0).clamp(lo, hi),
            )
        }
        crate::config::DockEdge::Right => {
            let lo = panel.y;
            let hi = (panel.y + panel.h - bg_h).max(lo);
            (
                panel.x - DOCK_TOOLTIP_GAP - bg_w,
                (anchor_cy - bg_h / 2.0).clamp(lo, hi),
            )
        }
    };
    let bg = Rect::new(bg_x, bg_y, bg_w, bg_h);

    // Text element (front), then the plaque behind it.
    let scale = output_scale as f32;
    let phys = Point::<f64, Physical>::from((
        ((bg_x + DOCK_TOOLTIP_PAD - off_x as f32) * scale) as f64,
        ((bg_y + DOCK_TOOLTIP_PAD - off_y as f32) * scale) as f64,
    ));
    if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys,
        &entry.buffer,
        Some(1.0),
        None,
        None,
        Kind::Unspecified,
    ) {
        out.push(BacakElements::Memory(el));
    }
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            bg,
            DOCK_TOOLTIP_BG,
            DOCK_TOOLTIP_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(bg, DOCK_TOOLTIP_BG, output_scale, off_x, off_y)),
    }
}

/// Push the hover-dwell window thumbnail: a dark rounded plaque with a
/// live, scaled-to-fit preview of window `id`'s surface, placed on the
/// same side of the panel as the text tooltip would be. Front-first
/// push order: surface elements on top, plaque behind. Replaces the
/// tooltip while shown so the user sees one thing, not two.
#[allow(clippy::too_many_arguments)]
fn dock_thumb_elements(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    anchor_cx: f32,
    anchor_cy: f32,
    panel: Rect,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let bg_w = DOCK_THUMB_W;
    let bg_h = DOCK_THUMB_H;
    let (bg_x, bg_y) = match state.config.dock_edge {
        crate::config::DockEdge::Bottom => {
            let lo = panel.x;
            let hi = (panel.x + panel.w - bg_w).max(lo);
            (
                (anchor_cx - bg_w / 2.0).clamp(lo, hi),
                panel.y - DOCK_TOOLTIP_GAP - bg_h,
            )
        }
        crate::config::DockEdge::Top => {
            let lo = panel.x;
            let hi = (panel.x + panel.w - bg_w).max(lo);
            (
                (anchor_cx - bg_w / 2.0).clamp(lo, hi),
                panel.y + panel.h + DOCK_TOOLTIP_GAP,
            )
        }
        crate::config::DockEdge::Left => {
            let lo = panel.y;
            let hi = (panel.y + panel.h - bg_h).max(lo);
            (
                panel.x + panel.w + DOCK_TOOLTIP_GAP,
                (anchor_cy - bg_h / 2.0).clamp(lo, hi),
            )
        }
        crate::config::DockEdge::Right => {
            let lo = panel.y;
            let hi = (panel.y + panel.h - bg_h).max(lo);
            (
                panel.x - DOCK_TOOLTIP_GAP - bg_w,
                (anchor_cy - bg_h / 2.0).clamp(lo, hi),
            )
        }
    };
    let bg = Rect::new(bg_x, bg_y, bg_w, bg_h);
    let surface_area = Rect::new(
        bg_x + DOCK_THUMB_PAD,
        bg_y + DOCK_THUMB_PAD,
        (bg_w - 2.0 * DOCK_THUMB_PAD).max(1.0),
        (bg_h - 2.0 * DOCK_THUMB_PAD).max(1.0),
    );

    // Surface preview pushed first (front), plaque behind.
    for el in surface_fit_into_area(
        state,
        renderer,
        id,
        surface_area,
        1.0,
        output_scale,
        off_x,
        off_y,
    ) {
        out.push(el);
    }
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            bg,
            DOCK_TOOLTIP_BG,
            DOCK_TOOLTIP_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(bg, DOCK_TOOLTIP_BG, output_scale, off_x, off_y)),
    }
}

/// Push the right-click context menu for this output, if any: dark
/// rounded plaque with one row per item, hover-highlighted under the
/// pointer. Labels rasterise fresh per frame (the menu is short-lived
/// and small, so the per-frame upload is bounded). Push order is
/// front-first: text on top, hover chip behind it, plaque last.
fn render_dock_menu(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(menu) = state.dock_menu.as_ref() else { return };
    if menu.output != output {
        return;
    }
    let rect = menu.rect;
    let (px, py) = state.pointer_position;
    let (px, py) = (px as f32, py as f32);
    let hover_idx = if px >= rect.x
        && px <= rect.x + rect.w
        && py >= rect.y
        && py <= rect.y + rect.h
    {
        let row_y = py - rect.y - crate::state::DOCK_MENU_PAD;
        if row_y >= 0.0 {
            let idx = (row_y / crate::state::DOCK_MENU_ROW_H) as usize;
            (idx < menu.items.len()).then_some(idx)
        } else {
            None
        }
    } else {
        None
    };

    let font = state.text.as_ref();
    let row_inset = 10.0;
    for (i, item) in menu.items.iter().enumerate() {
        let row_rect = Rect::new(
            rect.x + crate::state::DOCK_MENU_PAD,
            rect.y
                + crate::state::DOCK_MENU_PAD
                + i as f32 * crate::state::DOCK_MENU_ROW_H,
            rect.w - 2.0 * crate::state::DOCK_MENU_PAD,
            crate::state::DOCK_MENU_ROW_H,
        );
        if let Some(font) = font {
            let max_w = (row_rect.w - 2.0 * row_inset).max(1.0) as usize;
            if let Some((buffer, _w, h)) =
                rasterize_label(font, &item.label, SWITCHER_LABEL_PX, SWITCHER_LABEL_RGB, max_w)
            {
                let text_x = row_rect.x + row_inset;
                let text_y = row_rect.y
                    + (crate::state::DOCK_MENU_ROW_H - h as f32) / 2.0;
                let scale = output_scale as f32;
                let phys = Point::<f64, Physical>::from((
                    ((text_x - off_x as f32) * scale) as f64,
                    ((text_y - off_y as f32) * scale) as f64,
                ));
                if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    phys,
                    &buffer,
                    Some(1.0),
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    out.push(BacakElements::Memory(el));
                }
            }
        }
        if hover_idx == Some(i) {
            out.push(solid_element(
                row_rect,
                DOCK_HOVER_COLOR,
                output_scale,
                off_x,
                off_y,
            ));
        }
    }
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            rect,
            DOCK_TOOLTIP_BG,
            DOCK_TOOLTIP_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(
            rect,
            DOCK_TOOLTIP_BG,
            output_scale,
            off_x,
            off_y,
        )),
    }
}

/// Render the Android-style floating action menu: a horizontal strip of
/// touch-friendly buttons (Copy | Paste | Select all | Search) hovering near
/// the long-press point. Geometry comes straight from the precomputed
/// [`crate::state::FloatingMenu`] (same rects the hit-test uses). Front-to-back
/// push order mirrors the dock menu: each label first, then the hover chip
/// behind the pointer's button, then the rounded plaque last.
pub(crate) fn render_floating_menu(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(menu) = state.floating_menu.as_ref() else { return };
    if menu.output != output {
        return;
    }

    // Pointer hover (mouse only; touch never hovers) → which button to chip.
    let (px, py) = state.pointer_position;
    let hover_idx = menu.item_at(px as f32, py as f32);

    let font = state.text.as_ref();
    for (i, (item, btn)) in menu.items.iter().zip(menu.buttons.iter()).enumerate() {
        // Icon glyph, centred near the top of the button.
        let isz = crate::state::SEL_MENU_ICON_PX;
        let icon_rect = Rect::new(btn.x + (btn.w - isz) / 2.0, btn.y + 6.0, isz, isz);
        if let Some(el) =
            app_icon_element(state, renderer, item.icon, icon_rect, 1.0, output_scale, off_x, off_y)
        {
            out.push(el);
        }
        // Label below the icon.
        if let Some(font) = font {
            let max_w = (btn.w - 2.0 * crate::state::SEL_MENU_BTN_PAD).max(1.0) as usize;
            if let Some((buffer, w, _h)) = rasterize_label(
                font,
                &item.label,
                crate::state::SEL_MENU_LABEL_PX,
                SWITCHER_LABEL_RGB,
                max_w,
            ) {
                let text_x = btn.x + (btn.w - w as f32) / 2.0;
                let text_y = btn.y + 6.0 + isz + 3.0;
                let scale = output_scale as f32;
                let phys = Point::<f64, Physical>::from((
                    ((text_x - off_x as f32) * scale) as f64,
                    ((text_y - off_y as f32) * scale) as f64,
                ));
                if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    phys,
                    &buffer,
                    Some(1.0),
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    out.push(BacakElements::Memory(el));
                }
            }
        }
        if hover_idx == Some(i) {
            out.push(solid_element(*btn, DOCK_HOVER_COLOR, output_scale, off_x, off_y));
        }
    }

    // Rounded background plaque, furthest back of the menu's own elements.
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            menu.rect,
            DOCK_TOOLTIP_BG,
            DOCK_TOOLTIP_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(menu.rect, DOCK_TOOLTIP_BG, output_scale, off_x, off_y)),
    }
}

/// Tier A native text panel: rounded plaque, the cached glyph bitmap, the
/// selection highlight, and the two draggable handles. Front-to-back push:
/// handles, text, highlight, plaque — so handles sit above the text and the
/// highlight tints behind the glyphs.
pub(crate) fn render_text_panel(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(panel) = state.text_panel.as_ref() else { return };
    if panel.output != output {
        return;
    }
    let (ox, oy) = panel.text_origin;

    // Handles (front-most) — a round knob at each end of the selection.
    if let Some((s, e)) = panel.native.handle_points() {
        let r = crate::state::SEL_HANDLE_R;
        for (hx, hy) in [s, e] {
            let knob = Rect::new(ox + hx - r, oy + hy - r, 2.0 * r, 2.0 * r);
            match card_program(renderer) {
                Some(p) => out.push(switcher_card_element(
                    p, knob, SEL_HANDLE_COLOR, r, false, None, 1.0, output_scale, off_x, off_y,
                )),
                None => out.push(solid_element(knob, SEL_HANDLE_COLOR, output_scale, off_x, off_y)),
            }
        }
    }

    // Text bitmap — the buffer is cached on the panel (built once per layout),
    // so the GPU texture uploads once and is reused; we only build the render
    // element each frame.
    if let Some((buffer, _w, _h)) = panel.bitmap.as_ref() {
        let scale = output_scale as f32;
        let phys = Point::<f64, Physical>::from((
            ((ox - off_x as f32) * scale) as f64,
            ((oy - off_y as f32) * scale) as f64,
        ));
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            phys,
            buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        ) {
            out.push(BacakElements::Memory(el));
        }
    }

    // Selection highlight (behind the text) — one rect per visual row.
    for hr in panel.native.highlight_rects() {
        let gr = Rect::new(ox + hr.x, oy + hr.y, hr.w, hr.h);
        out.push(solid_element(gr, SEL_HIGHLIGHT, output_scale, off_x, off_y));
    }

    // Plaque (back-most).
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p,
            panel.rect,
            SEL_PANEL_BG,
            SEL_PANEL_RADIUS,
            false,
            None,
            1.0,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(panel.rect, SEL_PANEL_BG, output_scale, off_x, off_y)),
    }
}

// --- On-screen keyboard palette --------------------------------------------
const OSK_PANEL_BG: Color32F = Color32F::new(0.07, 0.09, 0.13, 0.96);
const OSK_KEY_BG: Color32F = Color32F::new(0.18, 0.21, 0.27, 1.0);
const OSK_KEY_ACTION_BG: Color32F = Color32F::new(0.12, 0.15, 0.20, 1.0);
const OSK_KEY_PRESSED: Color32F = Color32F::new(0.30, 0.55, 0.95, 1.0);
const OSK_KEY_ACTIVE_MOD: Color32F = Color32F::new(0.22, 0.38, 0.62, 1.0);
const OSK_TITLE_BG: Color32F = Color32F::new(0.10, 0.13, 0.18, 1.0);
const OSK_GRIP: Color32F = Color32F::new(0.45, 0.50, 0.60, 1.0);
const OSK_LABEL_RGB: [u8; 3] = [232, 236, 244];
const OSK_PANEL_RADIUS: f32 = 16.0;
const OSK_KEY_RADIUS: f32 = 9.0;
const OSK_KEY_LABEL_PX: f32 = 19.0;

/// Draw the Desktop Settings panel.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_desktop_settings(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(ds) = state.desktop_settings.as_ref() else { return };
    if ds.output != output { return; }

    let p = ds.panel;
    let panel_bg  = Color32F::new(0.10, 0.12, 0.18, 0.96);
    let section_bg = Color32F::new(0.14, 0.17, 0.24, 0.92);
    let row_hover  = Color32F::new(1.0,  1.0,  1.0,  0.10);
    let accent     = SWITCHER_RING_COLOR;
    let toggle_off = Color32F::new(1.0,  1.0,  1.0,  0.18);
    let ok_col     = Color32F::new(0.25, 0.72, 0.48, 0.95);
    let cancel_col = Color32F::new(1.0,  1.0,  1.0,  0.12);
    let field_bg   = Color32F::new(1.0,  1.0,  1.0,  0.08);
    let shadow     = Color32F::new(0.0,  0.0,  0.0,  0.55);
    const TITLE_H: f32 = 38.0;

    // ── Pass 1: foreground (labels, knobs, borders) — pushed first = topmost ──

    // Title + status labels.
    cc_blit_label(out, renderer, &ds.title, p.x + 16.0, p.y + (TITLE_H - 18.0) / 2.0, output_scale, off_x, off_y);
    if ds.status.is_some() {
        cc_blit_label(out, renderer, &ds.status, p.x + p.w - 180.0, p.y + 10.0, output_scale, off_x, off_y);
    }

    // Overlays (auth / entry) — foreground pass.
    if matches!(ds.mode, crate::state::DsMode::Auth { .. }) {
        ds_overlay_fg(out, renderer, p, ds.auth_field, &ds.auth_pw_label,
            &ds.auth_title_label, ds.auth_cancel_rect, &ds.auth_cancel_label,
            ds.auth_ok_rect, &ds.auth_ok_label, output_scale, off_x, off_y);
    }
    if matches!(ds.mode, crate::state::DsMode::HostnameEntry { .. } | crate::state::DsMode::PwChange { .. } | crate::state::DsMode::WallpaperImageEntry) {
        ds_overlay_fg(out, renderer, p, ds.entry_field, &ds.entry_field_label,
            &ds.entry_title_label, ds.entry_cancel_rect, &ds.entry_cancel_label,
            ds.entry_ok_rect, &ds.entry_ok_label, output_scale, off_x, off_y);
    }

    // Row foreground (labels, toggle knobs, swatch borders).
    if matches!(ds.mode, crate::state::DsMode::Main) {
        for row in &ds.rows {
            let r = row.rect;
            if row.swatch.is_some() {
                // Selected swatch: green border drawn on top of the swatch card.
                if row.toggled {
                    let border = Color32F::new(0.35, 0.78, 0.55, 1.0);
                    let bt = 2.5_f32;
                    out.push(solid_element(Rect::new(r.x, r.y, r.w, bt), border, output_scale, off_x, off_y));
                    out.push(solid_element(Rect::new(r.x, r.y + r.h - bt, r.w, bt), border, output_scale, off_x, off_y));
                    out.push(solid_element(Rect::new(r.x, r.y, bt, r.h), border, output_scale, off_x, off_y));
                    out.push(solid_element(Rect::new(r.x + r.w - bt, r.y, bt, r.h), border, output_scale, off_x, off_y));
                }
                continue;
            }
            let lh = row.label.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(16.0);
            if !row.is_toggle && row.value_label.is_none() {
                // Section header: label only, no card background.
                cc_blit_label(out, renderer, &row.label, r.x, r.y + (r.h - lh) / 2.0, output_scale, off_x, off_y);
                continue;
            }
            // Regular row: label + (toggle knob | value label).
            cc_blit_label(out, renderer, &row.label, r.x + 14.0, r.y + (r.h - lh) / 2.0, output_scale, off_x, off_y);
            if row.is_toggle {
                let pill_w = 44.0_f32;
                let pill_h = 24.0_f32;
                let pill_x = r.x + r.w - pill_w - 14.0;
                let pill_y = r.y + (r.h - pill_h) / 2.0;
                let knob   = pill_h - 4.0;
                let knob_x = if row.toggled { pill_x + pill_w - knob - 2.0 } else { pill_x + 2.0 };
                cc_card(out, renderer, Rect::new(knob_x, pill_y + 2.0, knob, knob), Color32F::new(1.0, 1.0, 1.0, 0.96), knob / 2.0, output_scale, off_x, off_y);
            } else {
                let vw = row.value_label.as_ref().map(|(_, w, _)| *w as f32).unwrap_or(0.0);
                let vh = row.value_label.as_ref().map(|(_, _, h)| *h as f32).unwrap_or(0.0);
                cc_blit_label(out, renderer, &row.value_label, r.x + r.w - vw - 30.0, r.y + (r.h - vh) / 2.0, output_scale, off_x, off_y);
            }
        }
    }

    // ── Pass 2: mid-ground (row cards, toggle pill tracks, swatch fills) ──

    // Overlay card backgrounds.
    if matches!(ds.mode, crate::state::DsMode::Auth { .. }) {
        ds_overlay_bg(out, renderer, p, field_bg, cancel_col, ok_col, output_scale, off_x, off_y,
            ds.auth_field, ds.auth_cancel_rect, ds.auth_ok_rect);
    }
    if matches!(ds.mode, crate::state::DsMode::HostnameEntry { .. } | crate::state::DsMode::PwChange { .. } | crate::state::DsMode::WallpaperImageEntry) {
        ds_overlay_bg(out, renderer, p, field_bg, cancel_col, ok_col, output_scale, off_x, off_y,
            ds.entry_field, ds.entry_cancel_rect, ds.entry_ok_rect);
    }

    // Row card backgrounds.
    if matches!(ds.mode, crate::state::DsMode::Main) {
        for row in &ds.rows {
            let r = row.rect;
            if let Some(color) = row.swatch {
                let c = Color32F::new(color[0] as f32 / 255.0, color[1] as f32 / 255.0, color[2] as f32 / 255.0, 1.0);
                cc_card(out, renderer, r, c, 8.0, output_scale, off_x, off_y);
                continue;
            }
            if !row.is_toggle && row.value_label.is_none() { continue; } // header: no card
            cc_card(out, renderer, r, row_hover, 8.0, output_scale, off_x, off_y);
            if row.is_toggle {
                let pill_w = 44.0_f32;
                let pill_h = 24.0_f32;
                let pill_col = if row.toggled { accent } else { toggle_off };
                let pill_x = r.x + r.w - pill_w - 14.0;
                let pill_y = r.y + (r.h - pill_h) / 2.0;
                cc_card(out, renderer, Rect::new(pill_x, pill_y, pill_w, pill_h), pill_col, pill_h / 2.0, output_scale, off_x, off_y);
            }
        }
    }

    // ── Pass 3: title bar + panel background + drop shadow — furthest back ──
    cc_card(out, renderer, Rect::new(p.x, p.y, p.w, TITLE_H), section_bg, 16.0, output_scale, off_x, off_y);
    cc_card(out, renderer, p, panel_bg, 16.0, output_scale, off_x, off_y);
    out.push(solid_element(Rect::new(p.x + 4.0, p.y + 6.0, p.w, p.h), shadow, output_scale, off_x, off_y));
}

/// Overlay foreground pass: labels on top.
#[allow(clippy::too_many_arguments)]
fn ds_overlay_fg(
    out: &mut Vec<BacakElements>,
    renderer: &mut GlesRenderer,
    panel: Rect,
    field: Rect,
    field_label: &Option<(MemoryRenderBuffer, usize, usize)>,
    title_label: &Option<(MemoryRenderBuffer, usize, usize)>,
    cancel_rect: Rect,
    cancel_label: &Option<(MemoryRenderBuffer, usize, usize)>,
    ok_rect: Rect,
    ok_label: &Option<(MemoryRenderBuffer, usize, usize)>,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) {
    let aw = panel.w - 40.0;
    let ah = 180.0;
    let ax = panel.x + (panel.w - aw) / 2.0;
    let ay = panel.y + (panel.h - ah) / 2.0;
    cc_blit_label(out, renderer, title_label, ax + 16.0, ay + 16.0, output_scale, off_x, off_y);
    if let Some((_, _, lh)) = field_label {
        let lh = *lh as f32;
        cc_blit_label(out, renderer, field_label, field.x + 10.0, field.y + (field.h - lh) / 2.0, output_scale, off_x, off_y);
    }
    let btn_y = |r: Rect| r.y + (r.h - 16.0) / 2.0;
    cc_blit_label(out, renderer, cancel_label, cancel_rect.x + 10.0, btn_y(cancel_rect), output_scale, off_x, off_y);
    cc_blit_label(out, renderer, ok_label, ok_rect.x + 10.0, btn_y(ok_rect), output_scale, off_x, off_y);
}

/// Overlay background pass: card + field + buttons — pushed after foreground.
#[allow(clippy::too_many_arguments)]
fn ds_overlay_bg(
    out: &mut Vec<BacakElements>,
    renderer: &mut GlesRenderer,
    panel: Rect,
    field_bg: Color32F,
    cancel_col: Color32F,
    ok_col: Color32F,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    field: Rect,
    cancel_rect: Rect,
    ok_rect: Rect,
) {
    let aw = panel.w - 40.0;
    let ah = 180.0;
    let ax = panel.x + (panel.w - aw) / 2.0;
    let ay = panel.y + (panel.h - ah) / 2.0;
    cc_card(out, renderer, field, field_bg, 8.0, output_scale, off_x, off_y);
    cc_card(out, renderer, cancel_rect, cancel_col, 8.0, output_scale, off_x, off_y);
    cc_card(out, renderer, ok_rect, ok_col, 8.0, output_scale, off_x, off_y);
    cc_card(out, renderer, Rect::new(ax, ay, aw, ah), Color32F::new(0.08, 0.10, 0.16, 0.98), 12.0, output_scale, off_x, off_y);
}

/// Draw the on-screen keyboard: rounded panel + a draggable title strip +
/// every key as a rounded card with its glyph, pressed/active-modifier tints,
/// on the output it's bound to. Topmost compositor chrome (drawn after the
/// dock so it floats over everything, independent of app windows). Geometry is
/// output-local (0-based) from [`OskController`]; we add the output origin
/// (`off_*`) to lift it into the global space the element helpers expect.
pub(crate) fn render_osk(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    if !state.osk.is_visible() {
        return;
    }
    // Only draw on the output the OSK is bound to (multi-monitor: the screen
    // holding the focused field). Fall back to the primary if unbound.
    match state.osk.bound_output() {
        Some(o) if o == output => {}
        Some(_) => return,
        None => {
            if state.wm.primary_output() != Some(output) {
                return;
            }
        }
    }

    let panel = state.osk.panel_rect();
    let g = |x: f32, y: f32, w: f32, h: f32| Rect::new(x + off_x as f32, y + off_y as f32, w, h);

    // Keys are rendered front-to-back: we push the panel last so it sits behind
    // (Bacak's element list is top-first). Push keys + labels first. Use the
    // dedicated symbol font (DejaVu) so control glyphs (⇧ ↵ ⌫ arrows) render
    // instead of tofu boxes; fall back to the general UI font.
    let font = state.osk_font.as_ref().or(state.text.as_ref());
    for key in state.osk.render_keys() {
        let bg = if key.pressed {
            OSK_KEY_PRESSED
        } else if key.active_mod {
            OSK_KEY_ACTIVE_MOD
        } else if key.is_action {
            OSK_KEY_ACTION_BG
        } else {
            OSK_KEY_BG
        };
        // Inset the key cap slightly inside its cell for breathing room.
        let inset = 2.0;
        let cap = g(key.x + inset, key.y + inset, key.w - 2.0 * inset, key.h - 2.0 * inset);

        // Label: the brand logo (Win key), a colour emoji, else a fontdue glyph.
        let mut drew = false;
        if key.is_logo {
            let px = (key.h * 0.74 * OSK_EMOJI_SS as f32).clamp(16.0, 160.0) as u32;
            if let Some((buf, sz)) = win_logo_buffer(px) {
                let lx = key.x + (key.w - sz) / 2.0 + off_x as f32;
                let ly = key.y + (key.h - sz) / 2.0 + off_y as f32;
                cc_blit_label(
                    out,
                    renderer,
                    &Some((buf, sz as usize, sz as usize)),
                    lx,
                    ly,
                    output_scale,
                    off_x,
                    off_y,
                );
                drew = true;
            }
        }
        if !drew && crate::emoji::leading_emoji(&key.label).is_some() {
            if let Some((buf, sz)) = osk_emoji_buffer(state, &key.label) {
                let lx = key.x + (key.w - sz) / 2.0 + off_x as f32;
                let ly = key.y + (key.h - sz) / 2.0 + off_y as f32;
                cc_blit_label(
                    out,
                    renderer,
                    &Some((buf, sz as usize, sz as usize)),
                    lx,
                    ly,
                    output_scale,
                    off_x,
                    off_y,
                );
                drew = true;
            }
        }
        // Rasterise + centre. Spaces / empty draw nothing.
        if !drew {
            if let Some(f) = font {
                let trimmed = key.label.trim();
                if !trimmed.is_empty() {
                    if let Some((buf, lw, lh)) =
                        rasterize_label(f, trimmed, OSK_KEY_LABEL_PX, OSK_LABEL_RGB, key.w as usize)
                    {
                        let lx = key.x + (key.w - lw as f32) / 2.0 + off_x as f32;
                        let ly = key.y + (key.h - lh as f32) / 2.0 + off_y as f32;
                        cc_blit_label(out, renderer, &Some((buf, lw, lh)), lx, ly, output_scale, off_x, off_y);
                    }
                }
            }
        }

        match card_program(renderer) {
            Some(p) => out.push(switcher_card_element(
                p, cap, bg, OSK_KEY_RADIUS, false, None, 1.0, output_scale, off_x, off_y,
            )),
            None => out.push(solid_element(cap, bg, output_scale, off_x, off_y)),
        }
    }

    // Title-strip labels (front-most): the active layout badge (e.g. "TR") at
    // the left and a close "✕" at the right.
    let th = crate::input::OSK_TITLE_H;
    if let Some(f) = font {
        let badge = state.osk.layout_id().short_label();
        if let Some((buf, lw, lh)) = rasterize_label(f, badge, 15.0, OSK_LABEL_RGB, 60) {
            let lx = panel.x + 12.0 + off_x as f32;
            let ly = panel.y + (th - lh as f32) / 2.0 + off_y as f32;
            cc_blit_label(out, renderer, &Some((buf, lw, lh)), lx, ly, output_scale, off_x, off_y);
        }
        if let Some((buf, lw, lh)) = rasterize_label(f, "✕", 15.0, OSK_LABEL_RGB, 40) {
            let lx = panel.x + panel.w - crate::input::OSK_CLOSE_W / 2.0 - lw as f32 / 2.0 + off_x as f32;
            let ly = panel.y + (th - lh as f32) / 2.0 + off_y as f32;
            cc_blit_label(out, renderer, &Some((buf, lw, lh)), lx, ly, output_scale, off_x, off_y);
        }
    }

    // Title-strip grip: three short bars centred in the drag handle.
    let grip_w = 46.0;
    let grip_y = panel.y + th / 2.0 - 2.0;
    let grip = g(panel.x + (panel.w - grip_w) / 2.0, grip_y, grip_w, 4.0);
    out.push(solid_element(grip, OSK_GRIP, output_scale, off_x, off_y));

    // Title strip background.
    let title = g(panel.x, panel.y, panel.w, th);
    out.push(solid_element(title, OSK_TITLE_BG, output_scale, off_x, off_y));

    // Panel backdrop (back-most): a rounded card with a soft drop shadow.
    let panel_rect = g(panel.x, panel.y, panel.w, panel.h);
    match card_program(renderer) {
        Some(p) => out.push(switcher_card_element(
            p, panel_rect, OSK_PANEL_BG, OSK_PANEL_RADIUS, false, None, 1.0, output_scale, off_x, off_y,
        )),
        None => out.push(solid_element(panel_rect, OSK_PANEL_BG, output_scale, off_x, off_y)),
    }
}

/// Tier C overlay: draw the AT-SPI-derived selection (handles + highlight)
/// *over a foreign client*. No text/plaque — the app draws its own glyphs; we
/// only tint the selection and add Android handles. Screen-space already, so
/// no per-output gate (off-output parts clip).
pub(crate) fn render_atspi_selection(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(sel) = state.atspi_selection.as_ref() else { return };
    // We deliberately do NOT draw our own handle knobs here: an AT-SPI-aware app
    // (Firefox, LibreOffice) already paints its native selection handles
    // (teardrops) at the same endpoints, so drawing ours too gave two overlapping
    // handles (round + teardrop). The drag-grab logic in `atspi_handle_press`
    // still uses `sel.handles`, co-located with the app's teardrop, so dragging
    // the visible handle still extends the selection over AT-SPI.
    let _ = renderer;
    for hr in &sel.highlight {
        out.push(solid_element(*hr, SEL_HIGHLIGHT, output_scale, off_x, off_y));
    }
}

/// Drag-reorder insertion marker rect for snap `target` (range
/// `0..=pinned_len`). Horizontal docks get a thin vertical bar at the
/// gap between pinned slots; vertical docks get a thin horizontal bar
/// (same idea, axis swapped). Clamped to before the first / after the
/// last pinned tile. `pad` is the layout gap (= pinned tile spacing).
fn dock_insertion_marker_rect(
    edge: crate::config::DockEdge,
    tiles: &[crate::state::DockEntry],
    panel: Rect,
    pinned_len: usize,
    target: usize,
    pad: f32,
) -> Option<Rect> {
    if pinned_len == 0 {
        return None;
    }
    let gap = pad;
    let first = tiles.first()?.rect;
    let last = tiles.get(pinned_len - 1)?.rect;
    if edge.is_horizontal() {
        let x = if target == 0 {
            first.x - gap / 2.0
        } else if target >= pinned_len {
            last.x + last.w + gap / 2.0
        } else {
            let a = tiles.get(target - 1)?.rect;
            let b = tiles.get(target)?.rect;
            (a.x + a.w + b.x) / 2.0
        };
        Some(Rect::new(
            x - DOCK_INSERT_MARK_W / 2.0,
            panel.y + panel.h * 0.15,
            DOCK_INSERT_MARK_W,
            panel.h * 0.7,
        ))
    } else {
        let y = if target == 0 {
            first.y - gap / 2.0
        } else if target >= pinned_len {
            last.y + last.h + gap / 2.0
        } else {
            let a = tiles.get(target - 1)?.rect;
            let b = tiles.get(target)?.rect;
            (a.y + a.h + b.y) / 2.0
        };
        Some(Rect::new(
            panel.x + panel.w * 0.15,
            y - DOCK_INSERT_MARK_W / 2.0,
            panel.w * 0.7,
            DOCK_INSERT_MARK_W,
        ))
    }
}

/// Focus / urgency indicator bar for the tile, placed on its *inner*
/// edge (toward the screen centre, opposite the dock edge). Horizontal
/// docks get a short horizontal accent bar; vertical docks get a short
/// vertical bar — same length fraction, axis swapped.
fn dock_indicator_bar(edge: crate::config::DockEdge, tile: Rect, pad: f32) -> Rect {
    match edge {
        crate::config::DockEdge::Bottom => {
            let bw = tile.w * DOCK_INDICATOR_W_FRAC;
            Rect::new(
                tile.x + (tile.w - bw) / 2.0,
                tile.y + tile.h + (pad - DOCK_INDICATOR_H) / 2.0,
                bw,
                DOCK_INDICATOR_H,
            )
        }
        crate::config::DockEdge::Top => {
            let bw = tile.w * DOCK_INDICATOR_W_FRAC;
            Rect::new(
                tile.x + (tile.w - bw) / 2.0,
                tile.y - pad + (pad - DOCK_INDICATOR_H) / 2.0,
                bw,
                DOCK_INDICATOR_H,
            )
        }
        crate::config::DockEdge::Left => {
            let bh = tile.h * DOCK_INDICATOR_W_FRAC;
            Rect::new(
                tile.x + tile.w + (pad - DOCK_INDICATOR_H) / 2.0,
                tile.y + (tile.h - bh) / 2.0,
                DOCK_INDICATOR_H,
                bh,
            )
        }
        crate::config::DockEdge::Right => {
            let bh = tile.h * DOCK_INDICATOR_W_FRAC;
            Rect::new(
                tile.x - pad + (pad - DOCK_INDICATOR_H) / 2.0,
                tile.y + (tile.h - bh) / 2.0,
                DOCK_INDICATOR_H,
                bh,
            )
        }
    }
}

/// Multi-window count dots: `n` rects in the panel's *outer* padding
/// strip (opposite the indicator), arranged along the bar's row axis
/// and centred over the tile.
fn dock_count_dot_rects(
    edge: crate::config::DockEdge,
    tile: Rect,
    pad: f32,
    n: usize,
) -> Vec<Rect> {
    let run = n as f32 * DOCK_DOT_SIZE + (n as f32 - 1.0).max(0.0) * DOCK_DOT_GAP;
    let step = DOCK_DOT_SIZE + DOCK_DOT_GAP;
    let mut out = Vec::with_capacity(n);
    match edge {
        crate::config::DockEdge::Bottom => {
            let x0 = tile.x + (tile.w - run) / 2.0;
            let y = tile.y - pad + (pad - DOCK_DOT_SIZE) / 2.0;
            for k in 0..n {
                out.push(Rect::new(
                    x0 + k as f32 * step,
                    y,
                    DOCK_DOT_SIZE,
                    DOCK_DOT_SIZE,
                ));
            }
        }
        crate::config::DockEdge::Top => {
            let x0 = tile.x + (tile.w - run) / 2.0;
            let y = tile.y + tile.h + (pad - DOCK_DOT_SIZE) / 2.0;
            for k in 0..n {
                out.push(Rect::new(
                    x0 + k as f32 * step,
                    y,
                    DOCK_DOT_SIZE,
                    DOCK_DOT_SIZE,
                ));
            }
        }
        crate::config::DockEdge::Left => {
            let x = tile.x + tile.w + (pad - DOCK_DOT_SIZE) / 2.0;
            let y0 = tile.y + (tile.h - run) / 2.0;
            for k in 0..n {
                out.push(Rect::new(
                    x,
                    y0 + k as f32 * step,
                    DOCK_DOT_SIZE,
                    DOCK_DOT_SIZE,
                ));
            }
        }
        crate::config::DockEdge::Right => {
            let x = tile.x - pad + (pad - DOCK_DOT_SIZE) / 2.0;
            let y0 = tile.y + (tile.h - run) / 2.0;
            for k in 0..n {
                out.push(Rect::new(
                    x,
                    y0 + k as f32 * step,
                    DOCK_DOT_SIZE,
                    DOCK_DOT_SIZE,
                ));
            }
        }
    }
    out
}

/// Paint every window on `ws` into `out`, in front-to-back order, with
/// every position translated by `(-off_x, -off_y)` so the elements end
/// up in the destination output's surface-local coords. Used directly
/// for static rendering and twice (with different offsets) during a
/// slide.
/// Drain queued dma-buf imports, test-importing each into the live renderer.
///
/// The `DmabufHandler` (on `BacakState`) has no renderer, so it queues buffers
/// in `state.pending_dmabuf`; here — where a backend has a real `GlesRenderer`
/// — we actually try the import. A buffer that imports is accepted
/// (`notifier.successful`); one that doesn't is rejected (`notifier.failed`) so
/// the client renegotiates a format we can sample, instead of rendering black.
/// The test texture is dropped immediately (smithay re-imports per frame); the
/// point is the pass/fail signal. Called once per render tick by both backends.
pub fn process_pending_dmabuf(state: &mut BacakState, renderer: &mut GlesRenderer) {
    use smithay::backend::renderer::ImportDma;
    if state.pending_dmabuf.is_empty() {
        return;
    }
    for (dmabuf, notifier) in std::mem::take(&mut state.pending_dmabuf) {
        match renderer.import_dmabuf(&dmabuf, None) {
            Ok(_tex) => {
                if let Err(err) = notifier.successful::<BacakState>() {
                    tracing::debug!(?err, "dmabuf accepted but client gone");
                }
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "dmabuf test-import failed; rejecting so the client picks another format"
                );
                notifier.failed();
            }
        }
    }
}

/// Fulfil queued `zwlr_screencopy_v1` captures: render the requested output
/// offscreen, read back the region, copy it into the client's SHM buffer, and
/// fire `ready` (or `failed`). SHM only; full readback per request (no damage).
/// Called once per render tick by both backends — like `process_pending_dmabuf`.
pub fn process_pending_screencopy(state: &mut BacakState, renderer: &mut GlesRenderer) {
    if state.pending_screencopy.is_empty() {
        return;
    }
    for req in std::mem::take(&mut state.pending_screencopy) {
        if screencopy_one(state, renderer, &req) {
            // flags before ready, per protocol. We render upright; if a client
            // shows the image upside-down, the fix is `Flags::YInvert` here.
            use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::Flags;
            req.frame.flags(Flags::empty());
            let t = state.start_time.elapsed();
            let secs = t.as_secs();
            req.frame.ready((secs >> 32) as u32, secs as u32, t.subsec_nanos());
        } else {
            req.frame.failed();
        }
    }
}

/// Render one screencopy request into its SHM buffer. Returns false on any
/// failure (the caller then sends `failed`).
fn screencopy_one(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    req: &crate::screencopy::ScreencopyRequest,
) -> bool {
    use smithay::backend::renderer::{Bind, ExportMem};
    let Some(output) = state.outputs.get(&req.output) else { return false };
    let scale = output.current_scale().integer_scale().max(1);
    let Some(mode) = output.current_mode() else { return false };
    let (fw, fh) = (mode.size.w.max(1), mode.size.h.max(1));

    // Full-output scene (windows + dock + cursor — what's actually on screen).
    let scene = build_styled_elements_for_output(state, renderer, scale, req.output);

    let size = Size::<i32, BufferCoord>::from((fw, fh));
    let Ok(mut tex) = renderer.create_buffer(Fourcc::Argb8888, size) else { return false };
    let phys = Size::<i32, Physical>::from((fw, fh));
    // Same desktop backdrop the real frame clears to, so the screenshot matches.
    let clear = Color32F::new(0.024, 0.165, 0.239, 1.0);
    {
        let Ok(mut fb) = renderer.bind(&mut tex) else { return false };
        let mut dt = OutputDamageTracker::new(phys, Scale::from(scale as f64), Transform::Normal);
        if dt.render_output(renderer, &mut fb, 0, &scene, clear).is_err() {
            return false;
        }
    }

    // Read back the requested region (physical px, output-relative).
    let r = req.region;
    let buf_region = Rectangle::<i32, BufferCoord>::new(
        (r.loc.x, r.loc.y).into(),
        (r.size.w.max(1), r.size.h.max(1)).into(),
    );
    let Ok(mapping) = renderer.copy_texture(&tex, buf_region, Fourcc::Argb8888) else {
        return false;
    };
    let Ok(bytes) = renderer.map_texture(&mapping) else { return false };

    let src_stride = (r.size.w.max(1) as usize) * 4;
    let src_rows = r.size.h.max(1) as usize;
    smithay::wayland::shm::with_buffer_contents_mut(&req.buffer, |ptr, len, data| {
        let dst_stride = data.stride.max(0) as usize;
        let dst_rows = data.height.max(0) as usize;
        let copy = src_stride.min(dst_stride);
        let rows = src_rows.min(dst_rows);
        for row in 0..rows {
            let src_off = row * src_stride;
            let dst_off = data.offset.max(0) as usize + row * dst_stride;
            if src_off + copy > bytes.len() || dst_off + copy > len {
                break;
            }
            // SAFETY: bounds checked above; src and dst don't overlap (distinct
            // allocations — GL mapping vs client SHM pool).
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(src_off),
                    ptr.add(dst_off),
                    copy,
                );
            }
        }
    })
    .is_ok()
}

/// Fulfil a queued built-in screenshot (PrintScreen): render the requested
/// output offscreen, read back every pixel, and write a PNG to disk. Mirrors
/// [`screencopy_one`]'s offscreen-render + `ExportMem` readback, then encodes
/// with the `image` crate instead of copying into a client SHM buffer. Called
/// once per render tick by both backends, like [`process_pending_screencopy`].
pub fn process_pending_screenshot(state: &mut BacakState, renderer: &mut GlesRenderer) {
    let Some(req) = state.pending_screenshot.take() else { return };
    // Clear any lingering toast so a *previous* "saved" banner can't end up in
    // this capture (the capture path is the same `build_output_frame`).
    state.toast = None;
    // A window-pick request renders just that window in isolation; otherwise
    // capture the output (whole or a region) from the composited frame.
    let result = match req.window {
        Some(id) => capture_window_png(state, renderer, id),
        None => capture_output_png(state, renderer, req.output, req.region),
    };
    match result {
        Some((path, png)) => {
            // Also put the PNG on the clipboard (image/png) so it can be pasted
            // straight into chats/editors without opening the file.
            state.copy_image_to_clipboard(png);
            // Camera flash — set *after* the capture so it's never in the image.
            // Drawn on this very tick's live frame and faded by tick_animations.
            state.flash = Some((req.output, std::time::Instant::now()));
            // Notify the user it was saved + where (HOME shown as `~`).
            let full = path.display().to_string();
            let friendly = std::env::var_os("HOME")
                .map(|h| h.to_string_lossy().into_owned())
                .and_then(|h| full.strip_prefix(&h).map(|rest| format!("~{rest}")))
                .unwrap_or_else(|| full.clone());
            state.show_toast(req.output, "❏  Ekran görüntüsü kaydedildi", &friendly);
            tracing::info!(path = %path.display(), "screenshot saved + copied to clipboard");
        }
        None => {
            state.show_toast(req.output, "Ekran görüntüsü alınamadı", "");
            tracing::warn!("screenshot capture failed");
        }
    }
}

/// Render `output_id` to an offscreen buffer, read back `region` (WM-global
/// logical px; `None` = whole output), and save it as a PNG. Returns the
/// written path on success. The readback path is identical to [`screencopy_one`];
/// only the destination differs (a file, not a wl_buffer).
fn capture_output_png(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_id: crate::wm::OutputId,
    region: Option<Rect>,
) -> Option<(std::path::PathBuf, Vec<u8>)> {
    use smithay::backend::renderer::{Bind, ExportMem};
    let output = state.outputs.get(&output_id)?;
    let scale = output.current_scale().integer_scale().max(1);
    let mode = output.current_mode()?;
    let (fw, fh) = (mode.size.w.max(1), mode.size.h.max(1));

    // Full-output scene (windows + dock + cursor) — what's actually on screen.
    let scene = build_styled_elements_for_output(state, renderer, scale, output_id);
    let size = Size::<i32, BufferCoord>::from((fw, fh));
    let mut tex = renderer.create_buffer(Fourcc::Argb8888, size).ok()?;
    let phys = Size::<i32, Physical>::from((fw, fh));
    let clear = Color32F::new(0.024, 0.165, 0.239, 1.0);
    {
        let mut fb = renderer.bind(&mut tex).ok()?;
        let mut dt = OutputDamageTracker::new(phys, Scale::from(scale as f64), Transform::Normal);
        dt.render_output(renderer, &mut fb, 0, &scene, clear).ok()?;
    }

    // The readback region in output-local *physical* px. A `region` is given in
    // WM-global logical coords, so subtract the output origin and scale up;
    // clamp to the framebuffer. `None` reads the whole output.
    let ob = state.wm.output(output_id).map(|o| o.bounds);
    let (rx, ry, rw, rh) = match (region, ob) {
        (Some(r), Some(b)) => {
            let lx = ((r.x - b.x).max(0.0) as i32) * scale;
            let ly = ((r.y - b.y).max(0.0) as i32) * scale;
            let lw = (r.w as i32 * scale).clamp(1, (fw - lx).max(1));
            let lh = (r.h as i32 * scale).clamp(1, (fh - ly).max(1));
            (lx.min(fw - 1), ly.min(fh - 1), lw, lh)
        }
        _ => (0, 0, fw, fh),
    };
    let region_rect = Rectangle::<i32, BufferCoord>::new((rx, ry).into(), (rw, rh).into());
    let mapping = renderer.copy_texture(&tex, region_rect, Fourcc::Argb8888).ok()?;
    let bytes = renderer.map_texture(&mapping).ok()?;

    // `map_texture` returns Argb8888 = little-endian 0xAARRGGBB → byte order
    // B,G,R,A. The `image` crate's Rgba8 wants R,G,B,A, so swap B↔R per pixel.
    let mut rgba = bytes.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let img = image::RgbaImage::from_raw(rw as u32, rh as u32, rgba)?;
    save_screenshot(img)
}

/// Render just `id`'s own pixels (content + SSD title bar, no overlapping
/// windows / dock / cursor) to an offscreen buffer sized to the window, read it
/// back, and save as PNG. The transparent clear leaves anything outside the
/// window's surface transparent. Element set mirrors `render_workspace_windows`
/// for one window, positioned so the capture rect's top-left is the buffer
/// origin. This is true window isolation, unlike reading a region of the
/// composited frame (which would include whatever overlaps the window).
fn capture_window_png(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: crate::wm::WindowId,
) -> Option<(std::path::PathBuf, Vec<u8>)> {
    use smithay::backend::renderer::{Bind, ExportMem};
    let win = state.wm.get(id).ok()?;
    let rect = state.window_shot_rect(id)?; // capture rect, WM-global logical
    let output_id = state.wm.output_for_window(id).map(|o| o.id)?;
    let output = state.outputs.get(&output_id)?;
    let scale = output.current_scale().integer_scale().max(1);
    let off_x = rect.x as i32;
    let off_y = rect.y as i32;
    let pw = (rect.w as i32 * scale).max(1);
    let ph = (rect.h as i32 * scale).max(1);

    // Scene = this window's SSD title bar + its surface tree, positioned so the
    // capture rect's top-left maps to the buffer origin. Other windows, popups,
    // dock and cursor are excluded — this is the window itself.
    let wl_surface = state.surface_for_window(id)?;
    let location: Point<i32, smithay::utils::Logical> =
        ((win.geom.x as i32) - off_x, (win.geom.y as i32) - off_y).into();
    let mut scene: Vec<BacakElements> = Vec::new();
    push_window_decoration(state, renderer, &win, scale, off_x, off_y, &mut scene);
    for el in render_elements_from_surface_tree(
        renderer,
        &wl_surface,
        location.to_physical_precise_round::<f64, _>(scale as f64),
        scale as f64,
        1.0,
        Kind::Unspecified,
    ) {
        scene.push(BacakElements::Surface(el));
    }

    let size = Size::<i32, BufferCoord>::from((pw, ph));
    let mut tex = renderer.create_buffer(Fourcc::Argb8888, size).ok()?;
    let phys = Size::<i32, Physical>::from((pw, ph));
    let clear = Color32F::new(0.0, 0.0, 0.0, 0.0); // transparent outside the window
    {
        let mut fb = renderer.bind(&mut tex).ok()?;
        let mut dt = OutputDamageTracker::new(phys, Scale::from(scale as f64), Transform::Normal);
        dt.render_output(renderer, &mut fb, 0, &scene, clear).ok()?;
    }
    let region_rect = Rectangle::<i32, BufferCoord>::new((0, 0).into(), (pw, ph).into());
    let mapping = renderer.copy_texture(&tex, region_rect, Fourcc::Argb8888).ok()?;
    let bytes = renderer.map_texture(&mapping).ok()?;
    let mut rgba = bytes.to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let img = image::RgbaImage::from_raw(pw as u32, ph as u32, rgba)?;
    save_screenshot(img)
}

/// Encode an RGBA image to PNG, write it to a timestamped file under the
/// pictures dir, and return `(path, png_bytes)` so the caller can also put the
/// bytes on the clipboard. Shared by the output and window capture paths.
fn save_screenshot(img: image::RgbaImage) -> Option<(std::path::PathBuf, Vec<u8>)> {
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    let path = screenshot_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, &png).ok()?;
    Some((path, png))
}

/// Where to write a screenshot: `$XDG_PICTURES_DIR`, else `~/Pictures`, else
/// `$HOME`, else `/tmp`; filename `Screenshot_<unixsecs>.png`.
fn screenshot_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_PICTURES_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Pictures"))
        })
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    dir.join(format!("Screenshot_{secs}.png"))
}

/// Push render elements for every xdg_popup (app menu, tooltip, combo-box
/// dropdown) attached to `wl_surface`, drawn on top of that window's content.
///
/// `location` is the window's output-local logical origin — where its root
/// surface `(0,0)` is drawn. Popup positioners are anchored relative to the
/// parent's *window geometry*, not its buffer, so we re-base by the toplevel's
/// geometry offset (`geo_loc`, the CSD shadow margin) exactly as smithay's own
/// `Window` render does: `geo_loc + popup_offset - popup.geometry().loc`.
fn push_window_popups(
    renderer: &mut GlesRenderer,
    wl_surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    location: Point<i32, smithay::utils::Logical>,
    output_scale: i32,
    alpha: f32,
    out: &mut Vec<BacakElements>,
) {
    let geo_loc = smithay::wayland::compositor::with_states(wl_surface, |states| {
        states
            .cached_state
            .get::<smithay::wayland::shell::xdg::SurfaceCachedState>()
            .current()
            .geometry
            .map(|g| g.loc)
            .unwrap_or_default()
    });
    let scale = output_scale as f64;
    for (popup, popup_offset) in
        smithay::desktop::PopupManager::popups_for_surface(wl_surface)
    {
        let offset = geo_loc + popup_offset - popup.geometry().loc;
        let ploc = (location + offset).to_physical_precise_round::<f64, _>(scale);
        if crate::popup_debug() {
            let g = popup.geometry();
            tracing::info!(
                logical = ?(location.x + offset.x, location.y + offset.y),
                size = ?(g.size.w, g.size.h),
                output_scale,
                "POPUP render: drawing popup surface tree"
            );
        }
        for el in render_elements_from_surface_tree(
            renderer,
            popup.wl_surface(),
            ploc,
            Scale::from(scale),
            alpha,
            Kind::Unspecified,
        ) {
            out.push(BacakElements::Surface(el));
        }
    }
}

/// Render active IME candidate popups (input-method-v2) near the text cursor,
/// on top of windows. Position = parent window rect loc + popup-relative
/// location; off-output positions clip naturally. **Coordinates unverified on
/// hardware** (CJK IME only path).
pub(crate) fn render_ime_popups(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let scale = output_scale as f64;
    for popup in &state.ime_popups {
        if !popup.alive() {
            continue;
        }
        let Some(parent) = popup.get_parent() else { continue };
        let base = parent.location.loc + popup.location();
        let loc = Point::<i32, Physical>::from((
            (((base.x - off_x) as f64) * scale).round() as i32,
            (((base.y - off_y) as f64) * scale).round() as i32,
        ));
        for el in render_elements_from_surface_tree(
            renderer,
            popup.wl_surface(),
            loc,
            Scale::from(scale),
            1.0,
            Kind::Unspecified,
        ) {
            out.push(BacakElements::Surface(el));
        }
    }
}

/// Render the wlr-layer-shell surfaces in `want` for `output_id`, at the
/// positions the output's `LayerMap` arranged them (output-local logical →
/// physical). Called twice: `[Overlay, Top]` above the windows, `[Bottom,
/// Background]` behind them. No-op if the output isn't registered.
fn render_layer_set(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_id: OutputId,
    output_scale: i32,
    want: &[smithay::wayland::shell::wlr_layer::Layer],
    out: &mut Vec<BacakElements>,
) {
    let Some(output) = state.outputs.get(&output_id) else { return };
    let map = smithay::desktop::layer_map_for_output(output);
    let scale = output_scale as f64;
    for layer in map.layers() {
        if !want.contains(&layer.layer()) {
            continue;
        }
        let Some(geo) = map.layer_geometry(layer) else { continue };
        // Popups parented to this layer surface (e.g. a panel's menu), on top of
        // the layer's own content. Offset like toplevel popups, anchored at the
        // layer's arranged position.
        for (popup, popup_offset) in
            smithay::desktop::PopupManager::popups_for_surface(layer.wl_surface())
        {
            let off = geo.loc + popup_offset - popup.geometry().loc;
            let ploc = off.to_physical_precise_round::<f64, _>(scale);
            for el in render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                ploc,
                Scale::from(scale),
                1.0,
                Kind::Unspecified,
            ) {
                out.push(BacakElements::Surface(el));
            }
        }
        let loc = geo.loc.to_physical_precise_round::<f64, _>(scale);
        for el in render_elements_from_surface_tree(
            renderer,
            layer.wl_surface(),
            loc,
            Scale::from(scale),
            1.0,
            Kind::Unspecified,
        ) {
            out.push(BacakElements::Surface(el));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_workspace_windows(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    ws: WorkspaceId,
    off_x: i32,
    off_y: i32,
    blur_tex: Option<&GlesTexture>,
    out: &mut Vec<BacakElements>,
) {
    let mut windows = state.wm.windows_on_workspace(ws);
    windows.sort_by_key(|w| w.z);
    windows.reverse();

    // One compile attempt up front; cheap thereafter (thread-local).
    let prog = card_program(renderer);

    // `windows` is sorted ascending-z then reversed, so index 0 is the
    // topmost window — exactly the stack rank the shadow ramp wants.
    for (z_rank, win) in windows.iter().enumerate() {
        // Minimised windows aren't drawn. During the shrink animation
        // the window is still `Floating` (we only flip the state on
        // settle), so it stays visible while it scales down and then
        // cleanly disappears the frame it lands.
        if matches!(win.state, WinState::Minimized) {
            continue;
        }
        // Translate the WM-global position into output-local pixels.
        let location: Point<i32, smithay::utils::Logical> =
            ((win.geom.x as i32) - off_x, (win.geom.y as i32) - off_y).into();

        let Some(wl_surface) = state.surface_for_window(win.id) else {
            continue;
        };

        // App menus / tooltips / dropdowns attached to this window. Pushed
        // first in this window's group so they sit above its content *and* its
        // title bar (and behind any higher window, which was drawn earlier).
        let alpha = if win.focused { 1.0 } else { PASSIVE_WINDOW_ALPHA };
        push_window_popups(renderer, &wl_surface, location, output_scale, alpha, out);

        // Server-side title bar, pushed before this window's content (so it's
        // on top of it) but after higher windows (drawn earlier) — correct
        // z-order when windows overlap. No-op for undecorated windows.
        push_window_decoration(state, renderer, win, output_scale, off_x, off_y, out);

        // Glassmorphism: passive windows are drawn at reduced alpha
        // over a frosted (pre-blurred) crop of the scene behind them —
        // the surface's translucent regions reveal the blur, opaque
        // regions get a subtle tint. The focused window stays crisp at
        // full alpha. (Phase 1/2 of the blur pipeline; the blur source
        // is rendered offscreen by the udev backend.)
        // `alpha` was computed above (shared with the popup pass).
        let surface_elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                &wl_surface,
                location.to_physical_precise_round::<f64, _>(output_scale as f64),
                output_scale as f64,
                alpha,
                Kind::Unspecified,
            );
        for el in surface_elements {
            out.push(BacakElements::Surface(el));
        }

        // Frosted backdrop behind this passive window. Pushed *after*
        // the surface (front-first → it sits behind it). Sampled from
        // the output-sized blurred scene at the window's own rect,
        // clamped to the texture so partially-offscreen windows don't
        // sample garbage.
        if !win.focused {
            if let Some(tex) = blur_tex {
                use smithay::backend::renderer::Texture;
                let ts = tex.size();
                let wx = win.geom.x - off_x as f32;
                let wy = win.geom.y - off_y as f32;
                let cx0 = wx.max(0.0);
                let cy0 = wy.max(0.0);
                let cx1 = (wx + win.geom.w).min(ts.w as f32);
                let cy1 = (wy + win.geom.h).min(ts.h as f32);
                if cx1 > cx0 && cy1 > cy0 {
                    let cw = cx1 - cx0;
                    let ch = cy1 - cy0;
                    let src = Rectangle::<f64, smithay::utils::Logical>::new(
                        Point::from((cx0 as f64, cy0 as f64)),
                        Size::from((cw as f64, ch as f64)),
                    );
                    let loc = Point::<f64, Physical>::from((
                        (cx0 * output_scale as f32) as f64,
                        (cy0 * output_scale as f32) as f64,
                    ));
                    let size = Size::<i32, smithay::utils::Logical>::from((
                        cw as i32, ch as i32,
                    ));
                    out.push(BacakElements::Texture(
                        TextureRenderElement::from_static_texture(
                            Id::new(),
                            renderer.context_id(),
                            loc,
                            tex.clone(),
                            output_scale,
                            Transform::Normal,
                            Some(1.0),
                            Some(src),
                            Some(size),
                            None,
                            Kind::Unspecified,
                        ),
                    ));
                }
            }
        }

        // Soft drop shadow — every window casts one, graded by stack
        // depth: the topmost (z_rank 0, normally the focused window)
        // lifts highest, each window further back is dimmer down to a
        // floor. Pushed last for this window so it's the deepest layer
        // (cast onto whatever is behind it). The card shader draws a
        // gaussian-ish falloff in one pass; with a fully transparent
        // fill only the shadow contributes. Falls back to the old hard
        // solid quad if the shader didn't compile.
        let cfg = &state.config;
        let intensity = (cfg.shadow_top - z_rank as f32 * cfg.shadow_step)
            .max(cfg.shadow_floor);
        match prog.clone() {
            Some(p) => out.push(window_shadow_element(p, win.geom, intensity, output_scale, off_x, off_y)),
            None => out.push(BacakElements::Solid(SolidColorRenderElement::new(
                Id::new(),
                shadow_rect_offset(win.geom, output_scale, off_x, off_y),
                0usize,
                fade_color(SHADOW_COLOR, intensity),
                Kind::Unspecified,
            ))),
        }
    }
}

/// A shadow-only element behind `win`: the card shader with a fully
/// transparent fill, so only its soft drop shadow renders. The shadow
/// corner radius is modest — windows are rectangular, but a small
/// rounding keeps the shadow from looking like a hard box.
/// `intensity` scales the shadow's opacity (via the element alpha,
/// which is all that's left once the fill is transparent), giving the
/// focus-based depth hierarchy.
fn window_shadow_element(
    program: GlesPixelProgram,
    win: Rect,
    intensity: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> BacakElements {
    switcher_card_element(
        program,
        win,
        Color32F::new(0.0, 0.0, 0.0, 0.0), // transparent fill → shadow only
        WINDOW_SHADOW_RADIUS,
        false,     // no selection border
        None,      // no punch
        intensity, // element alpha == shadow opacity here
        output_scale,
        off_x,
        off_y,
    )
}

/// Paint the alt+tab task-switcher overlay: a centred row of
/// colour-coded tiles, one per cycle candidate, with the active tile
/// framed by an accent ring and the whole row backed by a dark
/// translucent panel. Pushes elements in front-first order so the
/// active tile sits on top of the ring, which sits on top of the
/// other tiles, which sit on top of the backdrop.
///
/// `output_bounds` is the host output's bounds (used to centre the
/// row); `off_x` / `off_y` are the WM-global → output-local offsets
/// the rest of `build_styled_elements_for_output` already computes.
#[allow(clippy::too_many_arguments)]
fn render_task_switcher(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    output_bounds: Rect,
    off_x: i32,
    off_y: i32,
    blur_tex: Option<&GlesTexture>,
    out: &mut Vec<BacakElements>,
) {
    let Some((fade, candidates, current)) = state.switcher_render() else {
        return;
    };
    if candidates.is_empty() {
        return;
    }
    let candidates: &[WindowId] = &candidates;

    let n = candidates.len() as f32;
    let row_w = n * SWITCHER_TILE_W + (n - 1.0).max(0.0) * SWITCHER_TILE_GAP;
    let row_h = SWITCHER_TILE_H;
    let cx = output_bounds.x + output_bounds.w / 2.0;
    let cy = output_bounds.y + output_bounds.h / 2.0;
    let row_x = cx - row_w / 2.0;
    let row_y = cy - row_h / 2.0;

    let tile_rect = |i: usize| -> Rect {
        let x = row_x + (i as f32) * (SWITCHER_TILE_W + SWITCHER_TILE_GAP);
        Rect::new(x, row_y, SWITCHER_TILE_W, SWITCHER_TILE_H)
    };

    // ---- Front-first push order --------------------------------------
    // 0. Labels (topmost — must read over the bottom band).
    // 1. Live thumbnails (between label and tile fill).
    // 2. Current tile (the card behind the thumbnail; also the
    //    fallback fill when a window has no surface).
    // 3. Other tiles.
    // 4. Selection ring (a slightly larger rect behind the current tile;
    //    the tile occludes its centre, leaving a visible border).
    // 5. Backdrop (deepest layer in the overlay).

    for (i, id) in candidates.iter().enumerate() {
        let tile = tile_rect(i);
        // Resolve the icon first — its width decides how far the label
        // text is pushed right so the two never overlap.
        let icon =
            switcher_icon_element(state, renderer, *id, tile, fade, output_scale, off_x, off_y);
        let text_pad = if icon.is_some() {
            SWITCHER_ICON_SIZE + SWITCHER_ICON_GAP
        } else {
            0.0
        };
        if state.text.is_some() {
            if let Some(el) = switcher_label_element(
                state,
                renderer,
                *id,
                tile,
                text_pad,
                fade,
                output_scale,
                off_x,
                off_y,
            ) {
                out.push(el);
            }
        }
        if let Some(el) = icon {
            out.push(el);
        }
    }

    let prog = card_program(renderer);

    for (i, id) in candidates.iter().enumerate() {
        let tile = tile_rect(i);
        let thumbs =
            switcher_thumb_elements(state, renderer, *id, tile, fade, output_scale, off_x, off_y);
        if thumbs.is_empty() {
            continue;
        }
        // Overpaint the same card with a rounded "window" punched over
        // the thumbnail area. Pushed before the thumbnail (front-first
        // → on top), it repaints the thumbnail's square corners with
        // pixel-identical card material, so the preview reads as
        // rounded and seamless with the card behind it.
        if let Some(p) = prog.clone() {
            let app = window_app(state, *id);
            let selected = Some(*id) == current;
            let alpha = if selected { 0.95 } else { SWITCHER_TILE_ALPHA_DIM };
            let color = tile_color(&app, alpha);
            out.push(switcher_card_element(
                p,
                tile,
                color,
                CARD_RADIUS,
                selected,
                Some((thumb_area(tile), THUMB_RADIUS)),
                fade,
                output_scale,
                off_x,
                off_y,
            ));
        }
        for el in thumbs {
            out.push(el);
        }
    }

    // Cards: one rounded, shadowed element per candidate. The shader
    // folds the drop shadow and (for the active tile) the selection
    // border into a single pass, so there's no separate ring element.
    // Push the active card first (front-first → it sits above the
    // others where shadows would otherwise overlap at the seams).
    let push_card = |i: usize, id: WindowId, out: &mut Vec<BacakElements>| {
        let app = window_app(state, id);
        let selected = Some(id) == current;
        let alpha = if selected { 0.95 } else { SWITCHER_TILE_ALPHA_DIM };
        let color = tile_color(&app, alpha);
        match prog.clone() {
            Some(p) => out.push(switcher_card_element(
                p,
                tile_rect(i),
                color,
                CARD_RADIUS,
                selected,
                None,
                fade,
                output_scale,
                off_x,
                off_y,
            )),
            None => {
                // No shader → flat tile + (active) plain ring fallback.
                out.push(solid_element(
                    tile_rect(i),
                    fade_color(color, fade),
                    output_scale,
                    off_x,
                    off_y,
                ));
                if selected {
                    let t = tile_rect(i);
                    let ring = Rect::new(
                        t.x - SWITCHER_RING,
                        t.y - SWITCHER_RING,
                        t.w + 2.0 * SWITCHER_RING,
                        t.h + 2.0 * SWITCHER_RING,
                    );
                    out.push(solid_element(
                        ring,
                        fade_color(SWITCHER_RING_COLOR, fade),
                        output_scale,
                        off_x,
                        off_y,
                    ));
                }
            }
        }
    };
    if let Some(active) = current {
        if let Some((i, _)) =
            candidates.iter().enumerate().find(|(_, id)| **id == active)
        {
            push_card(i, active, out);
        }
    }
    for (i, id) in candidates.iter().enumerate() {
        if Some(*id) == current {
            continue;
        }
        push_card(i, *id, out);
    }

    let backdrop = Rect::new(
        row_x - SWITCHER_PAD,
        row_y - SWITCHER_PAD,
        row_w + 2.0 * SWITCHER_PAD,
        row_h + 2.0 * SWITCHER_PAD,
    );

    if let Some(tex) = blur_tex {
        // Real frosted glass, rounded. Front-first push order:
        //   1. translucent tint card (rounded) — tints the whole panel
        //      including the blurred interior.
        //   2. a *rectangular* opaque-tint card with a rounded punch =
        //      the panel interior. This covers the rectangular blur
        //      texture's square corners with the tint colour while the
        //      punch lets the blur show inside the rounded shape; the
        //      same recipe used to round thumbnails. Outside the rect
        //      it's transparent, so beyond the panel the crisp scene
        //      shows through.
        //   3. the rectangular blurred crop (behind both).
        match prog {
            Some(p) => {
                out.push(switcher_card_element(
                    p.clone(),
                    backdrop,
                    fade_color(SWITCHER_BACKDROP_COLOR, 0.55), // blur shows through
                    BACKDROP_RADIUS,
                    false,
                    None,
                    fade,
                    output_scale,
                    off_x,
                    off_y,
                ));
                out.push(switcher_card_element(
                    p,
                    backdrop,
                    SWITCHER_BACKDROP_COLOR, // opaque tint hides square corners
                    0.0,                     // rectangular outer = full cover
                    false,
                    Some((backdrop, BACKDROP_RADIUS)), // rounded window for the blur
                    fade,
                    output_scale,
                    off_x,
                    off_y,
                ));
            }
            // No shader → can't round; a flat translucent panel is the
            // honest fallback (square, but consistent).
            None => out.push(solid_element(
                backdrop,
                fade_color(fade_color(SWITCHER_BACKDROP_COLOR, 0.55), fade),
                output_scale,
                off_x,
                off_y,
            )),
        }
        // Crop of the blurred full-output texture under the panel.
        let lx = backdrop.x - off_x as f32;
        let ly = backdrop.y - off_y as f32;
        let src = Rectangle::<f64, smithay::utils::Logical>::new(
            Point::from((lx as f64, ly as f64)),
            Size::from((backdrop.w as f64, backdrop.h as f64)),
        );
        let loc = Point::<f64, Physical>::from((
            (lx * output_scale as f32) as f64,
            (ly * output_scale as f32) as f64,
        ));
        let size = Size::<i32, smithay::utils::Logical>::from((
            backdrop.w as i32,
            backdrop.h as i32,
        ));
        out.push(BacakElements::Texture(TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            loc,
            tex.clone(),
            output_scale,
            Transform::Normal,
            Some(fade),
            Some(src),
            Some(size),
            None,
            Kind::Unspecified,
        )));
        return;
    }

    match prog {
        // Same shader, no selection border, larger radius. Its soft
        // shadow lands on the wallpaper and lifts the whole panel.
        Some(p) => out.push(switcher_card_element(
            p,
            backdrop,
            SWITCHER_BACKDROP_COLOR,
            BACKDROP_RADIUS,
            false,
            None,
            fade,
            output_scale,
            off_x,
            off_y,
        )),
        None => out.push(solid_element(
            backdrop,
            fade_color(SWITCHER_BACKDROP_COLOR, fade),
            output_scale,
            off_x,
            off_y,
        )),
    }
}

/// Build a rounded, drop-shadowed card for `tile`. The element's
/// `area` is the tile grown by [`CARD_MARGIN`]; the card occupies the
/// inner tile rect via `card_min`/`card_max` in element-local px.
///
/// Build a rounded card. `punch`, when `Some((rect, r))`, carves a
/// rounded-rect transparent window at `rect` (in WM-global coords,
/// radius `r`) so a thumbnail drawn behind shows with matching
/// corners. `None` disables the punch (`punch_radius = -1`).
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn switcher_card_element(
    program: GlesPixelProgram,
    tile: Rect,
    color: Color32F,
    radius: f32,
    selected: bool,
    punch: Option<(Rect, f32)>,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> BacakElements {
    // Element area in output-local LOGICAL px; PixelShaderElement scales
    // it to physical via the output's scale, and the shader divides the
    // physical fragment position back down by `u_scale` (see CARD_SHADER).
    let area = Rectangle::<i32, smithay::utils::Logical>::new(
        Point::from((
            (tile.x - off_x as f32 - CARD_MARGIN).round() as i32,
            (tile.y - off_y as f32 - CARD_MARGIN).round() as i32,
        )),
        Size::from((
            (tile.w + 2.0 * CARD_MARGIN).round() as i32,
            (tile.h + 2.0 * CARD_MARGIN).round() as i32,
        )),
    );
    // Card rect inside the area: offset by the margin on each side.
    let cmin = [CARD_MARGIN, CARD_MARGIN];
    let cmax = [CARD_MARGIN + tile.w, CARD_MARGIN + tile.h];
    let border = if selected { CARD_BORDER } else { 0.0 };

    // Punch rect is given in WM-global coords; convert to the same
    // element-local space as the card (area top-left = tile-CARD_MARGIN).
    let (pmin, pmax, prad) = match punch {
        Some((r, pr)) => {
            // off_x/off_y cancel: element origin is tile - CARD_MARGIN.
            let lx = r.x - tile.x + CARD_MARGIN;
            let ly = r.y - tile.y + CARD_MARGIN;
            ([lx, ly], [lx + r.w, ly + r.h], pr)
        }
        None => ([0.0, 0.0], [0.0, 0.0], -1.0),
    };

    let element = PixelShaderElement::new(
        program,
        area,
        None,
        fade,
        vec![
            Uniform::new("u_scale", output_scale as f32),
            Uniform::new("card_min", cmin),
            Uniform::new("card_max", cmax),
            Uniform::new("radius", radius),
            Uniform::new("shadow_soft", CARD_SHADOW_SOFT),
            Uniform::new("shadow_off", CARD_SHADOW_OFF),
            Uniform::new("card_col", color.components()),
            Uniform::new("shadow_col", CARD_SHADOW_COL),
            Uniform::new("border", border),
            Uniform::new("border_col", SWITCHER_RING_COLOR.components()),
            Uniform::new("punch_min", pmin),
            Uniform::new("punch_max", pmax),
            Uniform::new("punch_radius", prad),
        ],
        Kind::Unspecified,
    );
    BacakElements::Pixel(element)
}

/// Build the text-selection **magnifier** (loupe): a zoomed crop of `scene_tex`
/// (the output's full scene, rendered at `output_scale`, `ow×oh` physical)
/// sampled around the finger `(fx, fy)` in WM-global logical px, drawn in a
/// bordered box centred above the finger. Returned topmost-first so the caller
/// prepends it to the frame.
#[allow(clippy::too_many_arguments)]
pub fn loupe_elements(
    renderer: &mut GlesRenderer,
    scene_tex: GlesTexture,
    fx: f32,
    fy: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    ow: i32,
    oh: i32,
) -> Vec<BacakElements> {
    // All logical px. Magnify a small crop under the finger.
    const ZOOM: f32 = 1.6;
    const CROP_W: f32 = 90.0;
    const CROP_H: f32 = 50.0;
    const GAP: f32 = 28.0; // loupe floats this far above the finger
    const BORDER: f32 = 3.0;

    let lw = CROP_W * ZOOM;
    let lh = CROP_H * ZOOM;
    let scale = output_scale as f32;
    let owl = ow as f32 / scale; // output logical width
    let ohl = oh as f32 / scale;
    // Output-local finger position.
    let flx = fx - off_x as f32;
    let fly = fy - off_y as f32;
    // Loupe box top-left (output-local), clamped on-screen.
    let lx = (flx - lw / 2.0).clamp(BORDER, (owl - lw - BORDER).max(BORDER));
    let ly = (fly - lh - GAP).max(BORDER);
    // Source crop in the texture's logical (= output-local) space, clamped so we
    // never sample outside the texture.
    let sx = (flx - CROP_W / 2.0).clamp(0.0, (owl - CROP_W).max(0.0));
    let sy = (fly - CROP_H / 2.0).clamp(0.0, (ohl - CROP_H).max(0.0));
    let src = Rectangle::<f64, smithay::utils::Logical>::new(
        Point::from((sx as f64, sy as f64)),
        Size::from((CROP_W as f64, CROP_H as f64)),
    );
    let loc = Point::<f64, Physical>::from(((lx * scale) as f64, (ly * scale) as f64));
    let size = Size::<i32, smithay::utils::Logical>::from((lw as i32, lh as i32));

    let mut out: Vec<BacakElements> = Vec::new();
    // Magnified crop (topmost).
    out.push(BacakElements::Texture(
        TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            loc,
            scene_tex,
            output_scale,
            Transform::Normal,
            None,
            Some(src),
            Some(size),
            None,
            Kind::Unspecified,
        ),
    ));
    // A light border card behind the crop — the crop covers its interior, so a
    // thin frame shows around the loupe. WM-global rect for `solid_element`.
    let border = Rect::new(
        lx + off_x as f32 - BORDER,
        ly + off_y as f32 - BORDER,
        lw + 2.0 * BORDER,
        lh + 2.0 * BORDER,
    );
    out.push(solid_element(
        border,
        Color32F::new(0.92, 0.92, 0.95, 0.95),
        output_scale,
        off_x,
        off_y,
    ));
    out
}

fn solid_element(
    rect: Rect,
    color: Color32F,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> BacakElements {
    BacakElements::Solid(SolidColorRenderElement::new(
        Id::new(),
        to_physical_rect_offset(rect, output_scale, off_x, off_y),
        0usize,
        color,
        Kind::Unspecified,
    ))
}

fn window_app(state: &BacakState, id: WindowId) -> String {
    state.wm.get(id).map(|w| w.app).unwrap_or_default()
}

/// Switcher label text: the window title, falling back to the app id
/// when a client never set one (common for freshly-mapped surfaces).
fn window_title(state: &BacakState, id: WindowId) -> String {
    match state.wm.get(id) {
        Ok(w) if !w.title.trim().is_empty() => w.title,
        Ok(w) => w.app,
        Err(_) => String::new(),
    }
}

/// Render a downscaled live preview of window `id`'s surface, centred
/// in the thumbnail region of `tile` (everything above the label
/// band). Returns an empty `Vec` when the window has no mapped
/// surface or zero geometry — the caller then just shows the flat
/// coloured tile.
///
/// The surface is rendered at `thumb_scale = fit(window → area)`,
/// capped at 1.0 so tiny windows aren't blurrily upscaled. Aspect
/// ratio is preserved; the preview is centred in the area.
/// The thumbnail region inside a tile: the tile minus the inner
/// padding and the bottom label band. Shared by the thumbnail
/// renderer and the rounded-corner punch overpaint so they agree on
/// exactly which rect gets carved out.
fn thumb_area(tile: Rect) -> Rect {
    Rect::new(
        tile.x + SWITCHER_TILE_INNER_PAD,
        tile.y + SWITCHER_TILE_INNER_PAD,
        (tile.w - 2.0 * SWITCHER_TILE_INNER_PAD).max(1.0),
        (tile.h - SWITCHER_LABEL_BAND - 2.0 * SWITCHER_TILE_INNER_PAD).max(1.0),
    )
}

#[allow(clippy::too_many_arguments)]
fn switcher_thumb_elements(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    tile: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Vec<BacakElements> {
    // Thumbnail region: the tile minus inner padding and the bottom
    // label band. Shared with the dock hover preview via
    // [`surface_fit_into_area`].
    surface_fit_into_area(
        state,
        renderer,
        id,
        thumb_area(tile),
        fade,
        output_scale,
        off_x,
        off_y,
    )
}

/// Render window `id`'s surface tree scaled to fit inside `area`,
/// centred and aspect-preserved. Returns an empty vec when the window
/// has no mapped surface or zero geometry — the caller can then fall
/// back to a flat icon. Shared by the task switcher's tile thumbnails
/// and the dock's hover-dwell window preview.
#[allow(clippy::too_many_arguments)]
/// Title-bar text styling.
const DECO_LABEL_PX: f32 = 14.0;
const DECO_LABEL_RGB: [u8; 3] = [226, 230, 238];
const DECO_LABEL_INSET: f32 = 10.0;

/// The window title rendered into its server-side title bar, left-aligned and
/// vertically centred, reserving the close button on the right. Cached per
/// window in `deco_label_cache` (title-keyed) so it rasterises only when the
/// title changes, not every frame.
#[allow(clippy::too_many_arguments)]
fn deco_label_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    bar: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    let title = window_title(state, id);
    if title.trim().is_empty() {
        return None;
    }
    let font = state.text.as_ref()?;
    let max_w = (bar.w
        - 2.0 * DECO_LABEL_INSET
        - crate::decoration::BTN
        - crate::decoration::BTN_MARGIN)
        .max(1.0) as usize;

    let mut cache = state.deco_label_cache.lock();
    let stale = match cache.get(&id) {
        Some(e) => e.title != title,
        None => true,
    };
    if stale {
        let (buffer, _w, h) = rasterize_label(font, &title, DECO_LABEL_PX, DECO_LABEL_RGB, max_w)?;
        cache.insert(
            id,
            crate::state::LabelCacheEntry { title: title.clone(), buffer, height: h },
        );
    }
    let entry = cache.get(&id)?;
    let lx = bar.x + DECO_LABEL_INSET;
    let ly = bar.y + (bar.h - entry.height as f32) / 2.0;
    let scale = output_scale as f32;
    let phys = Point::<f64, Physical>::from((
        ((lx - off_x as f32) * scale) as f64,
        ((ly - off_y as f32) * scale) as f64,
    ));
    let elem = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys,
        &entry.buffer,
        Some(fade),
        None,
        None,
        Kind::Unspecified,
    )
    .ok()?;
    Some(BacakElements::Memory(elem))
}

/// Draw server-side title bars for decorated windows on workspace `ws`. The
/// bar sits directly above each window's content (see [`crate::decoration`]),
/// with the window title on the left and a close-button chip on the right.
/// Pushed before the window content so it renders on top.
#[allow(clippy::too_many_arguments)]
/// Push one window's server-side title bar (title + close chip/glyph + plaque).
/// Called from `render_workspace_windows` *immediately before* the window's own
/// content, so the bar sits on top of its content but below any higher window —
/// correct z-order even when windows overlap. No-op for undecorated or
/// fullscreen windows. `off_x/off_y` match the content's, so it slides with the
/// workspace.
#[allow(clippy::too_many_arguments)]
fn push_window_decoration(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    win: &crate::wm::Window,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    use crate::decoration::{bar_rect, close_rect};
    use crate::wm::WinState;

    if !state.decorated.contains(&win.id) || matches!(win.state, WinState::Fullscreen) {
        return;
    }
    let bar = bar_rect(win.geom);
    let close = close_rect(win.geom);
    // Focused windows get a brighter bar; unfocused dim so the active one reads.
    let bar_col = if win.focused {
        Color32F::new(0.13, 0.15, 0.20, 0.96)
    } else {
        Color32F::new(0.09, 0.10, 0.13, 0.92)
    };
    // Front-first push: title text, close "×" glyph, close chip, then the bar
    // plaque behind them all.
    let fade = if win.focused { 1.0 } else { 0.85 };
    if let Some(el) = deco_label_element(state, renderer, win.id, bar, fade, output_scale, off_x, off_y) {
        out.push(el);
    }
    if let Some((_, gw, gh)) = state.deco_close_glyph.as_ref() {
        let gx = close.x + (close.w - *gw as f32) / 2.0;
        let gy = close.y + (close.h - *gh as f32) / 2.0;
        cc_blit_label(out, renderer, &state.deco_close_glyph, gx, gy, output_scale, off_x, off_y);
    }
    cc_card(
        out,
        renderer,
        close,
        Color32F::new(0.84, 0.26, 0.26, 0.96),
        close.h / 2.0,
        output_scale,
        off_x,
        off_y,
    );
    cc_card(out, renderer, bar, bar_col, 8.0, output_scale, off_x, off_y);
}

/// Draw every mapped override-redirect X11 surface at its own absolute
/// (global-logical) geometry, 1:1. These are app popups (menus, tooltips)
/// that bypass the WM; `off_x/off_y` re-base them into the current output and
/// off-output positions clip naturally.
fn render_x11_override(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let scale = output_scale as f64;
    for surface in &state.x11_override {
        if !surface.is_mapped() {
            continue;
        }
        let Some(wl) = surface.wl_surface() else { continue };
        let geo = surface.geometry();
        let location: Point<i32, Physical> = Point::from((
            (((geo.loc.x - off_x) as f64) * scale).round() as i32,
            (((geo.loc.y - off_y) as f64) * scale).round() as i32,
        ));
        out.extend(
            render_elements_from_surface_tree(
                renderer,
                &wl,
                location,
                smithay::utils::Scale::from(scale),
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(BacakElements::Surface),
        );
    }
}

/// Cap on how far a window preview is scaled *up* to fill its card frame.
/// Aspect-fit lets small windows (terminals, dialogs, tiny utilities) grow to
/// fill the fixed frame so every card reads uniformly — but unbounded upscale
/// would blur a tiny dialog badly, so cap it. Big windows still scale down to
/// fit with no lower bound.
const MAX_PREVIEW_UPSCALE: f32 = 2.5;

/// Per-window refresh throttle for snapshots (ms).
const SNAP_REFRESH_MS: u128 = 700;
/// Skip windows larger than this (longest edge, logical px) to bound snapshot
/// memory — a snapshot is captured at native size, so very large windows would
/// be expensive. Such a card simply falls back to its app icon.
const SNAP_MAX_EDGE: f32 = 2560.0;

/// Refresh frozen GPU previews of mapped windows into `state.snapshots`, so the
/// Overview can show a real thumbnail for a window that's since minimised or
/// moved to another workspace (its content isn't on screen then). Throttled
/// per window and self-pruning. Mirrors [`blurred_backdrop`]'s offscreen
/// pattern (`render_output` into a bound texture); both backends call it while
/// the renderer is unbound (udev: before the per-target build; winit: after the
/// window frame finishes).
///
/// Window set depends on Overview state: while it's **open** every mapped
/// window is refreshed (off-workspace cards are visible then, and capture runs
/// before the build so they're fresh on the very first overview frame); while
/// it's **closed** only active-workspace windows are captured, so a later
/// minimise still has a recent snapshot without paying for hidden windows.
pub fn capture_window_snapshots(state: &BacakState, renderer: &mut GlesRenderer) {
    // Drop snapshots for windows that no longer exist.
    {
        let mut cache = state.snapshots.lock();
        if !cache.is_empty() {
            cache.retain(|id, _| state.wm.get(*id).is_ok());
        }
    }

    let windows = if state.overview.is_some() {
        state.wm.all_windows() // overview open → include off-workspace cards
    } else {
        state.wm.list_visible() // normal use → active workspace only
    };
    for win in windows {
        if win.geom.w <= 1.0
            || win.geom.h <= 1.0
            || win.geom.w.max(win.geom.h) > SNAP_MAX_EDGE
        {
            continue;
        }
        // Throttle: skip if a fresh-enough snapshot already exists.
        {
            let cache = state.snapshots.lock();
            if let Some(s) = cache.get(&win.id) {
                if s.captured.elapsed().as_millis() < SNAP_REFRESH_MS {
                    continue;
                }
            }
        }
        let Some(surface) = state.surface_for_window(win.id) else { continue };

        let tw = (win.geom.w as i32).max(1);
        let th = (win.geom.h as i32).max(1);
        // Native-size scene (scale 1.0), exactly like the blur backdrop and the
        // winit render path — no scale ambiguity. Downscale happens for free at
        // draw time when the small card fits the texture.
        let scene: Vec<BacakElements> = render_elements_from_surface_tree(
            renderer,
            &surface,
            (0, 0),
            Scale::from(1.0),
            1.0,
            Kind::Unspecified,
        )
        .into_iter()
        .map(BacakElements::Surface)
        .collect();
        if scene.is_empty() {
            continue;
        }

        let size = Size::<i32, BufferCoord>::from((tw, th));
        let Ok(mut tex) = renderer.create_buffer(Fourcc::Abgr8888, size) else { continue };
        let phys = Size::<i32, Physical>::from((tw, th));
        let captured = (|| -> Option<()> {
            let mut fb = renderer.bind(&mut tex).ok()?;
            let mut dt = OutputDamageTracker::new(phys, Scale::from(1.0), Transform::Normal);
            dt.render_output(renderer, &mut fb, 0, &scene, Color32F::TRANSPARENT)
                .ok()?;
            Some(())
        })()
        .is_some();
        if captured {
            state.snapshots.lock().insert(
                win.id,
                crate::state::WindowSnapshot { tex, w: tw, h: th, captured: Instant::now() },
            );
        }
    }
}

/// A `TextureRenderElement` drawing window `id`'s cached snapshot fit (centred,
/// aspect-preserved) into `area`. `None` when there's no snapshot — the caller
/// then falls back to the app icon.
#[allow(clippy::too_many_arguments)]
fn snapshot_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    area: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    let (tex, tw, th) = {
        let cache = state.snapshots.lock();
        let s = cache.get(&id)?;
        (s.tex.clone(), s.w as f32, s.h as f32)
    };
    if tw < 1.0 || th < 1.0 {
        return None;
    }
    // Same aspect-fit-to-fill rule as the live preview (capped upscale).
    let fit = (area.w / tw).min(area.h / th).min(MAX_PREVIEW_UPSCALE);
    let dw = (tw * fit).max(1.0);
    let dh = (th * fit).max(1.0);
    let dx = area.x + (area.w - dw) / 2.0;
    let dy = area.y + (area.h - dh) / 2.0;

    let tb = TextureBuffer::from_texture(renderer, tex, 1, Transform::Normal, None);
    let scale = output_scale as f64;
    let loc = Point::<f64, Physical>::from((
        ((dx - off_x as f32) as f64) * scale,
        ((dy - off_y as f32) as f64) * scale,
    ));
    let size = Size::<i32, Logical>::from((dw as i32, dh as i32));
    let el = TextureRenderElement::from_texture_buffer(
        loc,
        &tb,
        Some(fade),
        None,
        Some(size),
        Kind::Unspecified,
    );
    Some(BacakElements::Texture(el))
}

fn surface_fit_into_area(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    area: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Vec<BacakElements> {
    let Ok(win) = state.wm.get(id) else { return Vec::new() };
    if win.geom.w <= 1.0 || win.geom.h <= 1.0 {
        return Vec::new();
    }
    let Some(wl_surface) = state.surface_for_window(id) else {
        return Vec::new();
    };

    // Aspect-fit into the frame, scaling small windows *up* to fill it (capped)
    // and large ones down — so every card reads the same size regardless of the
    // real window dimensions. The centred preview is letterboxed / pillarboxed
    // by the card background.
    let thumb_scale =
        (area.w / win.geom.w).min(area.h / win.geom.h).clamp(0.01, MAX_PREVIEW_UPSCALE);
    let rendered_w = win.geom.w * thumb_scale;
    let rendered_h = win.geom.h * thumb_scale;
    let thumb_x = area.x + (area.w - rendered_w) / 2.0;
    let thumb_y = area.y + (area.h - rendered_h) / 2.0;

    let scale = output_scale as f64;
    let location: Point<i32, Physical> = Point::from((
        (((thumb_x - off_x as f32) as f64) * scale).round() as i32,
        (((thumb_y - off_y as f32) as f64) * scale).round() as i32,
    ));

    // Hard clip to the frame (#2/#7): the surface tree is walked whole, so a
    // subsurface at a negative offset (shadow, popup, CSD) could otherwise
    // paint left/above `thumb` — over the label band or the neighbour card.
    // Crop every element to the card's preview area in physical, output-
    // relative coords (the same space `location` lives in). Elements entirely
    // outside the frame intersect to nothing → `from_element` drops them.
    let crop_rect = Rectangle::<i32, Physical>::new(
        Point::from((
            (((area.x - off_x as f32) as f64) * scale).round() as i32,
            (((area.y - off_y as f32) as f64) * scale).round() as i32,
        )),
        Size::from((
            ((area.w as f64) * scale).round() as i32,
            ((area.h as f64) * scale).round() as i32,
        )),
    );

    render_elements_from_surface_tree(
        renderer,
        &wl_surface,
        location,
        smithay::utils::Scale::from(thumb_scale as f64 * scale),
        fade,
        Kind::Unspecified,
    )
    .into_iter()
    .filter_map(|el| CropRenderElement::from_element(el, scale, crop_rect))
    .map(BacakElements::CroppedSurface)
    .collect()
}

/// Resolve and place the app icon for window `id` in the left of the
/// tile's label band. The decoded icon is cached per `app_id` (shared
/// across that app's windows) so the filesystem walk + PNG decode +
/// GPU upload happen once per app per session. Returns `None` when
/// there's no app id, no usable icon, or the import fails — the label
/// then simply starts at the band's left edge.
#[allow(clippy::too_many_arguments)]
fn switcher_icon_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    tile: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    let app = window_app(state, id);
    // Square display footprint, vertically centred in the label band.
    let band_top = tile.y + tile.h - SWITCHER_LABEL_BAND;
    let dst = Rect::new(
        tile.x + SWITCHER_LABEL_INSET,
        band_top + (SWITCHER_LABEL_BAND - SWITCHER_ICON_SIZE) / 2.0,
        SWITCHER_ICON_SIZE,
        SWITCHER_ICON_SIZE,
    );
    app_icon_element(state, renderer, &app, dst, fade, output_scale, off_x, off_y)
}

/// Resolve `app`'s desktop icon and place it as a Memory element
/// filling `dst` (WM-global logical px). The decoded icon is cached
/// per app id — shared across that app's windows and across the
/// switcher and dock — so the FS walk + decode + GPU upload happen
/// once per app per session; `None` is cached too so a missing icon
/// doesn't re-walk the filesystem every frame. Returns `None` when
/// there's no app id, no usable icon, or the import fails.
#[allow(clippy::too_many_arguments)]
fn app_icon_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    app: &str,
    dst: Rect,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    if app.trim().is_empty() {
        return None;
    }

    let mut cache = state.icon_cache.lock();
    if !cache.contains_key(app) {
        let resolved = crate::icons::resolve_icon_rgba(app)
            .or_else(crate::icons::generic_fallback_icon)
            .map(|(rgba, w, h)| {
            let buffer = MemoryRenderBuffer::from_slice(
                &rgba,
                Fourcc::Abgr8888,
                (w as i32, h as i32),
                1,
                Transform::Normal,
                None,
            );
            crate::state::IconCacheEntry { buffer, w, h }
        });
        cache.insert(app.to_string(), resolved);
    }
    let entry = cache.get(app)?.as_ref()?;

    let scale = output_scale as f32;
    let phys = Point::<f64, Physical>::from((
        ((dst.x - off_x as f32) * scale) as f64,
        ((dst.y - off_y as f32) * scale) as f64,
    ));
    // `size` is the destination footprint (switcher glyph or dock tile); it's a
    // *logical* override Smithay multiplies by the output scale, so we must NOT
    // pre-scale here (that would square the scale on a HiDPI output).
    //
    // `src` MUST be the full buffer: when it's `None`, Smithay defaults it to a
    // rect of `size`, i.e. it samples only the top-left `dw×dh` of the source
    // and shows it 1:1 — a CROP, not a scale. For a 128px icon in a ~40px tile
    // that crop is just the icon's top-left corner (the blue body of the
    // LibreOffice glyph, the dark corner of foot) — the "blue rectangle" bug.
    // Passing the whole buffer as `src` makes Smithay scale the full icon down.
    let dw = dst.w.round().max(1.0) as i32;
    let dh = dst.h.round().max(1.0) as i32;
    let src = smithay::utils::Rectangle::<f64, smithay::utils::Logical>::from_size(
        smithay::utils::Size::from((entry.w as f64, entry.h as f64)),
    );
    let elem = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys,
        &entry.buffer,
        Some(fade),
        Some(src),
        Some(Size::<i32, smithay::utils::Logical>::from((dw, dh))),
        Kind::Unspecified,
    )
    .ok()?;
    Some(BacakElements::Memory(elem))
}

/// Build the label element for window `id`, reusing a cached
/// `MemoryRenderBuffer` whenever the title hasn't changed. Returns
/// `None` when there's no font, the title is empty, or the GPU import
/// fails — the caller then simply omits the label.
///
/// The cache lives behind a `Mutex` on `BacakState`; reusing the same
/// `MemoryRenderBuffer` instance is what skips the per-frame texture
/// upload (Smithay caches the imported texture inside the buffer).
#[allow(clippy::too_many_arguments)]
fn switcher_label_element(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    id: WindowId,
    tile: Rect,
    text_pad: f32,
    fade: f32,
    output_scale: i32,
    off_x: i32,
    off_y: i32,
) -> Option<BacakElements> {
    let title = window_title(state, id);
    if title.trim().is_empty() {
        return None;
    }
    let font = state.text.as_ref()?;
    // `text_pad` reserves the icon's footprint on the left. It's
    // resolved before this call (the icon cache is populated first),
    // so it's stable from frame one — the title-keyed buffer cache
    // never sees a width mismatch.
    let max_w = (tile.w - 2.0 * SWITCHER_LABEL_INSET - text_pad).max(1.0) as usize;

    let mut cache = state.label_cache.lock();
    // NLL forbids matching on `cache.get` and then `cache.insert` in the
    // same scrutinee, so decide first, mutate second.
    let stale = match cache.get(&id) {
        Some(entry) => entry.title != title,
        None => true,
    };
    if stale {
        let (buffer, _w, h) =
            rasterize_label(font, &title, SWITCHER_LABEL_PX, SWITCHER_LABEL_RGB, max_w)?;
        cache.insert(
            id,
            crate::state::LabelCacheEntry { title: title.clone(), buffer, height: h },
        );
    }

    let entry = cache.get(&id)?;
    // The label lives in the bottom band; the thumbnail owns the rest.
    let label_x = tile.x + SWITCHER_LABEL_INSET + text_pad;
    let band_top = tile.y + tile.h - SWITCHER_LABEL_BAND;
    let label_y = band_top + (SWITCHER_LABEL_BAND - entry.height as f32) / 2.0;
    let scale = output_scale as f32;
    let phys = Point::<f64, Physical>::from((
        ((label_x - off_x as f32) * scale) as f64,
        ((label_y - off_y as f32) * scale) as f64,
    ));

    let elem = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        phys,
        &entry.buffer,
        Some(fade), // overlay fade; glyph coverage is in the buffer
        None,       // src: whole buffer
        None,       // size: native (scale-1 buffer; output_scale is 1)
        Kind::Unspecified,
    )
    .ok()?;
    Some(BacakElements::Memory(elem))
}

/// Deterministic colour from app name. Same app → same tile colour
/// across sessions, which doubles as a (very modest) "I recognise
/// this" cue while text rendering is still missing.
fn tile_color(app: &str, alpha: f32) -> Color32F {
    let h = app
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let (r, g, b) = SWITCHER_PALETTE[(h as usize) % SWITCHER_PALETTE.len()];
    Color32F::new(r, g, b, alpha)
}

/// Scale a colour's alpha by the overlay fade. Used by the no-shader
/// solid fallbacks, where there's no per-element alpha to ride.
fn fade_color(c: Color32F, fade: f32) -> Color32F {
    let [r, g, b, a] = c.components();
    Color32F::new(r, g, b, a * fade)
}

/// Convert a WM-logical `Rect` (in compositor-global coords) to a
/// physical-pixel rectangle in **output-local** coords. `off_x` /
/// `off_y` are the output's logical origin; we subtract them so the
/// element lands at (0,0) of the output's framebuffer when the rect
/// is at the output's top-left.
/// Rasterise a UI label with the same supersampling as
/// [`crate::state`]'s `cc_rasterize`: glyphs are drawn at
/// `LABEL_SUPERSAMPLE×` the point size into a buffer tagged with that
/// buffer scale, so the returned *logical* dimensions are unchanged but
/// the text is pixel-crisp at any output scale. Returns
/// `(buffer, logical_w, logical_h)`.
fn rasterize_label(
    font: &crate::text::TextRenderer,
    s: &str,
    px: f32,
    rgb: [u8; 3],
    max_w: usize,
) -> Option<(MemoryRenderBuffer, usize, usize)> {
    let ss = crate::state::LABEL_SUPERSAMPLE;
    let (rgba, w, h) = font.rasterize_line(s, px * ss as f32, rgb, max_w * ss as usize)?;
    let buffer = MemoryRenderBuffer::from_slice(
        &rgba,
        Fourcc::Abgr8888,
        (w as i32, h as i32),
        ss,
        Transform::Normal,
        None,
    );
    Some((buffer, w / ss as usize, h / ss as usize))
}

fn to_physical_rect_offset(
    r: Rect,
    scale: i32,
    off_x: i32,
    off_y: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::new(
        Point::<i32, Physical>::from((
            ((r.x as i32) - off_x).saturating_mul(scale),
            ((r.y as i32) - off_y).saturating_mul(scale),
        )),
        Size::<i32, Physical>::from((
            (r.w as i32).saturating_mul(scale),
            (r.h as i32).saturating_mul(scale),
        )),
    )
}

/// Compute the physical-pixel rectangle that will hold the drop shadow:
/// expand outward by `SHADOW_INSET` on every side and nudge down by
/// `SHADOW_OFFSET_Y` so the shadow sits below the window (matches the
/// light-from-above convention every modern WM uses). The `off_*` args
/// translate from compositor-global to output-local coords.
fn shadow_rect_offset(
    window_geom: Rect,
    scale: i32,
    off_x: i32,
    off_y: i32,
) -> Rectangle<i32, Physical> {
    let inset = SHADOW_INSET; // negative → expand
    let off_y_shadow = SHADOW_OFFSET_Y;
    let x = ((window_geom.x as i32) - off_x + inset).saturating_mul(scale);
    let y = ((window_geom.y as i32) - off_y + inset + off_y_shadow).saturating_mul(scale);
    let w = ((window_geom.w as i32) - 2 * inset).saturating_mul(scale);
    let h = ((window_geom.h as i32) - 2 * inset).saturating_mul(scale);
    Rectangle::new(
        Point::<i32, Physical>::from((x, y)),
        Size::<i32, Physical>::from((w, h)),
    )
}

// ---------------------------------------------------------------------------
// File Browser overlay
// ---------------------------------------------------------------------------

pub(crate) fn render_file_browser(
    state: &BacakState,
    renderer: &mut GlesRenderer,
    output: OutputId,
    scale: i32,
    off_x: i32,
    off_y: i32,
    out: &mut Vec<BacakElements>,
) {
    let Some(fb) = state.file_browser.as_ref() else { return };
    if fb.output != output { return; }

    let p   = fb.panel;
    let lr  = fb.list_rect;

    let panel_bg   = Color32F::new(0.08, 0.10, 0.16, 0.97);
    let header_bg  = Color32F::new(0.12, 0.14, 0.22, 1.0);
    let path_bg    = Color32F::new(0.10, 0.12, 0.19, 1.0);
    let row_dir    = Color32F::new(0.14, 0.18, 0.30, 0.90);
    let row_img    = Color32F::new(0.12, 0.24, 0.16, 0.90);
    let row_other  = Color32F::new(0.10, 0.11, 0.17, 0.70);
    let btn_cancel = Color32F::new(0.22, 0.22, 0.28, 0.95);
    let btn_nav    = Color32F::new(0.16, 0.22, 0.38, 0.95);
    let shadow_col = Color32F::new(0.0,  0.0,  0.0,  0.60);
    let divider    = Color32F::new(1.0,  1.0,  1.0,  0.07);

    const TITLE_H: f32 = 44.0;
    const PATH_H:  f32 = 28.0;
    const BOT_H:   f32 = 50.0;
    const RADIUS:  f32 = 3.0;

    // ── Pass 1: foreground labels (pushed first = topmost) ──

    // Title label
    cc_blit_label(out, renderer, &fb.title_label,
        p.x + 16.0, p.y + (TITLE_H - 16.0) / 2.0, scale, off_x, off_y);

    // Path label
    cc_blit_label(out, renderer, &fb.path_label,
        p.x + 12.0, p.y + TITLE_H + (PATH_H - 13.0) / 2.0, scale, off_x, off_y);

    // Entry icons + labels (clipped to list_rect in Y)
    {
        let row_h          = fb.row_h;
        let icon_sz        = 28.0_f32;
        let icon_pad       = 8.0_f32;
        let label_x_off    = icon_pad + icon_sz + 8.0; // label starts after icon
        let visible_top    = lr.y;
        let visible_bottom = lr.y + lr.h;
        for (i, entry) in fb.entries.iter().enumerate() {
            let ey = lr.y + i as f32 * row_h - fb.scroll_y;
            if ey + row_h < visible_top    { continue; }
            if ey         > visible_bottom { break; }
            // Icon
            let icon_buf = if entry.is_dir { &fb.icon_folder } else if entry.is_image { &fb.icon_image } else { &None };
            if let Some(buf) = icon_buf {
                let ix = lr.x + icon_pad;
                let iy = ey + (row_h - icon_sz) / 2.0;
                // Only blit if icon is at least partially visible
                if iy + icon_sz > visible_top && iy < visible_bottom {
                    let phys = Point::<f64, Physical>::from((
                        ((ix - off_x as f32) * scale as f32) as f64,
                        ((iy - off_y as f32) * scale as f32) as f64,
                    ));
                    if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(
                        renderer, phys, buf, Some(1.0), None, None, Kind::Unspecified,
                    ) {
                        out.push(BacakElements::Memory(el));
                    }
                }
            }
            // Label (shifted right to make room for icon)
            let label_y = ey + (row_h - 14.0) / 2.0;
            cc_blit_label(out, renderer, &entry.label,
                lr.x + label_x_off, label_y, scale, off_x, off_y);
        }
    }

    // Bottom button labels
    cc_blit_label(out, renderer, &fb.cancel_label,
        fb.cancel_rect.x + 8.0,
        fb.cancel_rect.y + (fb.cancel_rect.h - 14.0) / 2.0,
        scale, off_x, off_y);
    cc_blit_label(out, renderer, &fb.home_label,
        fb.home_rect.x + 8.0,
        fb.home_rect.y + (fb.home_rect.h - 14.0) / 2.0,
        scale, off_x, off_y);
    cc_blit_label(out, renderer, &fb.up_label,
        fb.up_rect.x + 6.0,
        fb.up_rect.y + (fb.up_rect.h - 14.0) / 2.0,
        scale, off_x, off_y);
    cc_blit_label(out, renderer, &fb.scroll_up_label,
        fb.scroll_up_rect.x + 6.0,
        fb.scroll_up_rect.y + (fb.scroll_up_rect.h - 14.0) / 2.0,
        scale, off_x, off_y);
    cc_blit_label(out, renderer, &fb.scroll_dn_label,
        fb.scroll_dn_rect.x + 6.0,
        fb.scroll_dn_rect.y + (fb.scroll_dn_rect.h - 14.0) / 2.0,
        scale, off_x, off_y);

    // ── Pass 2: entry row backgrounds ──
    {
        let row_h          = fb.row_h;
        let visible_top    = lr.y;
        let visible_bottom = lr.y + lr.h;
        for (i, entry) in fb.entries.iter().enumerate() {
            let ey = lr.y + i as f32 * row_h - fb.scroll_y;
            if ey + row_h < visible_top    { continue; }
            if ey         > visible_bottom { break; }
            let ry     = ey.max(visible_top);
            let ry_end = (ey + row_h - 1.0).min(visible_bottom);
            let rh     = (ry_end - ry).max(0.0);
            if rh <= 0.0 { continue; }
            let bg = if entry.is_dir { row_dir } else if entry.is_image { row_img } else { row_other };
            out.push(solid_element(Rect::new(lr.x, ry, lr.w, rh), bg, scale, off_x, off_y));
            let div_y = (ey + row_h - 1.0).min(visible_bottom - 1.0);
            if div_y > visible_top {
                out.push(solid_element(Rect::new(lr.x, div_y, lr.w, 1.0), divider, scale, off_x, off_y));
            }
        }
    }

    // Bottom buttons
    out.push(solid_element(fb.cancel_rect,    btn_cancel, scale, off_x, off_y));
    out.push(solid_element(fb.home_rect,      btn_nav,    scale, off_x, off_y));
    out.push(solid_element(fb.up_rect,        btn_nav,    scale, off_x, off_y));
    out.push(solid_element(fb.scroll_up_rect, Color32F::new(0.18, 0.26, 0.46, 0.95), scale, off_x, off_y));
    out.push(solid_element(fb.scroll_dn_rect, Color32F::new(0.18, 0.26, 0.46, 0.95), scale, off_x, off_y));

    // Scrollbar track + thumb
    {
        let sb = fb.scrollbar_rect;
        out.push(solid_element(sb, Color32F::new(1.0, 1.0, 1.0, 0.06), scale, off_x, off_y));
        if fb.scroll_max > 0.0 {
            let content_h = fb.entries.len() as f32 * fb.row_h;
            let thumb_h   = (sb.h * sb.h / content_h).max(20.0).min(sb.h);
            let thumb_y   = sb.y + (sb.h - thumb_h) * (fb.scroll_y / fb.scroll_max);
            out.push(solid_element(
                Rect::new(sb.x + 2.0, thumb_y, sb.w - 4.0, thumb_h),
                Color32F::new(0.6, 0.7, 1.0, 0.55), scale, off_x, off_y));
        }
    }

    // ── Pass 3: panel chrome (list area bg, path bar, title bar, shadow) ──

    out.push(solid_element(lr, Color32F::new(0.09, 0.11, 0.17, 1.0), scale, off_x, off_y));

    out.push(solid_element(
        Rect::new(p.x, p.y + TITLE_H + PATH_H - 1.0, p.w, 1.0),
        divider, scale, off_x, off_y));

    out.push(solid_element(
        Rect::new(p.x, p.y + p.h - BOT_H, p.w, BOT_H),
        header_bg, scale, off_x, off_y));

    out.push(solid_element(
        Rect::new(p.x, p.y + TITLE_H, p.w, PATH_H),
        path_bg, scale, off_x, off_y));

    out.push(solid_element(
        Rect::new(p.x, p.y, p.w, TITLE_H),
        header_bg, scale, off_x, off_y));

    out.push(solid_element(p, panel_bg, scale, off_x, off_y));

    let shadow_inset = -RADIUS;
    out.push(solid_element(
        Rect::new(p.x - shadow_inset - 8.0, p.y - shadow_inset + 6.0,
                  p.w + 2.0 * shadow_inset + 16.0,
                  p.h + 2.0 * shadow_inset + 4.0),
        shadow_col, scale, off_x, off_y));

    if let Some(o) = state.wm.output(output) {
        out.push(solid_element(o.bounds, Color32F::new(0.0, 0.0, 0.0, 0.45), scale, off_x, off_y));
    }
}
