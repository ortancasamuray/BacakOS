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
    /// Names this image's file in the board's save folder (see `persist`).
    /// Unlike `id` (a per-run counter) it's unique across runs, so images
    /// restored from an earlier session never collide with new ones.
    pub key: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The PDF page this image was drawn from, so a part of it can be
    /// drawn again sharper when it's cut out (`pdf::render_region`).
    /// `None` for screen grabs and cut-outs.
    pub source: Option<PdfSource>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PdfSource {
    pub path: std::path::PathBuf,
    pub page: i32,
}

impl PdfImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Self {
        let id = next_image_id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let key = nanos ^ ((std::process::id() as u64) << 32) ^ id;
        Self { id, key, width, height, rgba, source: None }
    }

    pub fn with_source(mut self, source: Option<PdfSource>) -> Self {
        self.source = source;
        self
    }

    /// An image read back from a board file, keeping its saved `key`.
    pub fn restored(key: u64, width: u32, height: u32, rgba: Vec<u8>) -> Self {
        Self { id: next_image_id(), key, width, height, rgba, source: None }
    }
}

/// Largest centered rect of a `width`×`height` image that fits within
/// `screen_size` (letterboxed), in screen-space pixels — where a PDF page
/// image is drawn (`renderer`), and how a frame on it maps back to its
/// pixels (`crop_image`).
pub fn letterbox_rect(width: u32, height: u32, screen_size: Vec2) -> (Vec2, Vec2) {
    let image_aspect = width as f32 / height.max(1) as f32;
    let screen_aspect = screen_size.x / screen_size.y;
    let size = if image_aspect > screen_aspect {
        Vec2::new(screen_size.x, screen_size.x / image_aspect)
    } else {
        Vec2::new(screen_size.y * image_aspect, screen_size.y)
    };
    ((screen_size - size) / 2.0, size)
}

/// Pixel bounds `[x0, y0, x1, y1)` of `image` under the screen-space
/// frame `(a, b)`, with `image` drawn letterboxed on a `screen_size`
/// screen. `None` when the frame misses the image or covers less than a
/// few pixels of it.
pub fn crop_bounds(image: &PdfImage, (a, b): (Vec2, Vec2), screen_size: Vec2) -> Option<[u32; 4]> {
    let (top_left, size) = letterbox_rect(image.width, image.height, screen_size);
    let dims = Vec2::new(image.width as f32, image.height as f32);
    let px = dims / size;
    let lo = ((a.min(b) - top_left) * px).clamp(Vec2::ZERO, dims).floor();
    let hi = ((a.max(b) - top_left) * px).clamp(Vec2::ZERO, dims).ceil();
    let [x0, y0, x1, y1] = [lo.x as u32, lo.y as u32, hi.x as u32, hi.y as u32];
    (x1 >= x0 + 8 && y1 >= y0 + 8).then_some([x0, y0, x1, y1])
}

/// The pixels of `image` within `crop_bounds`.
pub fn crop_image(image: &PdfImage, frame: (Vec2, Vec2), screen_size: Vec2) -> Option<PdfImage> {
    let [x0, y0, x1, y1] = crop_bounds(image, frame, screen_size)?.map(|v| v as usize);
    let stride = image.width as usize * 4;
    let mut rgba = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        rgba.extend_from_slice(&image.rgba[y * stride + x0 * 4..y * stride + x1 * 4]);
    }
    Some(PdfImage::new((x1 - x0) as u32, (y1 - y0) as u32, rgba))
}

/// An image placed on a page — a part cut out of a PDF page — that can be
/// moved and resized. Canvas space, like ink, so it pans/zooms with it.
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<PdfImage>,
    pub pos: Vec2,
    pub size: Vec2,
}

impl Picture {
    /// Screen-space (top-left, size) under the given pan/zoom.
    pub fn screen_rect(&self, view_offset: Vec2, view_scale: f32) -> (Vec2, Vec2) {
        (self.pos * view_scale + view_offset, self.size * view_scale)
    }
}

