//! Page/background state: what's *behind* the ink. A board is a sequence
//! of [`Page`]s, each with its own strokes and its own background+grid
//! preset, navigated with the toolbar's page buttons (auto-extends when
//! you page past the end, so there's no separate "add page" button).

use glam::Vec2;

use crate::stroke::{push_line, push_rect, Stroke, Vertex};

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
    fn grid_color(self) -> [f32; 4] {
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

const GRID_SPACING: f32 = 48.0;

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
}

impl Page {
    pub fn new() -> Self {
        Self { strokes: Vec::new(), background: BoardBackground::Black, grid: GridPattern::Plain }
    }
}

/// Fills the viewport with `page`'s background color and (if set) a grid
/// that scrolls with `view_offset` so panning feels like moving over an
/// infinite sheet rather than a fixed-size page.
pub fn render_background(
    page: &Page,
    screen_size: Vec2,
    view_offset: Vec2,
    out_vertices: &mut Vec<Vertex>,
    out_indices: &mut Vec<u32>,
) {
    push_rect(Vec2::ZERO, screen_size, page.background.color(), out_vertices, out_indices);

    if page.grid == GridPattern::Plain {
        return;
    }

    let grid_color = page.background.grid_color();
    let offset_x = view_offset.x.rem_euclid(GRID_SPACING);
    let offset_y = view_offset.y.rem_euclid(GRID_SPACING);

    if page.grid == GridPattern::Lined || page.grid == GridPattern::Checkered {
        let mut y = offset_y;
        while y < screen_size.y {
            push_line(Vec2::new(0.0, y), Vec2::new(screen_size.x, y), 1.0, grid_color, out_vertices, out_indices);
            y += GRID_SPACING;
        }
    }
    if page.grid == GridPattern::Checkered {
        let mut x = offset_x;
        while x < screen_size.x {
            push_line(Vec2::new(x, 0.0), Vec2::new(x, screen_size.y), 1.0, grid_color, out_vertices, out_indices);
            x += GRID_SPACING;
        }
    }
}
