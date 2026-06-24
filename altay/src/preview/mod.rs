// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Preview & thumbnailing. Phase 1 classifies files into preview kinds and
//! provides text/image readiness; PDF (poppler), video (thumbnailer) and audio
//! waveform previews arrive with their backends in later phases. Reads are
//! sandbox-checked and size-capped to keep the UI responsive.

use std::path::Path;

use crate::filesystem::Entry;
use crate::security::{AccessDenied, Sandbox};

/// What kind of preview a file affords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    Image,
    Pdf,
    Text,
    Video,
    Audio,
    Archive,
    None,
}

impl PreviewKind {
    pub fn of(entry: &Entry) -> PreviewKind {
        if entry.is_dir {
            return PreviewKind::None;
        }
        match entry.extension.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tiff" | "ico" | "avif" => PreviewKind::Image,
            "pdf" => PreviewKind::Pdf,
            "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "c" | "h" | "cpp"
            | "py" | "js" | "ts" | "sh" | "log" | "csv" | "ini" | "conf" | "xml" | "html" => PreviewKind::Text,
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" => PreviewKind::Video,
            "mp3" | "flac" | "wav" | "ogg" | "m4a" | "opus" => PreviewKind::Audio,
            "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "zst" | "bz2" => PreviewKind::Archive,
            _ => PreviewKind::None,
        }
    }
}

/// Maximum bytes of a text file to load into the preview pane.
const TEXT_PREVIEW_CAP: u64 = 256 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("no preview available for this file type")]
    Unsupported,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Load a UTF-8 text preview (lossy), capped in size.