fn next_image_id() -> u64 {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
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

/// How many undo steps each page keeps. Snapshots share unchanged strokes
/// (`Arc`), so a step costs a pointer per stroke plus whatever it changed.
const HISTORY_LIMIT: usize = 50;

/// A page's undoable content at one point in time.
#[derive(Clone)]
struct Snapshot {
    strokes: Vec<Arc<Stroke>>,
    background: BoardBackground,
    grid: GridPattern,
    pictures: Vec<Picture>,
}

pub struct Page {
    /// Shared so undo snapshots are cheap: mutate through `Arc::make_mut`,
    /// which copies a stroke only while a snapshot still holds it.
    pub strokes: Vec<Arc<Stroke>>,
    pub background: BoardBackground,
    pub grid: GridPattern,
    /// When set, this page is a PDF import: the image replaces the flat
    /// background/grid entirely and ink is annotated on top of it.
    pub pdf_image: Option<Arc<PdfImage>>,
    /// Images placed on the page, drawn above the background and under
    /// the ink, in order (last on top). Undoable like ink.
    pub pictures: Vec<Picture>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl Page {
    pub fn new() -> Self {
        Self::with(BoardBackground::Black, GridPattern::Plain, None)
    }

    pub fn from_pdf_image(image: Arc<PdfImage>) -> Self {
        Self::with(BoardBackground::White, GridPattern::Plain, Some(image))
    }

    pub fn with(background: BoardBackground, grid: GridPattern, pdf_image: Option<Arc<PdfImage>>) -> Self {
        Self { strokes: Vec::new(), background, grid, pdf_image, pictures: Vec::new(), undo: Vec::new(), redo: Vec::new() }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            strokes: self.strokes.clone(),
            background: self.background,
            grid: self.grid,
            pictures: self.pictures.clone(),
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.strokes = s.strokes;
        self.background = s.background;
        self.grid = s.grid;
        self.pictures = s.pictures;
    }

    /// Record the current state as an undo step — call right before any
    /// change the user may want to take back (a stroke, an erase gesture,
    /// Clear, a background change). Starts a new branch: redo is dropped.
    pub fn checkpoint(&mut self) {
        let snap = self.snapshot();
        self.undo.push(snap);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Forget the latest checkpoint without applying it, when the change it
    /// guarded was itself withdrawn (the double-tap dot) — but only if it
    /// really is that change's checkpoint, i.e. it held `stroke_count`
    /// strokes (a laser dot never set one).
    pub fn discard_checkpoint(&mut self, stroke_count: usize) {
        if self.undo.last().is_some_and(|s| s.strokes.len() == stroke_count) {
            self.undo.pop();
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        let cur = self.snapshot();
        self.redo.push(cur);
        self.restore(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        let cur = self.snapshot();
        self.undo.push(cur);
        self.restore(next);
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::BrushType;

    fn add_stroke(page: &mut Page, x: f32) {
        page.checkpoint();
        let mut s = Stroke::new([1.0; 4], 3.0, BrushType::Ballpoint);
        s.push_point(Vec2::new(x, 0.0), 0.0);
        page.strokes.push(Arc::new(s));
    }

    #[test]
    fn undo_redo_walks_strokes_clear_and_background() {
        let mut p = Page::new();
        assert!(!p.can_undo() && !p.can_redo());
        add_stroke(&mut p, 1.0);
        add_stroke(&mut p, 2.0);
        // Clear is just another undoable step.
        p.checkpoint();
        p.strokes.clear();
        p.checkpoint();
        p.background = BoardBackground::Green;

        assert!(p.undo());
        assert_eq!(p.background, BoardBackground::Black);
        assert!(p.undo());
        assert_eq!(p.strokes.len(), 2, "Clear taken back");
        assert!(p.undo());
        assert_eq!(p.strokes.len(), 1);
        assert!(p.redo());
        assert_eq!(p.strokes.len(), 2);

        // A new change drops the redo branch.
        add_stroke(&mut p, 3.0);
        assert!(!p.can_redo());
        assert!(!p.redo());
    }

    #[test]
    fn crop_maps_the_frame_onto_the_letterboxed_image() {
        let screen = Vec2::new(400.0, 400.0);
        assert_eq!(letterbox_rect(40, 20, screen), (Vec2::new(0.0, 100.0), Vec2::new(400.0, 200.0)));
        let big: Vec<u8> = (0..(40 * 20)).flat_map(|i: u32| [(i % 40) as u8, (i / 40) as u8, 0, 255]).collect();
        let img = PdfImage::new(40, 20, big);
        assert!(crop_image(&img, (Vec2::new(10.0, 10.0), Vec2::new(390.0, 90.0)), screen).is_none(), "above the image");
        // 10 px per image pixel; frame image pixels x 10..30, y 5..20,
        // reaching past the image's bottom (clamped).
        let c = crop_image(&img, (Vec2::new(300.0, 1000.0), Vec2::new(100.0, 150.0)), screen).unwrap();
        assert_eq!((c.width, c.height), (20, 15));
        assert_eq!(&c.rgba[0..2], &[10, 5]);
        assert_eq!(&c.rgba[c.rgba.len() - 4..c.rgba.len() - 2], &[29, 19]);
    }

    #[test]
    fn pictures_are_part_of_undo() {
        let mut p = Page::new();
        let img = Arc::new(PdfImage::new(1, 1, vec![0; 4]));
        p.checkpoint();
        p.pictures.push(Picture { image: img, pos: Vec2::ZERO, size: Vec2::splat(10.0) });
        p.checkpoint();
        p.pictures[0].size = Vec2::splat(30.0);
        assert!(p.undo());
        assert_eq!(p.pictures[0].size, Vec2::splat(10.0));
        assert!(p.undo());
        assert!(p.pictures.is_empty());
    }

    #[test]
    fn snapshots_share_strokes_until_one_changes() {
        let mut p = Page::new();
        add_stroke(&mut p, 1.0);
        p.checkpoint();
        // Erasing un-shares just the touched stroke; the snapshot keeps
        // the original.
        Arc::make_mut(&mut p.strokes[0]).clear();
        assert!(p.strokes[0].points.is_empty());
        assert!(p.undo());
        assert_eq!(p.strokes[0].points.len(), 1);
    }

    #[test]
    fn history_is_capped_and_withdrawn_dot_leaves_no_step() {
        let mut p = Page::new();
        for i in 0..(HISTORY_LIMIT + 10) {
            add_stroke(&mut p, i as f32);
        }
        let mut steps = 0;
        while p.undo() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_LIMIT);

        let mut p = Page::new();
        add_stroke(&mut p, 1.0);
        add_stroke(&mut p, 2.0); // the double-tap's first dot
        p.strokes.pop();
        p.discard_checkpoint(1);
        assert!(p.undo());
        assert!(p.strokes.is_empty(), "one undo takes back the real stroke, not the dot");
        // A checkpoint that doesn't match (laser dot set none) is kept.
        let mut p = Page::new();
        add_stroke(&mut p, 1.0);
        p.discard_checkpoint(5);
        assert!(p.can_undo());
    }
}
