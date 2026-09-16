//! Tier A native text selection — Android-style selection over text the
//! **compositor itself lays out** (a built-in viewer/notes panel, later a
//! terminal). Because we own the [`cosmic_text`] layout we have the one thing
//! Wayland never gives us for foreign clients: glyph geometry. That lets us
//! hit-test taps to a caret, expand to words/lines, drag a range, and place
//! pixel-accurate selection handles + highlight — none of which is possible
//! over Firefox/foreign terminals (those are Tier B, input-synthesis only).
//!
//! This module is the pure-ish engine: it owns a `FontSystem` + `Buffer` for
//! one string at a given width and exposes selection mutation + geometry. The
//! renderer turns [`NativeText::highlight_rects`] / [`NativeText::handle_points`]
//! into draw calls; the host panel routes gestures into the mutators. All
//! coordinates are **text-local** (origin at the text's top-left) — the host
//! offsets by the panel position.
#![cfg(feature = "runtime")]

use std::sync::Arc;

use cosmic_text::{Attrs, Buffer, Color, Cursor, FontSystem, Metrics, Shaping, SwashCache};

use crate::wm::Rect;

/// Which end of the selection a handle drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    /// The leading (ordered-min) handle.
    Start,
    /// The trailing (ordered-max) handle.
    End,
}

/// A selectable run of compositor-laid-out text plus its live selection.
pub struct NativeText {
    font_system: FontSystem,
    buffer: Buffer,
    /// Glyph bitmap cache for [`rasterize`](NativeText::rasterize).
    swash: SwashCache,
    /// Layout width the buffer was last sized to (the rasterised bitmap width).
    width: f32,
    /// Selection endpoints, *unordered*: `anchor` is the fixed end, `focus` the
    /// one a drag/handle moves. Equal endpoints = a bare caret (no highlight).
    anchor: Cursor,
    focus: Cursor,
}

impl NativeText {
    /// Lay out `text` at `width` px with the given font + line metrics, sharing
    /// the same system face the UI overlays use (so it matches and tests are
    /// deterministic). Falls back to scanning system fonts if that face is
    /// missing.
    pub fn new(text: &str, font_px: f32, line_height: f32, width: f32) -> Self {
        let mut font_system = match crate::text::TextRenderer::ui_font_bytes() {
            Some(bytes) => {
                let src = cosmic_text::fontdb::Source::Binary(Arc::new(bytes));
                FontSystem::new_with_fonts([src])
            }
            None => FontSystem::new(),
        };
        let metrics = Metrics::new(font_px, line_height);
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(&mut font_system, Some(width.max(1.0)), None);
        buffer.set_text(
            &mut font_system,
            text,
            &Attrs::new(),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut font_system, false);
        Self {
            font_system,
            buffer,
            swash: SwashCache::new(),
            width: width.max(1.0),
            anchor: Cursor::new(0, 0),
            focus: Cursor::new(0, 0),
        }
    }

    /// Re-flow at a new width (panel resize).
    pub fn set_width(&mut self, width: f32) {
        self.width = width.max(1.0);
        self.buffer.set_size(&mut self.font_system, Some(self.width), None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);
    }

    /// Total laid-out size `(width, height)` in px — the host sizes the panel
    /// from this. Width is the wrap width; height spans every visual row.
    pub fn size(&self) -> (f32, f32) {
        (self.width, self.text_height())
    }

