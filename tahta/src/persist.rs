//! Tahta's own board file, so a lesson survives closing the app, a crash or
//! a power cut: `app.rs` autosaves the board a moment after every change
//! and restores it on the next start.
//!
//! A save is a folder (under `~/.local/share/tahta/tahtalar/`): `tahta.bin` (zlib-compressed, little-endian binary:
//! pages, backgrounds, strokes) plus one `sayfa-<key>.z` per imported PDF
//! page image. Images are written once and only referenced afterwards, so
//! the frequent autosave doesn't rewrite megabytes of scans each time.
//! Every file goes through a temp name + rename: a write cut short leaves
//! the previous save intact. Laser-pointer marks aren't saved (they fade
//! anyway); erased-to-nothing strokes are dropped.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use glam::Vec2;

use crate::board::{BoardBackground, GridPattern, PdfImage};
use crate::brush::BrushType;
use crate::stroke::Stroke;

const MAGIC: &[u8; 8] = b"TAHTA\0\0\x01";
const BOARD_FILE: &str = "tahta.bin";

/// One page as stored: what's behind the ink, and the ink.
pub struct PageData {
    pub background: BoardBackground,
    pub grid: GridPattern,
    pub pdf_image: Option<Arc<PdfImage>>,
    pub strokes: Vec<Arc<Stroke>>,
}

pub struct BoardData {
    pub current_page: usize,
    pub pages: Vec<PageData>,
}

/// `$XDG_DATA_HOME/tahta` (default `~/.local/share/tahta`).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("tahta")
}

/// Board saves live in their own subfolder: the browser engine Tahta once
/// embedded kept its storage directly in `data_dir()`.
fn boards_dir() -> PathBuf {
    data_dir().join("tahtalar")
}

/// The autosave of the board in use.
pub fn autosave_dir() -> PathBuf {
    boards_dir().join("son")
}

/// The board "New board" replaced — one level of safety net.
pub fn backup_dir() -> PathBuf {
    boards_dir().join("onceki")
}

pub fn save(dir: &Path, board: &BoardData) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("{} oluşturulamadı", dir.display()))?;
    let mut keep = vec![BOARD_FILE.to_string()];
    for page in &board.pages {
        if let Some(img) = &page.pdf_image {
            let name = image_file(img.key);
            if !dir.join(&name).exists() {
                let mut raw = Vec::with_capacity(8 + img.rgba.len());
                raw.extend_from_slice(&img.width.to_le_bytes());
                raw.extend_from_slice(&img.height.to_le_bytes());
                raw.extend_from_slice(&img.rgba);
                write_atomic(&dir.join(&name), &zlib(&raw)?)?;
            }
            keep.push(name);
        }
    }
    write_atomic(&dir.join(BOARD_FILE), &zlib(&encode(board))?)?;
    // Page images no longer referenced (pages replaced, another board).
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("sayfa-") && name.ends_with(".z") && !keep.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// The board saved in `dir`; `None` if there's none.
pub fn load(dir: &Path) -> anyhow::Result<Option<BoardData>> {
    let path = dir.join(BOARD_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("{} okunamadı", path.display())),
    };
    let raw = unzlib(&bytes).context("tahta.bin açılamadı")?;
    decode(&raw, |key| {
        let raw = unzlib(&std::fs::read(dir.join(image_file(key)))?)?;
        let mut r = Reader { buf: &raw, pos: 0 };
        let (w, h) = (r.u32()?, r.u32()?);
        let rgba = r.take(w as usize * h as usize * 4)?.to_vec();
        Ok(Arc::new(PdfImage::restored(key, w, h, rgba)))
    })
    .map(Some)
}

fn image_file(key: u64) -> String {
    format!("sayfa-{key:016x}.z")
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    // Unique per write: the autosave thread and a save on close may race.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!("tmp{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let res = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res.with_context(|| format!("{} yazılamadı", path.display()))
}

fn zlib(raw: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(raw)?;
    enc.finish()
}

fn unzlib(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    ZlibDecoder::new(bytes).read_to_end(&mut out)?;
    Ok(out)
}

