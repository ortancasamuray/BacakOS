use anyhow::{Context, Result};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

slint::include_modules!();

// ── Annotation / drawing ───────────────────────────────────────────────────

struct AnnotationState {
    base_buf:     Vec<u8>,  // completed strokes (RGBA)
    composite:    Vec<u8>,  // base_buf + current stroke preview
    stroke_buf:   Vec<u8>,  // current marker stroke at full opacity
    last_pt:      Option<(f32, f32)>,
    pen_color:    [u8; 4],  // RGB + alpha (220 pen / 255 marker)
    pen_radius:   f32,
    is_marker:    bool,
    marker_alpha: u8,       // layer opacity for marker strokes
    canvas_w:     u32,
    canvas_h:     u32,
}

impl AnnotationState {
    fn new() -> Self {
        Self {
            base_buf:     Vec::new(),
            composite:    Vec::new(),
            stroke_buf:   Vec::new(),
            last_pt:      None,
            pen_color:    [255, 68, 68, 220],
            pen_radius:   3.5,
            is_marker:    false,
            marker_alpha: 110,
            canvas_w:     0,
            canvas_h:     0,
        }
    }

    fn ensure_sized(&mut self, w: u32, h: u32) {
        let need = (w * h * 4) as usize;
        if self.canvas_w != w || self.canvas_h != h {
            self.canvas_w = w;
            self.canvas_h = h;
            self.base_buf.resize(need, 0);
            self.composite.resize(need, 0);
            self.stroke_buf.resize(need, 0);
        }
    }

    fn to_image(&self) -> Image {
        if self.canvas_w == 0 || self.canvas_h == 0 {
            return Image::default();
        }
        let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &self.composite, self.canvas_w, self.canvas_h,
        );
        Image::from_rgba8(buf)
    }
}

fn alpha_blend(dst: &mut [u8], idx: usize, color: [u8; 4], coverage: f32) {
    let sa = color[3] as f32 / 255.0 * coverage;
    let da = dst[idx + 3] as f32 / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa < 1e-4 { return; }
    dst[idx]     = ((color[0] as f32 * sa + dst[idx]     as f32 * da * (1.0 - sa)) / oa) as u8;
    dst[idx + 1] = ((color[1] as f32 * sa + dst[idx + 1] as f32 * da * (1.0 - sa)) / oa) as u8;
    dst[idx + 2] = ((color[2] as f32 * sa + dst[idx + 2] as f32 * da * (1.0 - sa)) / oa) as u8;
    dst[idx + 3] = (oa * 255.0) as u8;
}

fn stamp_circle(buf: &mut [u8], w: u32, h: u32, cx: f32, cy: f32, r: f32, color: [u8; 4]) {
    let x0 = ((cx - r - 1.0) as i32).max(0) as u32;
    let x1 = ((cx + r + 1.0) as i32).min(w as i32 - 1) as u32;
    let y0 = ((cy - r - 1.0) as i32).max(0) as u32;
    let y1 = ((cy + r + 1.0) as i32).min(h as i32 - 1) as u32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let dist = (((px as f32 - cx).powi(2) + (py as f32 - cy).powi(2)) as f32).sqrt();
            if dist > r + 1.0 { continue; }
            let coverage = (r + 1.0 - dist).clamp(0.0, 1.0);
            let idx = ((py * w + px) * 4) as usize;
            alpha_blend(buf, idx, color, coverage);
        }
    }
}

/// Composite a full-opacity stroke layer onto dst at reduced opacity (for marker tool).
/// Prevents alpha accumulation within a single stroke — uniform transparency across the whole path.
fn blend_layer(dst: &mut [u8], src: &[u8], alpha: u8) {
    let npix = dst.len() / 4;
    for i in 0..npix {
        let si = i * 4;
        let src_a = src[si + 3];
        if src_a == 0 { continue; }
        let eff_a = (src_a as u32 * alpha as u32 / 255) as u8;
        alpha_blend(dst, si, [src[si], src[si + 1], src[si + 2], eff_a], 1.0);
    }
}

fn draw_segment(buf: &mut [u8], w: u32, h: u32, p0: (f32, f32), p1: (f32, f32), r: f32, color: [u8; 4]) {
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let dist = (dx * dx + dy * dy).sqrt();
    let steps = (dist * 1.5).ceil() as u32 + 1;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        stamp_circle(buf, w, h, p0.0 + dx * t, p0.1 + dy * t, r, color);
    }
}