    /// Sum of every visual row's height.
    fn text_height(&self) -> f32 {
        self.buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0_f32, f32::max)
    }

    /// Rasterise the whole text block into a premultiplied-free RGBA buffer
    /// (`[R,G,B,A]` per pixel — matches the `Abgr8888` the overlay labels use)
    /// at scale 1, glyphs in `rgb`. Static given the text/width, so the host
    /// caches the result and only re-runs it on a re-layout. `None` if the
    /// layout is empty (no font / empty text).
    pub fn rasterize(&mut self, rgb: [u8; 3]) -> Option<(Vec<u8>, usize, usize)> {
        let (w, h) = self.size();
        let (w, h) = (w.ceil() as usize, h.ceil() as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let mut rgba = vec![0u8; w * h * 4];
        let color = Color::rgba(rgb[0], rgb[1], rgb[2], 0xFF);
        // `draw` hands us per-pixel coverage already folded into the colour's
        // alpha; src-over composite onto the transparent buffer.
        self.buffer
            .draw(&mut self.font_system, &mut self.swash, color, |x, y, cw, ch, col| {
                let a = col.a();
                if a == 0 {
                    return;
                }
                let (cr, cg, cb) = (col.r(), col.g(), col.b());
                for yy in y..y + ch as i32 {
                    for xx in x..x + cw as i32 {
                        if xx < 0 || yy < 0 || xx as usize >= w || yy as usize >= h {
                            continue;
                        }
                        let i = (yy as usize * w + xx as usize) * 4;
                        let sa = a as f32 / 255.0;
                        let inv = 1.0 - sa;
                        rgba[i] = (cr as f32 * sa + rgba[i] as f32 * inv) as u8;
                        rgba[i + 1] = (cg as f32 * sa + rgba[i + 1] as f32 * inv) as u8;
                        rgba[i + 2] = (cb as f32 * sa + rgba[i + 2] as f32 * inv) as u8;
                        rgba[i + 3] = (a as f32 + rgba[i + 3] as f32 * inv) as u8;
                    }
                }
            });
        Some((rgba, w, h))
    }

    /// Read-only access for the renderer (glyph iteration).
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// True when the highlight is non-empty (endpoints differ).
    pub fn has_selection(&self) -> bool {
        ordered(self.anchor, self.focus).map(|(s, e)| s != e).unwrap_or(false)
    }

    /// Drop the selection back to a bare caret at the current focus.
    pub fn clear(&mut self) {
        self.anchor = self.focus;
    }

    /// Select the entire buffer (Select all).
    pub fn select_all(&mut self) {
        let last = self.buffer.lines.len().saturating_sub(1);
        let end = self.line_text(last).len();
        self.anchor = Cursor::new(0, 0);
        self.focus = Cursor::new(last, end);
    }

    // --- gesture-driven mutators (text-local coords) ----------------------

    /// Tap: place a bare caret under `(x, y)` (no highlight).
    pub fn set_caret_at(&mut self, x: f32, y: f32) {
        if let Some(c) = self.buffer.hit(x, y) {
            self.anchor = c;
            self.focus = c;
        }
    }

    /// Double-tap / long-press: select the word under `(x, y)`.
    pub fn select_word_at(&mut self, x: f32, y: f32) {
        let Some(c) = self.buffer.hit(x, y) else { return };
        let line = self.line_text(c.line);
        let (s, e) = word_range(line, c.index);
        self.anchor = Cursor::new(c.line, s);
        self.focus = Cursor::new(c.line, e);
    }

    /// Triple-tap: select the whole logical line under `(x, y)`.
    pub fn select_line_at(&mut self, x: f32, y: f32) {
        let Some(c) = self.buffer.hit(x, y) else { return };
        let len = self.line_text(c.line).len();
        self.anchor = Cursor::new(c.line, 0);
        self.focus = Cursor::new(c.line, len);
    }

    /// Drag-select: begin a range whose fixed end is `(x, y)`.
    pub fn begin_drag_at(&mut self, x: f32, y: f32) {
        if let Some(c) = self.buffer.hit(x, y) {
            self.anchor = c;
            self.focus = c;
        }
    }

    /// Drag-select / handle-drag: move the focus end to `(x, y)`.
    pub fn extend_to(&mut self, x: f32, y: f32) {
        if let Some(c) = self.buffer.hit(x, y) {
            self.focus = c;
        }
    }

    /// If `(x, y)` is within `radius` of a selection handle, grab it: re-seat
    /// the anchor to the *opposite* endpoint so the grabbed handle becomes the
    /// `focus` that [`extend_to`] then moves. Returns which handle was grabbed.
    ///
    /// [`extend_to`]: NativeText::extend_to
    pub fn grab_handle(&mut self, x: f32, y: f32, radius: f32) -> Option<Handle> {
        let (sp, ep) = self.handle_points()?;
        let (s, e) = ordered(self.anchor, self.focus)?;
        let near = |p: (f32, f32)| (p.0 - x).hypot(p.1 - y) <= radius;
        // Prefer whichever handle is closer if both are in range.
        let ds = (sp.0 - x).hypot(sp.1 - y);
        let de = (ep.0 - x).hypot(ep.1 - y);
        if near(sp) && (ds <= de || !near(ep)) {
            self.anchor = e; // fix the end; drag the start
            self.focus = s;
            Some(Handle::Start)
        } else if near(ep) {
            self.anchor = s; // fix the start; drag the end
            self.focus = e;
            Some(Handle::End)
        } else {
            None
        }
    }

    // --- geometry / content queries (text-local coords) -------------------

    /// The selected substring (`\n`-joined across logical lines). Empty when
    /// there's no selection.
    pub fn selected_text(&self) -> String {
        let Some((s, e)) = ordered(self.anchor, self.focus) else { return String::new() };
        if s == e {
            return String::new();
        }
        if s.line == e.line {
            return slice(self.line_text(s.line), s.index, e.index).to_string();
        }
        let mut out = String::new();
        out.push_str(slice_from(self.line_text(s.line), s.index));
        for l in (s.line + 1)..e.line {
            out.push('\n');
            out.push_str(self.line_text(l));
        }
        out.push('\n');
        out.push_str(slice_to(self.line_text(e.line), e.index));
        out
    }

    /// One highlight rect per visual row the selection covers (text-local px).
    pub fn highlight_rects(&self) -> Vec<Rect> {
        let Some((s, e)) = ordered(self.anchor, self.focus) else { return Vec::new() };
        if s == e {
            return Vec::new();
        }
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            if let Some((x, w)) = run.highlight(s, e) {
                if w > 0.5 {
                    rects.push(Rect::new(x, run.line_top, w, run.line_height));
                }
            }
        }
        rects
    }

    /// Bottom-of-line points for the two selection handles `(start, end)`, or
    /// `None` when there's no selection. Each is `(x, y)` text-local px.
    pub fn handle_points(&self) -> Option<((f32, f32), (f32, f32))> {
        let (s, e) = ordered(self.anchor, self.focus)?;
        if s == e {
            return None;
        }
        Some((self.caret_point(s)?, self.caret_point(e)?))
    }

    // --- internals --------------------------------------------------------

    fn line_text(&self, line: usize) -> &str {
        self.buffer.lines.get(line).map(|l| l.text()).unwrap_or("")
    }

    /// Bottom point `(x, y)` of the caret at `cursor`, scanning the layout run
    /// that owns its byte index.
    fn caret_point(&self, cursor: Cursor) -> Option<(f32, f32)> {
        let mut chosen: Option<(f32, f32, f32)> = None; // (x, line_top, line_height)
        for run in self.buffer.layout_runs() {
            if run.line_i != cursor.line {
                continue;
            }
            // Caret x: left edge of the first glyph at/after the index, else the
            // right edge of the last glyph before it (end-of-line carets).
            let mut x = 0.0;
            for g in run.glyphs {
                if cursor.index <= g.start {
                    x = g.x;
                    break;
                }
                x = g.x + g.w;
            }
            let run_end = run.glyphs.last().map(|g| g.end).unwrap_or(0);
            chosen = Some((x, run.line_top, run.line_height));
            // This run owns the index if the index doesn't run past it; for a
            // wrapped logical line that means stop at the right visual row.
            if cursor.index <= run_end {
                break;
            }
        }
        chosen.map(|(x, top, h)| (x, top + h))
    }
}