fn background_code(b: BoardBackground) -> u8 {
    match b {
        BoardBackground::White => 0,
        BoardBackground::Black => 1,
        BoardBackground::DarkGray => 2,
        BoardBackground::Green => 3,
    }
}

fn background_from(c: u8) -> BoardBackground {
    match c {
        0 => BoardBackground::White,
        2 => BoardBackground::DarkGray,
        3 => BoardBackground::Green,
        _ => BoardBackground::Black,
    }
}

fn grid_code(g: GridPattern) -> u8 {
    match g {
        GridPattern::Plain => 0,
        GridPattern::Lined => 1,
        GridPattern::Checkered => 2,
    }
}

fn grid_from(c: u8) -> GridPattern {
    match c {
        1 => GridPattern::Lined,
        2 => GridPattern::Checkered,
        _ => GridPattern::Plain,
    }
}

fn brush_code(b: BrushType) -> u8 {
    match b {
        BrushType::Ballpoint => 0,
        BrushType::Calligraphy => 1,
        BrushType::Highlighter => 2,
        BrushType::LaserPointer => 3,
    }
}

fn brush_from(c: u8) -> BrushType {
    match c {
        1 => BrushType::Calligraphy,
        2 => BrushType::Highlighter,
        _ => BrushType::Ballpoint,
    }
}

fn kept(s: &Stroke) -> bool {
    !s.points.is_empty() && s.brush_type != BrushType::LaserPointer
}

fn encode(board: &BoardData) -> Vec<u8> {
    let mut w = Vec::new();
    w.extend_from_slice(MAGIC);
    w.extend_from_slice(&(board.current_page as u32).to_le_bytes());
    w.extend_from_slice(&(board.pages.len() as u32).to_le_bytes());
    for page in &board.pages {
        w.push(background_code(page.background));
        w.push(grid_code(page.grid));
        match &page.pdf_image {
            Some(img) => {
                w.push(1);
                w.extend_from_slice(&img.key.to_le_bytes());
            }
            None => w.push(0),
        }
        let strokes: Vec<&Stroke> = page.strokes.iter().map(|s| s.as_ref()).filter(|s| kept(s)).collect();
        w.extend_from_slice(&(strokes.len() as u32).to_le_bytes());
        for s in strokes {
            w.push(brush_code(s.brush_type));
            for c in s.color {
                w.extend_from_slice(&c.to_le_bytes());
            }
            w.extend_from_slice(&s.width.to_le_bytes());
            w.extend_from_slice(&(s.points.len() as u32).to_le_bytes());
            for (p, t) in s.points.iter().zip(&s.timestamps) {
                w.extend_from_slice(&p.x.to_le_bytes());
                w.extend_from_slice(&p.y.to_le_bytes());
                w.extend_from_slice(&t.to_le_bytes());
            }
        }
    }
    w
}

