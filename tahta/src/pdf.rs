//! PDF import: decodes every page of a document to an RGBA image up front
//! (via `mupdf`, the same crate/pattern `bacak-belge` uses) so each becomes
//! a [`crate::board::Page`] the user can annotate over.

use std::sync::Arc;

use anyhow::Context;

use crate::board::{PdfImage, PdfSource};

/// Matches `bacak-belge`'s render resolution: MuPDF pages are natively
/// 72dpi, scaled up to a sharper 96dpi-equivalent for a big touch panel.
const DPI_BASE: f32 = 96.0;

pub fn load_pdf_pages(path: &str) -> anyhow::Result<Vec<Arc<PdfImage>>> {
    let doc = mupdf::Document::open(path).context("PDF açılamadı")?;
    let page_count = doc.page_count().context("sayfa sayısı okunamadı")?;

    let scale = DPI_BASE / 72.0;
    let matrix = mupdf::Matrix::new_scale(scale, scale);

    let mut pages = Vec::with_capacity(page_count.max(0) as usize);
    for index in 0..page_count {
        let page = doc.load_page(index).with_context(|| format!("sayfa {index} yüklenemedi"))?;
        let pixmap = page
            .to_pixmap(&matrix, &mupdf::Colorspace::device_rgb(), 0.0, true)
            .with_context(|| format!("sayfa {index} render edilemedi"))?;

        let width = pixmap.width();
        let height = pixmap.height();
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for chunk in pixmap.samples().chunks(3) {
            rgba.extend_from_slice(chunk);
            rgba.push(255);
        }

        let source = PdfSource { path: path.into(), page: index };
        pages.push(Arc::new(PdfImage::new(width, height, rgba).with_source(Some(source))));
    }

    Ok(pages)
}

/// Draws again just the part `[u0, v0, u1, v1]` (fractions of the page's
/// width/height) of the source page, its longer side `long_side` pixels —
/// a cut-out shown large stays sharp instead of an enlarged 96 dpi crop.
pub fn render_region(source: &PdfSource, [u0, v0, u1, v1]: [f32; 4], long_side: u32) -> anyhow::Result<PdfImage> {
    let path = source.path.to_str().context("PDF yolu UTF-8 değil")?;
    let doc = mupdf::Document::open(path).context("PDF açılamadı")?;
    let page = doc.load_page(source.page).context("sayfa yüklenemedi")?;
    let b = page.bounds().context("sayfa boyutu okunamadı")?;
    let (pw, ph) = (b.x1 - b.x0, b.y1 - b.y0);
    let (rx, ry) = (b.x0 + u0 * pw, b.y0 + v0 * ph);
    let (rw, rh) = ((u1 - u0) * pw, (v1 - v0) * ph);
    anyhow::ensure!(rw > 0.0 && rh > 0.0, "boş bölge");
    let scale = long_side as f32 / rw.max(rh);
    let (w, h) = ((rw * scale).round().max(1.0) as i32, (rh * scale).round().max(1.0) as i32);
    let mut pixmap = mupdf::Pixmap::new_with_w_h(&mupdf::Colorspace::device_rgb(), w, h, false)?;
    pixmap.clear_with(255)?;
    {
        let device = mupdf::Device::from_pixmap(&pixmap)?;
        let ctm = mupdf::Matrix::new(scale, 0.0, 0.0, scale, -rx * scale, -ry * scale);
        page.run(&device, &ctm).context("bölge çizilemedi")?;
    }
    let (w, h) = (pixmap.width(), pixmap.height());
    let n = pixmap.n() as usize;
    let stride = pixmap.stride() as usize;
    let samples = pixmap.samples();
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        for px in samples[y * stride..y * stride + w as usize * n].chunks(n) {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
    }
    Ok(PdfImage::new(w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;

    #[test]
    fn a_region_is_drawn_again_at_the_asked_size() {
        // A white page with one thick dark stroke across its top-left
        // quarter, through Tahta's own PDF writer.
        let mut page = crate::board::Page::new();
        page.background = crate::board::BoardBackground::White;
        let mut s = crate::stroke::Stroke::new([0.0, 0.0, 0.0, 1.0], 20.0, crate::brush::BrushType::Ballpoint);
        s.push_point(Vec2::new(0.0, 100.0), 0.0);
        s.push_point(Vec2::new(400.0, 100.0), 0.0);
        page.strokes.push(Arc::new(s));
        let pdf = crate::pdf_export::build_pdf(&[page], Vec2::new(800.0, 600.0));
        let path = std::env::temp_dir().join(format!("tahta-bolge-{}.pdf", std::process::id()));
        std::fs::write(&path, pdf).unwrap();

        let pages = load_pdf_pages(path.to_str().unwrap()).unwrap();
        let source = pages[0].source.clone().unwrap();
        assert_eq!(source.page, 0);
        // Top-left quarter, 1200 px wide: 12× the 96 dpi crop's detail.
        let img = render_region(&source, [0.0, 0.0, 0.5, 0.5], 1200).unwrap();
        assert_eq!((img.width, img.height), (1200, 900));
        let dark = |x: u32, y: u32| img.rgba[((y * img.width + x) * 4) as usize] < 60;
        // The stroke (y = 100 of 600 → row 300 of 900) is there, crisp.
        assert!(dark(600, 300));
        assert!(!dark(600, 100) && !dark(600, 500));
        let _ = std::fs::remove_file(&path);
    }
}
