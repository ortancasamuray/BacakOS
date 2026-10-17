//! MIME types and file associations ("Open", "Open With").
//!
//! Detection and defaults go through the standard `xdg-mime` tool (so they
//! agree with every other desktop program and honour `mimeapps.list`), with
//! built-in fallbacks when it's missing or has no answer.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::masaustu::{self, Uygulama};

/// A file's MIME type: `xdg-mime query filetype` (content sniffing), else by
/// extension. Directories are `inode/directory`.
pub fn tur(dosya: &Path) -> Option<String> {
    if dosya.is_dir() {
        return Some("inode/directory".into());
    }
    if let Ok(out) = Command::new("xdg-mime").args(["query", "filetype"]).arg(dosya).stderr(Stdio::null()).output() {
        let m = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && m.contains('/') {
            return Some(m);
        }
    }
    uzantidan(dosya).map(String::from)
}

/// The default application id for `mime` (`xdg-mime query default`), else
/// the BacakOS built-in choice.
pub fn varsayilan(mime: &str) -> Option<String> {
    if let Ok(out) = Command::new("xdg-mime").args(["query", "default", mime]).stderr(Stdio::null()).output() {
        let id = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !id.is_empty() {
            return Some(id);
        }
    }
    yerlesik(mime).map(String::from)
}

/// Makes `uygulama_id` (e.g. `bacak-belge.desktop`) the default for `mime`.
pub fn varsayilan_yap(mime: &str, uygulama_id: &str) -> io::Result<()> {
    if mime.is_empty() || !mime.contains('/') || uygulama_id.is_empty() || uygulama_id.contains('/') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "geçersiz MIME türü ya da uygulama"));
    }
    let st = Command::new("xdg-mime").args(["default", uygulama_id, mime]).stdout(Stdio::null()).status()?;
    if st.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("xdg-mime default çıkış kodu {st}")))
    }
}

/// Candidates for "Open With": the default first, then applications that
/// declare `mime`, then every other application (many have incomplete
/// `MimeType=` lists but open the file fine), each group by name.
pub fn uygulamalar_icin(mime: &str) -> Vec<Uygulama> {
    let (mut matching, mut rest): (Vec<_>, Vec<_>) =
        masaustu::uygulamalar().into_iter().partition(|a| a.mime_turleri.iter().any(|m| m == mime));
    matching.append(&mut rest);
    if let Some(def) = varsayilan(mime) {
        if let Some(i) = matching.iter().position(|a| a.id == def) {
            let d = matching.remove(i);
            matching.insert(0, d);
        }
    }
    matching
}

/// Opens `dosya` in its default application, falling back to `xdg-open`.
pub fn ac(dosya: &Path) -> io::Result<()> {
    if let Some(app) = tur(dosya).and_then(|m| varsayilan(&m)).and_then(|id| masaustu::uygulama(&id)) {
        if app.baslat(&[dosya]).is_ok() {
            return Ok(());
        }
    }
    Command::new("xdg-open")
        .arg(dosya)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Opens `dosya` with the given application id.
pub fn birlikte_ac(dosya: &Path, uygulama_id: &str) -> io::Result<()> {
    let app = masaustu::uygulama(uygulama_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{uygulama_id} bulunamadı")))?;
    app.baslat(&[dosya]).map(|_| ())
}

/// BacakOS defaults when the user has none: documents and images in
/// bacak-belge.
fn yerlesik(mime: &str) -> Option<&'static str> {
    match mime {
        "application/pdf" | "application/epub+zip" => Some("bacak-belge.desktop"),
        m if m.starts_with("image/") => Some("bacak-belge.desktop"),
        "inode/directory" => Some("altay.desktop"),
        _ => None,
    }
}

/// Extension fallback for when `xdg-mime` is unavailable.
pub fn uzantidan(dosya: &Path) -> Option<&'static str> {
    let ext = dosya.extension()?.to_string_lossy().to_lowercase();
    Some(match ext.as_str() {
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "epub" => "application/epub+zip",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "ico" => "image/vnd.microsoft.icon",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/x-wav",
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "deb" => "application/vnd.debian.binary-package",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_fallback_and_builtins() {
        assert_eq!(uzantidan(Path::new("a.PDF")), Some("application/pdf"));
        assert_eq!(uzantidan(Path::new("a.bilinmeyen")), None);
        assert_eq!(uzantidan(Path::new("uzantisiz")), None);
        assert_eq!(yerlesik("image/webp"), Some("bacak-belge.desktop"));
        assert_eq!(yerlesik("text/plain"), None);
    }

    #[test]
    fn rejects_bad_defaults() {
        assert!(varsayilan_yap("bozuk", "a.desktop").is_err());
        assert!(varsayilan_yap("text/plain", "../x.desktop").is_err());
    }

    #[test]
    fn directories() {
        assert_eq!(tur(Path::new("/")).as_deref(), Some("inode/directory"));
    }
}
