use anyhow::{Context, Result};
use mupdf::{Document, Matrix};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::path::PathBuf;

slint::include_modules!();

const DPI_BASE: f32 = 96.0;

fn render_page(doc: &Document, page_idx: i32, zoom: f32) -> Result<Image> {
    let page = doc.load_page(page_idx)?;
    let scale = zoom * DPI_BASE / 72.0;
    let matrix = Matrix::new_scale(scale, scale);
    let pixmap = page.to_pixmap(&matrix, &mupdf::Colorspace::device_rgb(), 0.0, true)?;

    let w = pixmap.width() as u32;
    let h = pixmap.height() as u32;
    let samples = pixmap.samples();

    // MuPDF gives RGB; Slint wants RGBA
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for chunk in samples.chunks(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }

    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
    Ok(Image::from_rgba8(buf))
}

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("Kullanım: bacak-pdf <dosya.pdf>")?;

    let doc = Document::open(path.to_str().context("geçersiz yol")?)
        .context("PDF açılamadı")?;

    let page_count = doc.page_count()?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("belge.pdf")
        .to_string();

    let ui = AppWindow::new()?;
    ui.set_filename(filename.into());
    ui.set_page_count(page_count);

    let doc = std::rc::Rc::new(doc);
    let page_idx = std::rc::Rc::new(std::cell::Cell::new(0i32));
    let zoom_rc  = std::rc::Rc::new(std::cell::Cell::new(1.0f32));

    let refresh = {
        let ui_h   = ui.as_weak();
        let doc_rc = doc.clone();
        let pi     = page_idx.clone();
        let zi     = zoom_rc.clone();
        move || {
            let Some(ui) = ui_h.upgrade() else { return };
            let idx  = pi.get();
            let zoom = zi.get();
            ui.set_loading(true);
            match render_page(&doc_rc, idx, zoom) {
                Ok(img) => {
                    ui.set_page_image(img);
                    ui.set_page_num(idx + 1);
                    ui.set_zoom(zoom);
                }
                Err(e) => eprintln!("Sayfa render hatası: {e}"),
            }
            ui.set_loading(false);
        }
    };

    // Defer first render until the event loop is running so the window
    // appears immediately with the "Yükleniyor…" spinner visible.
    let r_deferred = refresh.clone();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || r_deferred());

    let pi_c = page_idx.clone();
    let r_c  = refresh.clone();
    ui.on_prev_page(move || {
        let i = pi_c.get();
        if i > 0 { pi_c.set(i - 1); r_c(); }
    });

    let pi_c = page_idx.clone();
    let pc   = page_count;
    let r_c  = refresh.clone();
    ui.on_next_page(move || {
        let i = pi_c.get();
        if i + 1 < pc { pi_c.set(i + 1); r_c(); }
    });

    let zi_c = zoom_rc.clone();
    let r_c  = refresh.clone();
    ui.on_zoom_in(move || { zi_c.set((zi_c.get() * 1.25).min(5.0)); r_c(); });

    let zi_c = zoom_rc.clone();
    let r_c  = refresh.clone();
    ui.on_zoom_out(move || { zi_c.set((zi_c.get() / 1.25).max(0.2)); r_c(); });

    let zi_c = zoom_rc.clone();
    let r_c  = refresh.clone();
    ui.on_fit_width(move || { zi_c.set(1.0); r_c(); });

    let pi_c = page_idx.clone();
    let pc   = page_count;
    let r_c  = refresh.clone();
    ui.on_goto_page(move |p| {
        let i = (p - 1).clamp(0, pc - 1);
        pi_c.set(i);
        r_c();
    });

    ui.run()?;
    Ok(())
}