pub fn text_preview(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<String, PreviewError> {
    let safe = sandbox.resolve(path)?;
    let meta = std::fs::metadata(safe.as_path())?;
    let cap = meta.len().min(TEXT_PREVIEW_CAP) as usize;
    use std::io::Read;
    let mut buf = vec![0u8; cap];
    let mut f = std::fs::File::open(safe.as_path())?;
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Resolve an image file to a sandbox-checked path the UI can load directly.
pub fn image_path(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<std::path::PathBuf, PreviewError> {
    let safe = sandbox.resolve(path)?;
    Ok(safe.into_path_buf())
}

/// The content the preview pane should display for an entry.
#[derive(Debug, Clone)]
pub enum Content {
    /// A path to an image to display (original or a generated thumbnail).
    Image(std::path::PathBuf),
    /// Text to show in a monospace viewer.
    Text(String),
    /// Fallback: a short human description (type, size).
    Info(String),
}

/// The largest dimension (px) of a generated thumbnail.
const THUMB_SIZE: u32 = 512;

/// Decide what to show for an entry and produce it. Images larger than
/// [`THUMB_SIZE`] are downscaled into the freedesktop thumbnail cache so the UI
/// never decodes a giant original.
pub fn load(sandbox: &Sandbox, entry: &Entry) -> Result<Content, PreviewError> {
    match PreviewKind::of(entry) {
        PreviewKind::Image => {
            let safe = sandbox.resolve(&entry.path)?;
            let thumb = thumbnail(safe.as_path()).unwrap_or_else(|_| safe.as_path().to_path_buf());
            Ok(Content::Image(thumb))
        }
        PreviewKind::Pdf => {
            let safe = sandbox.resolve(&entry.path)?;
            match render_pdf_first_page(safe.as_path()) {
                Ok(png) => Ok(Content::Image(png)),
                // No renderer available / failed: fall back to a description.
                Err(_) => Ok(Content::Info(format!(
                    "PDF document\nSize: {}\n(install poppler-utils or ghostscript for a preview)",
                    humansize::format_size(entry.size, humansize::DECIMAL)
                ))),
            }
        }
        PreviewKind::Video => {
            let safe = sandbox.resolve(&entry.path)?;
            match render_video_frame(safe.as_path()) {
                Ok(png) => Ok(Content::Image(png)),
                Err(_) => Ok(Content::Info(format!(
                    "Video\nSize: {}\n(install ffmpegthumbnailer or ffmpeg for a preview)",
                    humansize::format_size(entry.size, humansize::DECIMAL)
                ))),
            }
        }
        PreviewKind::Text => Ok(Content::Text(text_preview(sandbox, &entry.path)?)),
        kind => {
            let perms = crate::permissions::read(sandbox, &entry.path)
                .map(|p| format!("\nPermissions: {} ({:o})", p.symbolic(), p.mode))
                .unwrap_or_default();
            if entry.is_dir {
                let total = crate::filesystem::recursive_size(&entry.path);
                let count = crate::filesystem::child_count(&entry.path);
                let size = humansize::format_size(total, humansize::DECIMAL);
                Ok(Content::Info(format!(
                    "Klasör\n{count} öğe\nToplam: {size}{perms}"
                )))
            } else {
                let size = humansize::format_size(entry.size, humansize::DECIMAL);
                let kind_label = format!("{:?}", kind);
                Ok(Content::Info(format!(
                    "{kind_label}\nBoyut: {size}{perms}"
                )))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally valid one-page (blank) PDF, with a correct
    /// cross-reference table so any renderer accepts it.
    fn minimal_pdf() -> Vec<u8> {
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 144 144] /Resources << >> >>",
        ];
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(b"%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, body) in objs.iter().enumerate() {
            offsets.push(buf.len());
            buf.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
        }
        let xref_off = buf.len();
        buf.extend_from_slice(format!("xref\n0 {}\n", objs.len() + 1).as_bytes());
        buf.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            buf.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        buf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objs.len() + 1,
                xref_off
            )
            .as_bytes(),
        );
        buf
    }

    fn a_renderer_exists() -> bool {
        ["pdftoppm", "pdftocairo", "gs"].iter().any(|t| {
            std::process::Command::new(t)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
    }

    #[test]
    fn pdf_first_page_renders_when_backend_present() {
        if !a_renderer_exists() {
            eprintln!("skipping: no PDF renderer (pdftoppm/pdftocairo/gs) on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("altay-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("blank.pdf");
        std::fs::write(&pdf, minimal_pdf()).unwrap();

        let png = render_pdf_first_page(&pdf).expect("PDF should render to a PNG");
        assert!(png.exists(), "rendered PNG missing");
        // It must be a decodable image.
        let img = image::open(&png).expect("rendered output is not a valid image");
        assert!(img.width() > 0 && img.height() > 0);

        // Second call reuses the cached render.
        let png2 = render_pdf_first_page(&pdf).unwrap();
        assert_eq!(png, png2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn cmd_ok(prog: &str, args: &[&str]) -> bool {
        std::process::Command::new(prog)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    #[test]
    fn video_frame_renders_when_ffmpeg_present() {
        if !cmd_ok("ffmpeg", &["-version"]) {
            eprintln!("skipping: ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("altay-vid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mp4 = dir.join("test.mp4");
        // Synthesize a short test clip with ffmpeg's lavfi source.
        let made = cmd_ok(
            "ffmpeg",
            &["-y", "-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=10",
              mp4.to_str().unwrap()],
        );
        assert!(made && mp4.exists(), "could not synthesize test video");

        let png = render_video_frame(&mp4).expect("video should render to a PNG");
        let img = image::open(&png).expect("rendered output is not a valid image");
        assert!(img.width() > 0 && img.height() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generates_and_reuses_thumbnail() {
        let dir = std::env::temp_dir().join(format!("altay-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("big.png");
        // A 1000x1000 image — larger than THUMB_SIZE.
        let img = image::RgbImage::from_fn(1000, 1000, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        img.save(&src).unwrap();

        let thumb = thumbnail(&src).unwrap();
        assert!(thumb.exists(), "thumbnail not written");
        let decoded = image::open(&thumb).unwrap();
        assert!(decoded.width() <= THUMB_SIZE && decoded.height() <= THUMB_SIZE);

        // Second call should reuse the cached file (same path).
        let thumb2 = thumbnail(&src).unwrap();
        assert_eq!(thumb, thumb2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Generate (or reuse) a cached thumbnail for an image, per the freedesktop
/// thumbnail spec: `$XDG_CACHE_HOME/thumbnails/normal/<md5(uri)>.png`. Returns
/// the cached path. Small images are returned as-is by the caller.
pub fn thumbnail(image_path: &Path) -> Result<std::path::PathBuf, PreviewError> {
    let uri = format!("file://{}", image_path.display());
    let digest = format!("{:x}", md5::compute(uri.as_bytes()));
    let cache_dir = dirs::cache_dir()
        .map(|c| c.join("thumbnails/normal"))
        .ok_or(PreviewError::Unsupported)?;
    let _ = std::fs::create_dir_all(&cache_dir);
    let thumb_path = cache_dir.join(format!("{digest}.png"));

    // Reuse a valid cached thumbnail (cache newer than the source).
    if let (Ok(tmeta), Ok(smeta)) = (std::fs::metadata(&thumb_path), std::fs::metadata(image_path)) {
        if let (Ok(tt), Ok(st)) = (tmeta.modified(), smeta.modified()) {
            if tt >= st {
                return Ok(thumb_path);
            }
        }
    }

    let img = image::open(image_path).map_err(|e| PreviewError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())))?;
    let thumb = img.thumbnail(THUMB_SIZE, THUMB_SIZE);
    thumb
        .save_with_format(&thumb_path, image::ImageFormat::Png)
        .map_err(|e| PreviewError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
    Ok(thumb_path)
}

/// Render the first page of a PDF to a cached PNG and return its path.
///
/// Prefers poppler's renderers (`pdftoppm`, then `pdftocairo`) and falls back to
/// Ghostscript (`gs`).
pub fn render_pdf_first_page(pdf: &Path) -> Result<std::path::PathBuf, PreviewError> {
    cached_render("pdf-preview", pdf, render_pdf_backends)
}

/// Render a representative frame of a video to a cached PNG and return its path.
///
/// Prefers `ffmpegthumbnailer` (the freedesktop video thumbnailer) and falls
/// back to `ffmpeg`. Requires one of them at runtime.
pub fn render_video_frame(video: &Path) -> Result<std::path::PathBuf, PreviewError> {
    cached_render("video-preview", video, render_video_backends)
}

/// Shared cache wrapper: compute the freedesktop thumbnail path keyed by
/// `scheme://source`, reuse a fresh cached PNG, otherwise invoke `render`.
fn cached_render<F>(scheme: &str, source: &Path, render: F) -> Result<std::path::PathBuf, PreviewError>
where
    F: FnOnce(&Path, &Path) -> bool,
{
    let uri = format!("{scheme}://{}", source.display());
    let digest = format!("{:x}", md5::compute(uri.as_bytes()));
    let cache_dir = dirs::cache_dir()
        .map(|c| c.join("thumbnails/normal"))
        .ok_or(PreviewError::Unsupported)?;
    let _ = std::fs::create_dir_all(&cache_dir);
    let out_png = cache_dir.join(format!("{digest}.png"));

    if let (Ok(tmeta), Ok(smeta)) = (std::fs::metadata(&out_png), std::fs::metadata(source)) {
        if let (Ok(tt), Ok(st)) = (tmeta.modified(), smeta.modified()) {
            if tt >= st {
                return Ok(out_png);
            }
        }
    }

    if render(source, &out_png) && out_png.exists() {
        Ok(out_png)
    } else {
        Err(PreviewError::Unsupported)
    }
}

/// Try each available video thumbnailer in preference order.
fn render_video_backends(video: &Path, out_png: &Path) -> bool {
    use std::process::{Command, Stdio};
    let quiet = |cmd: &mut Command| -> bool {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    // 1) ffmpegthumbnailer: seek to 20%, longest side ~512px, write PNG.
    if quiet(
        Command::new("ffmpegthumbnailer")
            .arg("-i").arg(video)
            .arg("-o").arg(out_png)
            .args(["-s", "512", "-t", "20%"]),
    ) && out_png.exists()
    {
        return true;
    }
    // 2) ffmpeg: grab one frame a few seconds in, scaled to 512px wide.
    quiet(
        Command::new("ffmpeg")
            .args(["-y", "-ss", "00:00:02", "-i"])
            .arg(video)
            .args(["-frames:v", "1", "-vf", "scale=512:-2"])
            .arg(out_png),
    ) && out_png.exists()
}

/// Try each available PDF renderer in preference order. Returns true on success.
fn render_pdf_backends(pdf: &Path, out_png: &Path) -> bool {
    use std::process::{Command, Stdio};

    // pdftoppm / pdftocairo write `<prefix>.png` with -singlefile, so strip the
    // extension to form the prefix they expect.
    let prefix = out_png.with_extension("");
    let prefix = prefix.as_os_str();

    let run = |cmd: &mut Command| -> bool {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    // 1) poppler: pdftoppm
    if run(Command::new("pdftoppm")
        .args(["-png", "-singlefile", "-r", "120", "-f", "1", "-l", "1"])
        .arg(pdf)
        .arg(prefix))
        && out_png.exists()
    {
        return true;
    }
    // 2) poppler: pdftocairo
    if run(Command::new("pdftocairo")
        .args(["-png", "-singlefile", "-r", "120"])
        .arg(pdf)
        .arg(prefix))
        && out_png.exists()
    {
        return true;
    }
    // 3) ghostscript fallback (writes the exact output path)
    run(Command::new("gs").args([
        "-dNOPAUSE",
        "-dBATCH",
        "-dSAFER",
        "-sDEVICE=png16m",
        "-dFirstPage=1",
        "-dLastPage=1",
        "-r120",
        &format!("-sOutputFile={}", out_png.display()),
    ])
    .arg(pdf))
        && out_png.exists()
}
