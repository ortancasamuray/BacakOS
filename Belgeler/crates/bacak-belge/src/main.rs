use anyhow::{Context, Result};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::path::{Path, PathBuf};

slint::include_modules!();

// ── File-type detection ────────────────────────────────────────────────────

enum FileKind {
    Pdf,
    Epub,
    Image,
}

fn detect(path: &Path) -> Option<FileKind> {
    match path.extension()?.to_string_lossy().to_lowercase().as_str() {
        "pdf" => Some(FileKind::Pdf),
        "epub" => Some(FileKind::Epub),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tiff" | "tif"
        | "ico" | "qoi" | "hdr" | "pbm" | "pgm" | "ppm" | "pnm" => Some(FileKind::Image),
        _ => None,
    }
}

// ── Image helpers ──────────────────────────────────────────────────────────

fn image_siblings(path: &Path) -> Vec<PathBuf> {
    const EXTS: &[&str] = &[
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif",
        "ico", "qoi", "hdr", "pbm", "pgm", "ppm", "pnm",
    ];
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| EXTS.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

fn load_image(path: &Path) -> Option<Image> {
    let img = image::open(path).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), w, h);
    Some(Image::from_rgba8(buf))
}

fn image_info(path: &Path, idx: usize, total: usize) -> String {
    if let Ok((w, h)) = image::image_dimensions(path) {
        format!("{}×{}  {}/{}", w, h, idx + 1, total)
    } else {
        format!("{}/{}", idx + 1, total)
    }
}

// ── PDF helpers ────────────────────────────────────────────────────────────

const DPI_BASE: f32 = 96.0;

fn render_pdf_page(doc: &mupdf::Document, page_idx: i32, zoom: f32) -> Result<Image> {
    let page = doc.load_page(page_idx)?;
    let scale = zoom * DPI_BASE / 72.0;
    let mat = mupdf::Matrix::new_scale(scale, scale);
    let px = page.to_pixmap(&mat, &mupdf::Colorspace::device_rgb(), 0.0, true)?;
    let w = px.width() as u32;
    let h = px.height() as u32;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for chunk in px.samples().chunks(3) {
        rgba.extend_from_slice(chunk);
        rgba.push(255);
    }
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
    Ok(Image::from_rgba8(buf))
}

// ── EPUB helpers ───────────────────────────────────────────────────────────

fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut buf = String::new();
    for ch in html.chars() {
        match ch {
            '<' => { in_tag = true; buf.clear(); }
            '>' => {
                in_tag = false;
                let tag = buf.trim().to_lowercase();
                if matches!(tag.as_str(), "p" | "/p" | "br" | "br/" | "br /")
                    || tag.starts_with("h1") || tag.starts_with("h2") || tag.starts_with("h3")
                    || tag.starts_with("/h")
                {
                    out.push('\n');
                    if (tag.starts_with('h') && !tag.starts_with("/h")) || tag == "p" {
                        out.push('\n');
                    }
                }
                buf.clear();
            }
            _ if in_tag => { buf.push(ch); }
            '&' => { buf.clear(); buf.push('&'); }
            ';' if buf.starts_with('&') => {
                let entity = &buf[1..];
                out.push(match entity {
                    "amp" => '&', "lt" => '<', "gt" => '>',
                    "nbsp" => '\u{00A0}', "quot" => '"', "apos" => '\'',
                    _ => { out.push_str(&buf); out.push(';'); buf.clear(); continue; }
                });
                buf.clear();
            }
            _ if buf.starts_with('&') => { buf.push(ch); }
            _ => { out.push(ch); }
        }
    }
    // Collapse blank lines > 2
    let mut result = String::new();
    let mut blank = 0u32;
    for line in out.lines() {
        let t = line.trim();
        if t.is_empty() { blank += 1; if blank <= 2 { result.push('\n'); } }
        else { blank = 0; result.push_str(t); result.push('\n'); }
    }
    result
}

// ── Main ───────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("Kullanım: bacak-belge <dosya>")?;
    let path = path.canonicalize().unwrap_or(path);

    let kind = detect(&path).context("Desteklenmeyen dosya türü")?;

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let ui = AppWindow::new()?;
    ui.set_filename(filename.clone().into());
    ui.set_mode("loading".into());

    match kind {
        FileKind::Image => run_image(ui, path)?,
        FileKind::Pdf   => run_pdf(ui, path)?,
        FileKind::Epub  => run_epub(ui, path)?,
    }

    Ok(())
}

