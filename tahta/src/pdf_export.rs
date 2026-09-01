//! Hand-rolled minimal PDF writer for "PDF'e Dışa Aktar" — exports every
//! page's actual vector ink (not a pixel screenshot: tahta has no
//! render-to-texture pass over its own canvas, only a static-image
//! pipeline for imported PDF pages, so there's nothing to rasterize even
//! if we wanted to). No PDF-writing crate exists in this dependency set,
//! and pulling one in for a single feature is more than this needs — PDF
//! is a plain-text container format for exactly this (background fill,
//! straight line segments, one draw color at a time), so it's written
//! directly, the same "just enough, by hand" spirit as `digits.rs` and
//! `font5x7.rs`.
//!
//! Known simplifications: laser-pointer strokes are skipped (ephemeral
//! pointer marks, not meant to be permanent ink); an imported PDF page's
//! background *image* isn't re-embedded (only its flat background color/
//! grid and the ink drawn on top of it are), so annotations on top of an
//! imported PDF export onto a blank page instead of the original scan.

use std::path::PathBuf;

use glam::Vec2;

use crate::board::{GridPattern, Page, GRID_SPACING};
use crate::brush::BrushType;

/// Writes every page to a single multi-page PDF in a dedicated export
/// folder (`~/Belgeler/tahta-pdf/` if `~/Belgeler` exists, else
/// `~/tahta-pdf/`), named by export time so repeated exports never
/// collide. Returns the path written.
pub fn export_to_pdf(pages: &[Page], page_size: Vec2) -> anyhow::Result<PathBuf> {
    let dir = export_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("tahta-{}.pdf", local_timestamp()));

    let bytes = build_pdf(pages, page_size);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn export_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let belgeler = PathBuf::from(&home).join("Belgeler");
    let base = if belgeler.is_dir() { belgeler } else { PathBuf::from(home) };
    base.join("tahta-pdf")
}

/// `YYYY-MM-DD_HH-MM-SS` in the system's local timezone — no date/time
/// crate in this dependency set, so this goes through libc's
/// `localtime_r` directly (already a transitive dependency of the rest of
/// the stack, so this adds no real build cost).
fn local_timestamp() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}

fn build_pdf(pages: &[Page], page_size: Vec2) -> Vec<u8> {
    let (w, h) = (page_size.x, page_size.y);
    let page_count = pages.len().max(1);
    let mut out = Vec::new();
    let mut offsets = vec![0usize; 2 * page_count + 3]; // 1-indexed by object number

    out.extend_from_slice(b"%PDF-1.4\n");

    // Object 1: Catalog.
    offsets[1] = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages (kids are objects 3, 5, 7, ... one Page + one
    // Contents stream per page).
    offsets[2] = out.len();
    let mut kids = String::new();
    for i in 0..page_count {
        kids.push_str(&format!("{} 0 R ", 3 + i * 2));
    }
    out.extend_from_slice(format!("2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n", kids.trim_end(), page_count).as_bytes());

    let empty_page = Page::new();
    for i in 0..page_count {
        let page = pages.get(i).unwrap_or(&empty_page);
        let page_obj = 3 + i * 2;
        let contents_obj = page_obj + 1;
        let content = page_content(page, w, h);

        offsets[page_obj] = out.len();
        out.extend_from_slice(
            format!(
                "{page_obj} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w:.2} {h:.2}] \
/Resources << /ExtGState << /GSF << /Type /ExtGState /ca 1 >> /GSH << /Type /ExtGState /ca 0.35 >> >> >> \
/Contents {contents_obj} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        );

        offsets[contents_obj] = out.len();
        out.extend_from_slice(format!("{contents_obj} 0 obj\n<< /Length {} >>\nstream\n", content.len()).as_bytes());
        out.extend_from_slice(content.as_bytes());
        out.extend_from_slice(b"\nendstream\nendobj\n");
    }

    let xref_offset = out.len();
    let total_objects = offsets.len() - 1;
    out.extend_from_slice(format!("xref\n0 {}\n", total_objects + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for &offset in &offsets[1..] {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF", total_objects + 1).as_bytes(),
    );

    out
}

fn page_content(page: &Page, w: f32, h: f32) -> String {
    let mut s = String::new();

    let bg = page.background.color();
    s.push_str(&format!("{:.3} {:.3} {:.3} rg\n0 0 {w:.2} {h:.2} re f\n", bg[0], bg[1], bg[2]));

    let grid = page.background.grid_color();
    s.push_str(&format!("{:.3} {:.3} {:.3} RG\n0.75 w\n", grid[0], grid[1], grid[2]));
    match page.grid {
        GridPattern::Plain => {}
        GridPattern::Lined => draw_horizontal_grid(&mut s, w, h),
        GridPattern::Checkered => {
            draw_horizontal_grid(&mut s, w, h);
            draw_vertical_grid(&mut s, w, h);
        }
    }

    for stroke in &page.strokes {
        if stroke.brush_type == BrushType::LaserPointer || stroke.points.len() < 2 {
            continue;
        }
        let gs = if stroke.brush_type == BrushType::Highlighter { "GSH" } else { "GSF" };
        s.push_str(&format!("/{gs} gs\n{:.3} {:.3} {:.3} RG\n{:.2} w\n1 J 1 j\n", stroke.color[0], stroke.color[1], stroke.color[2], stroke.width));
        let p0 = stroke.points[0];
        s.push_str(&format!("{:.2} {:.2} m\n", p0.x, h - p0.y));
        for p in &stroke.points[1..] {
            s.push_str(&format!("{:.2} {:.2} l\n", p.x, h - p.y));
        }
        s.push_str("S\n");
    }

    s
}

fn draw_horizontal_grid(s: &mut String, w: f32, h: f32) {
    let mut y = 0.0;
    while y <= h {
        s.push_str(&format!("0 {y:.2} m\n{w:.2} {y:.2} l\nS\n"));
        y += GRID_SPACING;
    }
}

fn draw_vertical_grid(s: &mut String, w: f32, h: f32) {
    let mut x = 0.0;
    while x <= w {
        s.push_str(&format!("{x:.2} 0 m\n{x:.2} {h:.2} l\nS\n"));
        x += GRID_SPACING;
    }
}
