use anyhow::{Context, Result};
use image::DynamicImage;
use std::path::{Path, PathBuf};
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

slint::include_modules!();

fn load_slint_image(path: &Path) -> Option<Image> {
    let img = image::open(path).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(img.as_raw(), w, h);
    Some(Image::from_rgba8(buf))
}

fn image_files_in_dir(dir: &Path) -> Vec<PathBuf> {
    let exts = [
        "jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff", "tif",
        "ico", "qoi", "hdr", "pnm", "pbm", "pgm", "ppm",
    ];
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| exts.contains(&e.to_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("Kullanım: bacak-resim <resim-dosyası>")?;

    let path = path.canonicalize().unwrap_or(path);
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let siblings = image_files_in_dir(&dir);
    let current_idx = siblings
        .iter()
        .position(|p| p == &path)
        .unwrap_or(0);

    let ui = AppWindow::new()?;

    let update = {
        let ui_handle = ui.as_weak();
        let siblings = siblings.clone();
        move |idx: usize| {
            let Some(ui) = ui_handle.upgrade() else { return };
            let path = &siblings[idx];
            let img = load_slint_image(path).unwrap_or_default();
            let name = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let info = if let Ok(meta) = image::image_dimensions(path) {
                format!("{}×{}  ({}/{})", meta.0, meta.1, idx + 1, siblings.len())
            } else {
                format!("{}/{}", idx + 1, siblings.len())
            };
            ui.set_current_image(img);
            ui.set_filename(name.into());
            ui.set_info(info.into());
            ui.set_zoom(1.0);
            ui.set_has_prev(idx > 0);
            ui.set_has_next(idx + 1 < siblings.len());
        }
    };

    // Load first image
    update(current_idx);

    let idx = std::rc::Rc::new(std::cell::Cell::new(current_idx));

    let idx_c = idx.clone();
    let update_c = update.clone();
    ui.on_prev_image(move || {
        let i = idx_c.get();
        if i > 0 {
            idx_c.set(i - 1);
            update_c(i - 1);
        }
    });

    let idx_c = idx.clone();
    let update_c = update.clone();
    ui.on_next_image(move || {
        let i = idx_c.get();
        let siblings_len = siblings.len();
        if i + 1 < siblings_len {
            idx_c.set(i + 1);
            update_c(i + 1);
        }
    });

    let ui_handle = ui.as_weak();
    ui.on_zoom_in(move || {
        if let Some(ui) = ui_handle.upgrade() {
            let z = (ui.get_zoom() * 1.25).min(8.0);
            ui.set_zoom(z);
        }
    });

    let ui_handle = ui.as_weak();
    ui.on_zoom_out(move || {
        if let Some(ui) = ui_handle.upgrade() {
            let z = (ui.get_zoom() / 1.25).max(0.1);
            ui.set_zoom(z);
        }
    });

    let ui_handle = ui.as_weak();
    ui.on_fit_screen(move || {
        if let Some(ui) = ui_handle.upgrade() {
            ui.set_zoom(1.0);
        }
    });

    ui.run()?;
    Ok(())
}
