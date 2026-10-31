//! Desktop integration: the web goes to the system browser, and what's
//! on screen behind the board can be pulled onto a page.
//!
//! Tahta used to embed a browser engine (Servo) — 100+ MB of the binary,
//! most of a GB of RAM once a page was open, and no audio/video at all, so
//! EBA lessons and videos never played. The real browser plays everything;
//! to write on what it shows, "Ekranı tahtaya al" asks the compositor
//! (BacakOS IPC `ekran_goruntusu`) for what's behind Tahta's own window —
//! just the web page when that's a browser — and adds it as a page.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;

use crate::board::PdfImage;

/// Turkey's national education platform — what a classroom board opens.
pub const BROWSER_HOME: &str = "https://www.eba.gov.tr";

/// Opens `url` in the user's default browser, without waiting for it.
pub fn open_browser(url: &str) -> anyhow::Result<()> {
    let mut child = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .spawn()
        .context("xdg-open çalıştırılamadı")?;
    // Reap it once it exits (xdg-open returns as soon as the browser starts).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// What's behind Tahta's window (a browser's page, else the window), as
/// one page image.
/// Blocking (an IPC round trip plus a PNG decode) — run off the UI thread.
pub fn capture_screen() -> anyhow::Result<Vec<Arc<PdfImage>>> {
    let mut ipc = bacak_sdk::Istemci::baglan().context("BacakOS compositor'üne bağlanılamadı")?;
    let windows = ipc.pencereler().context("pencereler alınamadı")?;
    let own = windows
        .iter()
        .filter(|w| w.uygulama == "tahta")
        .max_by_key(|w| w.odakli)
        .map(|w| w.id);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = runtime.join(format!("tahta-ekran-{}.png", std::process::id()));
    // Just the page when a browser is behind (no tabs/address bar), else
    // the window underneath.
    ipc.ekran_goruntusu(&path, own, bacak_sdk::Alan::Belge).context("ekran görüntüsü alınamadı")?;
    let image = decode_png(&path);
    let _ = std::fs::remove_file(&path);
    Ok(vec![Arc::new(image?)])
}

fn decode_png(path: &std::path::Path) -> anyhow::Result<PdfImage> {
    let file = std::fs::File::open(path).with_context(|| format!("{} açılamadı", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().context("PNG okunamadı")?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).context("PNG çözülemedi")?;
    let pixels = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => pixels.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => anyhow::bail!("beklenmeyen PNG renk türü"),
    };
    Ok(PdfImage::new(info.width, info.height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_png_becomes_a_page_image() {
        let path = std::env::temp_dir().join(format!("tahta-ekran-test-{}.png", std::process::id()));
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 2, 1);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(&[255, 0, 0, 0, 0, 255]).unwrap();
        }
        let img = decode_png(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, [255, 0, 0, 255, 0, 0, 255, 255]);
    }
}