fn bind_annotations(ui: &AppWindow, annot: Rc<RefCell<AnnotationState>>) {
    // set-tool: "pen" or "marker"; clicking the active tool toggles draw-mode off
    let ui_h = ui.as_weak();
    let a1 = annot.clone();
    ui.on_set_tool(move |tool| {
        let Some(ui) = ui_h.upgrade() else { return };
        let same = ui.get_draw_mode() && ui.get_active_tool().as_str() == tool.as_str();
        if same {
            ui.set_draw_mode(false);
        } else {
            ui.set_active_tool(tool.clone());
            ui.set_draw_mode(true);
            let mut st = a1.borrow_mut();
            st.is_marker = tool.as_str() == "marker";
            st.pen_radius = if st.is_marker { 12.0 } else { 3.5 };
            st.pen_color[3] = if st.is_marker { 255 } else { 220 };
        }
    });

    let ui_h = ui.as_weak();
    let a2 = annot.clone();
    ui.on_pick_color(move |color| {
        let mut st = a2.borrow_mut();
        let alpha = if st.is_marker { 255 } else { 220 };
        st.pen_color = [color.red(), color.green(), color.blue(), alpha];
        if let Some(ui) = ui_h.upgrade() {
            ui.set_pen_color(color);
        }
    });

    let ui_h = ui.as_weak();
    let a3 = annot.clone();
    ui.on_draw_start(move |x, y| {
        let Some(ui) = ui_h.upgrade() else { return };
        let w = ui.get_canvas_w_px() as u32;
        let h = ui.get_canvas_h_px() as u32;
        if w == 0 || h == 0 { return; }
        let mut st = a3.borrow_mut();
        st.ensure_sized(w, h);
        if st.is_marker {
            st.stroke_buf.fill(0);
            let c = st.pen_color;
            let r = st.pen_radius;
            stamp_circle(&mut st.stroke_buf, w, h, x, y, r, c);
            let base = st.base_buf.clone();
            st.composite.copy_from_slice(&base);
            let ma = st.marker_alpha;
            let sb = st.stroke_buf.clone();
            blend_layer(&mut st.composite, &sb, ma);
        } else {
            let base = st.base_buf.clone();
            st.composite.copy_from_slice(&base);
            let r = st.pen_radius;
            let c = st.pen_color;
            stamp_circle(&mut st.composite, w, h, x, y, r, c);
        }
        st.last_pt = Some((x, y));
        drop(st);
        let img = a3.borrow().to_image();
        ui.set_annotation_layer(img);
    });

    let ui_h = ui.as_weak();
    let a4 = annot.clone();
    ui.on_draw_move(move |x, y| {
        let Some(ui) = ui_h.upgrade() else { return };
        let w = ui.get_canvas_w_px() as u32;
        let h = ui.get_canvas_h_px() as u32;
        let prev = {
            let st = a4.borrow();
            if w == 0 || h == 0 || st.last_pt.is_none() { return; }
            st.last_pt.unwrap()
        };
        {
            let mut st = a4.borrow_mut();
            let r = st.pen_radius;
            let c = st.pen_color;
            if st.is_marker {
                draw_segment(&mut st.stroke_buf, w, h, prev, (x, y), r, c);
                let base = st.base_buf.clone();
                st.composite.copy_from_slice(&base);
                let ma = st.marker_alpha;
                let sb = st.stroke_buf.clone();
                blend_layer(&mut st.composite, &sb, ma);
            } else {
                draw_segment(&mut st.composite, w, h, prev, (x, y), r, c);
            }
            st.last_pt = Some((x, y));
        }
        let img = a4.borrow().to_image();
        ui.set_annotation_layer(img);
    });

    let a5 = annot.clone();
    ui.on_draw_end(move || {
        let mut st = a5.borrow_mut();
        if st.is_marker {
            // Commit the marker stroke onto base at reduced opacity
            let ma = st.marker_alpha;
            let sb = st.stroke_buf.clone();
            blend_layer(&mut st.base_buf, &sb, ma);
            st.stroke_buf.fill(0);
            let base = st.base_buf.clone();
            st.composite.copy_from_slice(&base);
        } else {
            let comp = st.composite.clone();
            st.base_buf.copy_from_slice(&comp);
        }
        st.last_pt = None;
    });

    let ui_h = ui.as_weak();
    let a6 = annot.clone();
    ui.on_clear_annotations(move || {
        let mut st = a6.borrow_mut();
        st.base_buf.fill(0);
        st.composite.fill(0);
        st.stroke_buf.fill(0);
        st.last_pt = None;
        if let Some(ui) = ui_h.upgrade() {
            ui.set_annotation_layer(Image::default());
        }
    });
}

