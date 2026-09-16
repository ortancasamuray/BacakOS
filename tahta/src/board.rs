//! Page/background state: what's *behind* the ink. A board is a sequence
//! of [`Page`]s, each with its own strokes and its own background+grid
//! preset (or a decoded PDF page image instead of a flat background),
//! navigated with the toolbar's page buttons (auto-extends when you page
//! past the end, so there's no separate "add page" button).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use glam::Vec2;

use crate::stroke::{push_line, push_rect, Stroke, Vertex};

/// A single decoded PDF page, kept as plain RGBA pixels — texture upload
/// and caching is the renderer's job (see `renderer::Renderer::render`'s
/// `page_image` argument), keyed by `id` so a page already on the GPU
/// isn't re-uploaded every time it's paged back to.
pub struct PdfImage {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl PdfImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), width, height, rgba }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoardBackground {
    White,
    Black,
    DarkGray,
    Green,
}

impl BoardBackground {
    pub fn color(self) -> [f32; 4] {
        match self {
            BoardBackground::White => [0.94, 0.94, 0.96, 1.0],
            BoardBackground::Black => [0.05, 0.05, 0.06, 1.0],
            BoardBackground::DarkGray => [0.20, 0.21, 0.24, 1.0],
            BoardBackground::Green => [0.06, 0.28, 0.16, 1.0],
        }
    }

    /// Grid line color needs to contrast the background, not a fixed tone.
    pub fn grid_color(self) -> [f32; 4] {
        match self {
            BoardBackground::White => [0.0, 0.0, 0.0, 0.08],
            _ => [1.0, 1.0, 1.0, 0.10],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GridPattern {
    Plain,
    Lined,
    Checkered,
}

pub const GRID_SPACING: f32 = 48.0;

/// Curated background+grid combinations for the toolbar's single cycle
/// button — the full cross product (4 backgrounds x 3 patterns) is more
/// choice than a "tap once" control should expose.
pub const BACKGROUND_PRESETS: [(BoardBackground, GridPattern); 6] = [
    (BoardBackground::Black, GridPattern::Plain),
    (BoardBackground::White, GridPattern::Plain),
    (BoardBackground::White, GridPattern::Lined),
    (BoardBackground::White, GridPattern::Checkered),
    (BoardBackground::Green, GridPattern::Lined),
    (BoardBackground::DarkGray, GridPattern::Plain),
];

pub fn next_preset(current: (BoardBackground, GridPattern)) -> (BoardBackground, GridPattern) {
    let i = BACKGROUND_PRESETS.iter().position(|&p| p == current).unwrap_or(0);
    BACKGROUND_PRESETS[(i + 1) % BACKGROUND_PRESETS.len()]
}

pub struct Page {
    pub strokes: Vec<Stroke>,
    pub background: BoardBackground,
    pub grid: GridPattern,
    /// When set, this page is a PDF import: the image replaces the flat
    /// background/grid entirely and ink is annotated on top of it.
    pub pdf_image: Option<Arc<PdfImage>>,
}

impl Page {
    pub fn new() -> Self {
        Self { strokes: Vec::new(), background: BoardBackground::Black, grid: GridPattern::Plain, pdf_image: None }
    }

    pub fn from_pdf_image(image: Arc<PdfImage>) -> Self {
        Self { strokes: Vec::new(), background: BoardBackground::White, grid: GridPattern::Plain, pdf_image: Some(image) }
    }
}

/// Fills the viewport with `page`'s background color and (if set) a grid
/// that scrolls with `view_offset` so panning feels like moving over an
/// infinite sheet rather than a fixed-size page. No-op for PDF pages — the
/// renderer draws the page image itself instead (see `page.pdf_image`).
pub fn render_background(
    page: &Page,
    screen_size: Vec2,
    view_offset: Vec2,
    view_scale: f32,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    if page.pdf_image.is_some() {
        return;
    }

    push_rect(Vec2::ZERO, screen_size, page.background.color(), out_vertices, out_indices);

    if page.grid == GridPattern::Plain {
        return;
    }

    let grid_color = page.background.grid_color();
    // Spacing scales with zoom too, so the grid stays locked to the page
    // (not the screen) exactly like ink does.
    let spacing = GRID_SPACING * view_scale;
    let offset_x = view_offset.x.rem_euclid(spacing);
    let offset_y = view_offset.y.rem_euclid(spacing);

    if page.grid == GridPattern::Lined || page.grid == GridPattern::Checkered {
        let mut y = offset_y;
        while y < screen_size.y {
            push_line(Vec2::new(0.0, y), Vec2::new(screen_size.x, y), 1.0, grid_color, out_vertices, out_indices);
            y += spacing;
        }
    }
    if page.grid == GridPattern::Checkered {
        let mut x = offset_x;
        while x < screen_size.x {
            push_line(Vec2::new(x, 0.0), Vec2::new(x, screen_size.y), 1.0, grid_color, out_vertices, out_indices);
            x += spacing;
        }
    }
}