fn decode(
    raw: &[u8],
    mut image: impl FnMut(u64) -> anyhow::Result<Arc<PdfImage>>,
) -> anyhow::Result<BoardData> {
    let mut r = Reader { buf: raw, pos: 0 };
    if r.take(MAGIC.len())? != MAGIC {
        bail!("tahta dosyası değil ya da desteklenmeyen sürüm");
    }
    let current_page = r.u32()? as usize;
    let n_pages = r.u32()?;
    let mut pages = Vec::new();
    for _ in 0..n_pages {
        let background = background_from(r.u8()?);
        let grid = grid_from(r.u8()?);
        let pdf_image = match r.u8()? {
            1 => {
                let key = r.u64()?;
                // A missing scan shouldn't cost the ink drawn on it.
                match image(key) {
                    Ok(img) => Some(img),
                    Err(e) => {
                        log::warn!("sayfa görüntüsü yüklenemedi ({key:016x}): {e:#}");
                        None
                    }
                }
            }
            _ => None,
        };
        let n_strokes = r.u32()?;
        let mut strokes = Vec::new();
        for _ in 0..n_strokes {
            let brush = brush_from(r.u8()?);
            let color = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
            let width = r.f32()?;
            let n = r.u32()? as usize;
            let mut s = Stroke::new(color, width, brush);
            s.points.reserve(n);
            s.timestamps.reserve(n);
            for _ in 0..n {
                s.points.push(Vec2::new(r.f32()?, r.f32()?));
                s.timestamps.push(r.f64()?);
            }
            strokes.push(Arc::new(s));
        }
        pages.push(PageData { background, grid, pdf_image, strokes });
    }
    Ok(BoardData { current_page, pages })
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len()).context("dosya beklenenden kısa")?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }
    fn u8(&mut self) -> anyhow::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> anyhow::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> anyhow::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn f32(&mut self) -> anyhow::Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn f64(&mut self) -> anyhow::Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(brush: BrushType, pts: &[(f32, f32)]) -> Arc<Stroke> {
        let mut s = Stroke::new([0.1, 0.2, 0.3, 1.0], 4.0, brush);
        for (i, &(x, y)) in pts.iter().enumerate() {
            s.push_point(Vec2::new(x, y), i as f64 * 0.01);
        }
        Arc::new(s)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tahta-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn board_round_trips_without_laser_or_erased_strokes() {
        let dir = temp_dir("roundtrip");
        let img = Arc::new(PdfImage::new(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]));
        let board = BoardData {
            current_page: 1,
            pages: vec![
                PageData {
                    background: BoardBackground::Green,
                    grid: GridPattern::Lined,
                    pdf_image: None,
                    strokes: vec![
                        stroke(BrushType::Calligraphy, &[(0.0, 0.0), (10.0, 5.0), (20.0, 0.0)]),
                        stroke(BrushType::LaserPointer, &[(1.0, 1.0), (9.0, 9.0)]),
                        stroke(BrushType::Ballpoint, &[]),
                    ],
                },
                PageData {
                    background: BoardBackground::White,
                    grid: GridPattern::Plain,
                    pdf_image: Some(img.clone()),
                    strokes: vec![stroke(BrushType::Highlighter, &[(3.0, 4.0), (30.0, 40.0)])],
                },
            ],
        };
        save(&dir, &board).unwrap();
        let back = load(&dir).unwrap().unwrap();
        assert_eq!(back.current_page, 1);
        assert_eq!(back.pages.len(), 2);
        let p0 = &back.pages[0];
        assert_eq!((p0.background, p0.grid), (BoardBackground::Green, GridPattern::Lined));
        assert_eq!(p0.strokes.len(), 1, "laser + empty strokes are not saved");
        let s = &p0.strokes[0];
        assert_eq!(s.brush_type, BrushType::Calligraphy);
        assert_eq!(s.points, vec![Vec2::new(0.0, 0.0), Vec2::new(10.0, 5.0), Vec2::new(20.0, 0.0)]);
        assert_eq!(s.timestamps, vec![0.0, 0.01, 0.02]);
        assert_eq!((s.color, s.width), ([0.1, 0.2, 0.3, 1.0], 4.0));
        let back_img = back.pages[1].pdf_image.as_ref().unwrap();
        assert_eq!((back_img.key, back_img.width, back_img.height), (img.key, 2, 1));
        assert_eq!(back_img.rgba, img.rgba);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreferenced_page_images_are_cleaned_up() {
        let dir = temp_dir("cleanup");
        let page = |img: Option<Arc<PdfImage>>| PageData {
            background: BoardBackground::White,
            grid: GridPattern::Plain,
            pdf_image: img,
            strokes: Vec::new(),
        };
        let img = Arc::new(PdfImage::new(1, 1, vec![9, 9, 9, 9]));
        save(&dir, &BoardData { current_page: 0, pages: vec![page(Some(img.clone()))] }).unwrap();
        assert!(dir.join(image_file(img.key)).exists());
        save(&dir, &BoardData { current_page: 0, pages: vec![page(None)] }).unwrap();
        assert!(!dir.join(image_file(img.key)).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_corrupt_saves_are_reported_not_fatal() {
        let dir = temp_dir("corrupt");
        assert!(load(&dir).unwrap().is_none(), "no save yet");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(BOARD_FILE), zlib(b"TAHTA\0\0\x01\x00").unwrap()).unwrap();
        assert!(load(&dir).is_err(), "truncated file");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