fn bind_annotations_noop(ui: &AppWindow) {
    ui.on_set_tool(|_| {});
    ui.on_pick_color(|_| {});
    ui.on_draw_start(|_, _| {});
    ui.on_draw_move(|_, _| {});
    ui.on_draw_end(|| {});
    ui.on_clear_annotations(|| {});
}

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
    // Dock/menu launches pass no argument (there's no file to open yet), so
    // show an empty-state window instead of erroring out before any window
    // exists — a dock click must always produce a visible window.
    let Some(path) = std::env::args().nth(1).map(PathBuf::from) else {
        let ui = AppWindow::new()?;
        ui.set_mode("error".into());
        ui.set_status("Bir dosya açmak için Altay dosya yöneticisini kullanın.".into());
        ui.run()?;
        return Ok(());
    };
    let path = path.canonicalize().unwrap_or(path);

    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let ui = AppWindow::new()?;
    ui.set_filename(filename.clone().into());

    let Some(kind) = detect(&path) else {
        ui.set_mode("error".into());
        ui.set_status("Desteklenmeyen dosya türü".into());
        ui.run()?;
        return Ok(());
    };
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
    bind_annotations(&ui, Rc::new(RefCell::new(AnnotationState::new())));

    let siblings = image_siblings(&path);
    let start_idx = siblings.iter().position(|p| p == &path).unwrap_or(0);

    let idx = Rc::new(Cell::new(start_idx));
    let zoom = Rc::new(Cell::new(1.0f32));
    let siblings = Rc::new(siblings);

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
            ui.set_pan_x(0.0);
            ui.set_pan_y(0.0);
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
    ui.on_fit(move || {
        if let Some(ui) = ui_h.upgrade() {
            z.set(1.0); ui.set_zoom(1.0);
            ui.set_pan_x(0.0); ui.set_pan_y(0.0);
        }
    });

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
    bind_annotations(&ui, Rc::new(RefCell::new(AnnotationState::new())));

    let doc = mupdf::Document::open(path.to_str().context("geçersiz yol")?)
        .context("PDF açılamadı")?;
    let page_count = doc.page_count()?;

    ui.set_page_count(page_count);
    ui.set_has_prev(false);
    ui.set_has_next(page_count > 1);
    ui.set_mode("pdf".into());

    let doc  = Rc::new(doc);
    let pidx = Rc::new(Cell::new(0i32));
    let zoom = Rc::new(Cell::new(1.0f32));

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

    let pi = pidx.clone(); let r = refresh.clone(); let ui_h = ui.as_weak();
    ui.on_prev_action(move || {
        let i = pi.get(); if i > 0 { pi.set(i - 1); r();
            if let Some(ui) = ui_h.upgrade() { ui.set_pan_x(0.0); ui.set_pan_y(0.0); }
        }
    });

    let pi = pidx.clone(); let pc = page_count; let r = refresh.clone(); let ui_h = ui.as_weak();
    ui.on_next_action(move || {
        let i = pi.get(); if i + 1 < pc { pi.set(i + 1); r();
            if let Some(ui) = ui_h.upgrade() { ui.set_pan_x(0.0); ui.set_pan_y(0.0); }
        }
    });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_zoom_in(move || { zi.set((zi.get() * 1.25).min(5.0)); r(); });

    let zi = zoom.clone(); let r = refresh.clone();
    ui.on_zoom_out(move || { zi.set((zi.get() / 1.25).max(0.2)); r(); });

    let zi = zoom.clone(); let r = refresh.clone(); let ui_h = ui.as_weak();
    ui.on_fit(move || {
        zi.set(1.0); r();
        if let Some(ui) = ui_h.upgrade() { ui.set_pan_x(0.0); ui.set_pan_y(0.0); }
    });

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
    bind_annotations_noop(&ui);

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

    let book = Rc::new(RefCell::new(book));
    let cidx = Rc::new(Cell::new(0usize));

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