// ── Image mode ─────────────────────────────────────────────────────────────

fn run_image(ui: AppWindow, path: PathBuf) -> Result<()> {
    let siblings = image_siblings(&path);
    let start_idx = siblings.iter().position(|p| p == &path).unwrap_or(0);

    let idx = std::rc::Rc::new(std::cell::Cell::new(start_idx));
    let zoom = std::rc::Rc::new(std::cell::Cell::new(1.0f32));
    let siblings = std::rc::Rc::new(siblings);

    let load = {
        let ui_h = ui.as_weak();
        let sib  = siblings.clone();
        let z    = zoom.clone();
        move |i: usize| {
            let Some(ui) = ui_h.upgrade() else { return };
            let path = &sib[i];
            let img = load_image(path).unwrap_or_default();
            let info = image_info(path, i, sib.len());
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            ui.set_content_image(img);
            ui.set_filename(name.into());
            ui.set_status(info.into());
            ui.set_zoom(z.get());
            ui.set_has_prev(i > 0);
            ui.set_has_next(i + 1 < sib.len());
            ui.set_mode("image".into());
        }
    };

    // Deferred first load so window appears before heavy I/O
    let load_c = load.clone();
    let idx_init = start_idx;
    slint::Timer::single_shot(std::time::Duration::ZERO, move || load_c(idx_init));

    let idx_c = idx.clone(); let load_c = load.clone();
    ui.on_prev_action(move || {
        let i = idx_c.get(); if i > 0 { idx_c.set(i - 1); load_c(i - 1); }
    });

    let idx_c = idx.clone(); let sib_len = siblings.len(); let load_c = load.clone();
    ui.on_next_action(move || {
        let i = idx_c.get();
        if i + 1 < sib_len { idx_c.set(i + 1); load_c(i + 1); }
    });

    let z = zoom.clone(); let ui_h = ui.as_weak();
    ui.on_zoom_in(move || {
        if let Some(ui) = ui_h.upgrade() { let v = (z.get() * 1.25).min(8.0); z.set(v); ui.set_zoom(v); }
    });
    let z = zoom.clone(); let ui_h = ui.as_weak();
    ui.on_zoom_out(move || {
        if let Some(ui) = ui_h.upgrade() { let v = (z.get() / 1.25).max(0.1); z.set(v); ui.set_zoom(v); }
    });
    let z = zoom.clone(); let ui_h = ui.as_weak();
    ui.on_fit(move || { if let Some(ui) = ui_h.upgrade() { z.set(1.0); ui.set_zoom(1.0); } });

    let z = zoom.clone(); let ui_h = ui.as_weak();
    ui.on_pinch_ended(move |new_zoom| {
        if let Some(ui) = ui_h.upgrade() {
            let v = new_zoom.clamp(0.1, 8.0);
            z.set(v);
            ui.set_zoom(v);
        }
    });

    ui.run()?;
    Ok(())
}

// ── PDF mode ───────────────────────────────────────────────────────────────

fn run_pdf(ui: AppWindow, path: PathBuf) -> Result<()> {
    let doc = mupdf::Document::open(path.to_str().context("geçersiz yol")?)
        .context("PDF açılamadı")?;
    let page_count = doc.page_count()?;

    ui.set_page_count(page_count);
    ui.set_has_prev(false);
    ui.set_has_next(page_count > 1);
    ui.set_mode("pdf".into());

    let doc  = std::rc::Rc::new(doc);
    let pidx = std::rc::Rc::new(std::cell::Cell::new(0i32));
    let zoom = std::rc::Rc::new(std::cell::Cell::new(1.0f32));

    let refresh = {
        let ui_h = ui.as_weak();
        let doc  = doc.clone();
        let pi   = pidx.clone();
        let zi   = zoom.clone();
        let pc   = page_count;
        move || {
            let Some(ui) = ui_h.upgrade() else { return };
            let idx  = pi.get();
            let z    = zi.get();
            match render_pdf_page(&doc, idx, z) {
                Ok(img) => {
                    ui.set_content_image(img);
                    ui.set_page_num(idx + 1);
                    ui.set_zoom(z);
                    ui.set_status(format!("{}/{}", idx + 1, pc).into());
                    ui.set_has_prev(idx > 0);
                    ui.set_has_next(idx + 1 < pc);
                    ui.set_mode("pdf".into());
                }
                Err(e) => {
                    ui.set_status(format!("Hata: {e}").into());
                    ui.set_mode("error".into());
                }
            }
        }
    };

    let r = refresh.clone();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || r());

    let pi = pidx.clone(); let r = refresh.clone();
    ui.on_prev_action(move || {
        let i = pi.get(); if i > 0 { pi.set(i - 1); r(); }
    });

    let pi = pidx.clone(); let pc = page_count; let r = refresh.clone();
    ui.on_next_action(move || {
        let i = pi.get(); if i + 1 < pc { pi.set(i + 1); r(); }
    });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_zoom_in(move || { zi.set((zi.get() * 1.25).min(5.0)); r(); });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_zoom_out(move || { zi.set((zi.get() / 1.25).max(0.2)); r(); });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_fit(move || { zi.set(1.0); r(); });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_pinch_ended(move |new_zoom| {
        zi.set(new_zoom.clamp(0.2, 5.0));
        r();
    });

    ui.run()?;
    Ok(())
}

