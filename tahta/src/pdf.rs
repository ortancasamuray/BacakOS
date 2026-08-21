//! PDF import: decodes every page of a document to an RGBA image up front
//! (via `mupdf`, the same crate/pattern `bacak-belge` uses) so each becomes
//! a [`crate::board::Page`] the user can annotate over.

use std::sync::Arc;

use anyhow::Context;

use crate::board::PdfImage;

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

        pages.push(Arc::new(PdfImage::new(width, height, rgba)));
    }

    Ok(pages)
}