/// Order two cursors as `(min, max)` by `(line, index)`. `None` only if either
/// is unusable (never, given construction) — kept as `Option` for ergonomic
/// `?` chaining at call sites.
fn ordered(a: Cursor, b: Cursor) -> Option<(Cursor, Cursor)> {
    if (a.line, a.index) <= (b.line, b.index) {
        Some((a, b))
    } else {
        Some((b, a))
    }
}

/// Byte range of the word containing `index` in `text`, treating
/// alphanumeric + `_` as word characters. Returns `(index, index)` (empty)
/// when `index` isn't on a word char.
fn word_range(text: &str, index: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let idx = clamp_boundary(text, index);
    let start = text[..idx]
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(idx);
    let end = text[idx..]
        .char_indices()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map(|(i, c)| idx + i + c.len_utf8())
        .unwrap_or(idx);
    (start, end)
}

/// Clamp a byte index to the nearest char boundary ≤ `index` (defensive; hit()
/// already returns boundaries, but word math indexes raw bytes).
fn clamp_boundary(text: &str, index: usize) -> usize {
    let mut i = index.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn slice(text: &str, a: usize, b: usize) -> &str {
    let a = clamp_boundary(text, a);
    let b = clamp_boundary(text, b);
    text.get(a..b).unwrap_or("")
}
fn slice_from(text: &str, a: usize) -> &str {
    let a = clamp_boundary(text, a);
    text.get(a..).unwrap_or("")
}
fn slice_to(text: &str, b: usize) -> &str {
    let b = clamp_boundary(text, b);
    text.get(..b).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a NativeText, or skip the test if no system font is available
    /// (the engine is a no-op without a face, like the rest of the UI text).
    fn text(s: &str) -> Option<NativeText> {
        let t = NativeText::new(s, 18.0, 24.0, 800.0);
        // No font → no glyphs → hit-testing can't work; skip.
        if t.buffer().layout_runs().next().is_none() {
            eprintln!("skipping: no system font");
            return None;
        }
        Some(t)
    }

    #[test]
    fn word_range_picks_whole_word() {
        assert_eq!(word_range("hello world", 2), (0, 5));
        assert_eq!(word_range("hello world", 8), (6, 11));
        // At the trailing boundary of a word, expand back over it (a tap at a
        // word's end still selects that word).
        assert_eq!(word_range("hello world", 5), (0, 5));
        // Truly inside whitespace (space on both sides) → empty.
        assert_eq!(word_range("a  b", 2), (2, 2));
    }

    #[test]
    fn double_tap_selects_word() {
        let Some(mut t) = text("hello world") else { return };
        // Hit somewhere in the first word; y in the first line.
        t.select_word_at(10.0, 12.0);
        assert!(t.has_selection());
        assert_eq!(t.selected_text(), "hello");
    }

    #[test]
    fn triple_tap_selects_line() {
        let Some(mut t) = text("hello world") else { return };
        t.select_line_at(10.0, 12.0);
        assert_eq!(t.selected_text(), "hello world");
    }

    #[test]
    fn caret_then_no_selection() {
        let Some(mut t) = text("hello world") else { return };
        t.set_caret_at(10.0, 12.0);
        assert!(!t.has_selection());
        assert_eq!(t.selected_text(), "");
        assert!(t.highlight_rects().is_empty());
        assert!(t.handle_points().is_none());
    }

    #[test]
    fn selection_has_highlight_and_two_handles() {
        let Some(mut t) = text("hello world") else { return };
        t.select_word_at(10.0, 12.0);
        assert!(!t.highlight_rects().is_empty(), "a selection draws highlight");
        let (start, end) = t.handle_points().expect("two handles");
        // Start handle is left of end handle on one line.
        assert!(start.0 < end.0, "start handle left of end handle");
        // Handles sit at the bottom of the line (y > 0).
        assert!(start.1 > 0.0 && end.1 > 0.0);
    }

    #[test]
    fn grabbing_end_handle_then_extending_grows_selection() {
        let Some(mut t) = text("hello world") else { return };
        t.select_word_at(10.0, 12.0); // "hello"
        assert_eq!(t.selected_text(), "hello");
        let (_s, e) = t.handle_points().unwrap();
        // Grab the end handle and drag it well to the right → grows to include
        // more of the line.
        assert_eq!(t.grab_handle(e.0, e.1, 40.0), Some(Handle::End));
        t.extend_to(10_000.0, 12.0); // far right → end of line
        assert_eq!(t.selected_text(), "hello world");
    }
}