// ── EPUB mode ──────────────────────────────────────────────────────────────

fn run_epub(ui: AppWindow, path: PathBuf) -> Result<()> {
    let mut book = epub::doc::EpubDoc::new(&path).context("EPUB açılamadı")?;

    let title = book.mdata("title").map(|m| m.value.clone())
        .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string());

    let num_ch = book.get_num_chapters();

    let ch_titles: Vec<String> = (0..num_ch)
        .map(|i| {
            book.set_current_chapter(i);
            book.get_current_id()
                .map(|id| format!("Bölüm {} — {}", i + 1, id))
                .unwrap_or_else(|| format!("Bölüm {}", i + 1))
        })
        .collect();

    ui.set_filename(title.into());
    ui.set_mode("epub".into());

    let book = std::rc::Rc::new(std::cell::RefCell::new(book));
    let cidx = std::rc::Rc::new(std::cell::Cell::new(0usize));

    let load_ch = {
        let ui_h   = ui.as_weak();
        let book   = book.clone();
        let titles = ch_titles.clone();
        move |i: usize| {
            let Some(ui) = ui_h.upgrade() else { return };
            let mut b = book.borrow_mut();
            b.set_current_chapter(i);
            let text = b.get_current_str()
                .map(|(html, _)| html_to_text(&html))
                .unwrap_or_else(|| "(Bölüm okunamadı)".to_string());
            let ct = titles.get(i).cloned().unwrap_or_default();
            ui.set_chapter_title(ct.into());
            ui.set_body_text(text.into());
            ui.set_status(format!("{}/{}", i + 1, num_ch).into());
            ui.set_has_prev(i > 0);
            ui.set_has_next(i + 1 < num_ch);
            ui.set_mode("epub".into());
        }
    };

    let lc = load_ch.clone();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || lc(0));

    let ci = cidx.clone(); let lc = load_ch.clone();
    ui.on_prev_action(move || {
        let i = ci.get(); if i > 0 { ci.set(i - 1); lc(i - 1); }
    });

    let ci = cidx.clone(); let lc = load_ch.clone();
    ui.on_next_action(move || {
        let i = ci.get(); if i + 1 < num_ch { ci.set(i + 1); lc(i + 1); }
    });

    let ui_h = ui.as_weak();
    ui.on_font_larger(move || {
        if let Some(ui) = ui_h.upgrade() { ui.set_font_size((ui.get_font_size() + 2).min(32)); }
    });
    let ui_h = ui.as_weak();
    ui.on_font_smaller(move || {
        if let Some(ui) = ui_h.upgrade() { ui.set_font_size((ui.get_font_size() - 2).max(10)); }
    });
    let ui_h = ui.as_weak();
    ui.on_toggle_dark(move || {
        if let Some(ui) = ui_h.upgrade() { ui.set_dark_mode(!ui.get_dark_mode()); }
    });

    // zoom/fit not used in epub but must be bound
    ui.on_zoom_in(|| {});
    ui.on_zoom_out(|| {});
    ui.on_fit(|| {});

    let ui_h = ui.as_weak();
    ui.on_pinch_ended(move |scale| {
        if let Some(ui) = ui_h.upgrade() {
            let new_fs = ((ui.get_font_size() as f32 * scale).round() as i32).clamp(10, 32);
            ui.set_font_size(new_fs);
        }
    });

    ui.run()?;
    Ok(())
}
