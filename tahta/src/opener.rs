//! "Aç" panel — opens a PDF, or brings back the previous board, from
//! inside the app. Before this, a PDF could only be opened by starting
//! tahta "with" it from the file manager, and the backup kept by "New
//! board" had no way back at all.
//!
//! No file dialog: a flat panel has no keyboard and a folder tree is
//! hard to drive by finger, so the panel just lists the PDFs found where
//! a teacher's files end up — Belgeler, İndirilenler, Masaüstü (and the
//! English-named folders), one subfolder deep, and USB sticks mounted
//! under `/media/<user>` — newest first, a page at a time.
//! Centered and modal like the radial menu: a touch inside picks a row
//! or button, a touch anywhere else closes it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use glam::Vec2;

use crate::font_atlas::{FontAtlas, GlyphVertex};
use crate::stroke::{push_line, push_rect, Vertex};
use crate::urlbar::fit_text;

const PANEL_W: f32 = 720.0;
const HEADER_H: f32 = 56.0;
const ROW_H: f32 = 64.0;
const FOOTER_H: f32 = 60.0;
const PADDING: f32 = 12.0;
const MARGIN_TOP: f32 = 40.0;
/// Keeps the panel clear of the toolbar at the bottom.
const MARGIN_BOTTOM: f32 = 180.0;
const MAX_ROWS: usize = 8;
/// A folder full of PDFs shouldn't make opening the panel slow.
const MAX_FILES: usize = 300;

const COLOR_PANEL_BG: [f32; 4] = [0.12, 0.13, 0.17, 0.96];
const COLOR_HEADER: [f32; 4] = [0.18, 0.19, 0.24, 1.0];
const COLOR_ROW_ALT: [f32; 4] = [1.0, 1.0, 1.0, 0.03];
const COLOR_BUTTON: [f32; 4] = [0.22, 0.24, 0.30, 1.0];
const COLOR_ACCENT: [f32; 4] = [0.23, 0.51, 0.96, 1.0];
const COLOR_PDF: [f32; 4] = [0.85, 0.32, 0.30, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_DETAIL: [f32; 4] = [0.95, 0.95, 0.97, 0.55];
const COLOR_DISABLED: [f32; 4] = [0.95, 0.95, 0.97, 0.25];

#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    /// The board kept by "New board" / opening a PDF.
    Backup,
    Pdf(PathBuf),
}

pub enum Press {
    None,
    Close,
    Choose(Choice),
}

struct Item {
    choice: Choice,
    title: String,
    detail: String,
}

pub struct Opener {
    items: Vec<Item>,
    page: usize,
}

impl Opener {
    /// Lists what can be opened right now (scans the folders once).
    pub fn new() -> Self {
        let mut items = Vec::new();
        let backup = crate::persist::backup_dir().join("tahta.bin");
        if let Ok(modified) = std::fs::metadata(&backup).and_then(|m| m.modified()) {
            items.push(Item {
                choice: Choice::Backup,
                title: "Önceki tahta".to_string(),
                detail: format!("Yedek · {}", format_time(modified)),
            });
        }
        for (path, folder, modified) in find_pdfs(&search_roots()) {
            let title = compose_turkish(&path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            items.push(Item { choice: Choice::Pdf(path), title, detail: format!("{folder} · {}", format_time(modified)) });
        }
        Self { items, page: 0 }
    }

    fn rows_per_page(screen: Vec2) -> usize {
        let room = screen.y - MARGIN_TOP - MARGIN_BOTTOM - HEADER_H - FOOTER_H;
        ((room / ROW_H).floor() as usize).clamp(1, MAX_ROWS)
    }

    fn page_count(&self, screen: Vec2) -> usize {
        self.items.len().div_ceil(Self::rows_per_page(screen)).max(1)
    }

    fn rect(&self, screen: Vec2) -> (Vec2, Vec2) {
        let w = PANEL_W.min(screen.x - 40.0);
        let rows = Self::rows_per_page(screen).min(self.items.len().max(1));
        let footer = if self.page_count(screen) > 1 { FOOTER_H } else { PADDING };
        let h = HEADER_H + rows as f32 * ROW_H + footer;
        (Vec2::new((screen.x - w) / 2.0, MARGIN_TOP), Vec2::new(w, h))
    }

    fn close_rect(&self, screen: Vec2) -> (Vec2, Vec2) {
        let (pos, size) = self.rect(screen);
        let s = HEADER_H - 12.0;
        (Vec2::new(pos.x + size.x - s - 6.0, pos.y + 6.0), Vec2::splat(s))
    }

    /// (previous, next) page buttons in the footer.
    fn page_button_rects(&self, screen: Vec2) -> [(Vec2, Vec2); 2] {
        let (pos, size) = self.rect(screen);
        let y = pos.y + size.y - FOOTER_H + 8.0;
        let s = Vec2::new(64.0, FOOTER_H - 16.0);
        [(Vec2::new(pos.x + PADDING, y), s), (Vec2::new(pos.x + size.x - PADDING - s.x, y), s)]
    }

    /// Row rects on the current page, with the index of their item.
    fn visible_rows(&self, screen: Vec2) -> Vec<((Vec2, Vec2), usize)> {
        let (pos, size) = self.rect(screen);
        let per_page = Self::rows_per_page(screen);
        let first = self.page * per_page;
        (first..self.items.len().min(first + per_page))
            .enumerate()
            .map(|(row, index)| ((Vec2::new(pos.x, pos.y + HEADER_H + row as f32 * ROW_H), Vec2::new(size.x, ROW_H)), index))
            .collect()
    }

    pub fn contains(&self, screen: Vec2, p: Vec2) -> bool {
        rect_contains(self.rect(screen), p)
    }

    pub fn press_at(&mut self, screen: Vec2, p: Vec2) -> Press {
        if rect_contains(self.close_rect(screen), p) {
            return Press::Close;
        }
        let pages = self.page_count(screen);
        let [prev, next] = self.page_button_rects(screen);
        if pages > 1 && rect_contains(prev, p) {
            self.page = self.page.saturating_sub(1);
            return Press::None;
        }
        if pages > 1 && rect_contains(next, p) {
            self.page = (self.page + 1).min(pages - 1);
            return Press::None;
        }
        for (rect, index) in self.visible_rows(screen) {
            if rect_contains(rect, p) {
                return Press::Choose(self.items[index].choice.clone());
            }
        }
        Press::None
    }

    pub fn render(
        &self,
        screen: Vec2,
        font: &FontAtlas,
        out_v: &mut Vec<Vertex>,
        out_i: &mut Vec<u32>,
        glyph_v: &mut Vec<GlyphVertex>,
        glyph_i: &mut Vec<u32>,
    ) {
        let (pos, size) = self.rect(screen);
        push_rect(pos, size, COLOR_PANEL_BG, out_v, out_i);
        push_rect(pos, Vec2::new(size.x, HEADER_H), COLOR_HEADER, out_v, out_i);
        font.push_text("Aç", pos + Vec2::new(PADDING + 4.0, (HEADER_H - 22.0) / 2.0), 22.0, COLOR_TEXT, glyph_v, glyph_i);

        let (close_pos, close_size) = self.close_rect(screen);
        push_rect(close_pos, close_size, COLOR_BUTTON, out_v, out_i);
        let c = close_pos + close_size / 2.0;
        let r = close_size.x * 0.22;
        push_line(c + Vec2::new(-r, -r), c + Vec2::new(r, r), 3.0, COLOR_TEXT, out_v, out_i);
        push_line(c + Vec2::new(-r, r), c + Vec2::new(r, -r), 3.0, COLOR_TEXT, out_v, out_i);

        let text_x = PADDING + 28.0;
        let text_w = size.x - text_x - PADDING;
        if self.items.is_empty() {
            let row = pos + Vec2::new(0.0, HEADER_H);
            font.push_text("PDF bulunamadı", row + Vec2::new(text_x, 12.0), 20.0, COLOR_TEXT, glyph_v, glyph_i);
            let where_ = fit_text("Belgeler, İndirilenler, Masaüstü ve USB bellekte aranır", text_w, false, |t| font.measure(t, 14.0));
            font.push_text(&where_, row + Vec2::new(text_x, 40.0), 14.0, COLOR_DETAIL, glyph_v, glyph_i);
        }
        for (row, ((row_pos, row_size), index)) in self.visible_rows(screen).into_iter().enumerate() {
            if row % 2 == 1 {
                push_rect(row_pos, row_size, COLOR_ROW_ALT, out_v, out_i);
            }
            let item = &self.items[index];
            let mark = if item.choice == Choice::Backup { COLOR_ACCENT } else { COLOR_PDF };
            push_rect(row_pos + Vec2::new(PADDING, 14.0), Vec2::new(6.0, ROW_H - 28.0), mark, out_v, out_i);
            let title = fit_text(&item.title, text_w, false, |t| font.measure(t, 20.0));
            font.push_text(&title, row_pos + Vec2::new(text_x, 10.0), 20.0, COLOR_TEXT, glyph_v, glyph_i);
            let detail = fit_text(&item.detail, text_w, false, |t| font.measure(t, 14.0));
            font.push_text(&detail, row_pos + Vec2::new(text_x, 40.0), 14.0, COLOR_DETAIL, glyph_v, glyph_i);
        }

        let pages = self.page_count(screen);
        if pages > 1 {
            let [prev, next] = self.page_button_rects(screen);
            for (i, (bpos, bsize)) in [prev, next].into_iter().enumerate() {
                push_rect(bpos, bsize, COLOR_BUTTON, out_v, out_i);
                let enabled = if i == 0 { self.page > 0 } else { self.page + 1 < pages };
                let color = if enabled { COLOR_TEXT } else { COLOR_DISABLED };
                let c = bpos + bsize / 2.0;
                let d = if i == 0 { -1.0 } else { 1.0 };
                push_line(c + Vec2::new(-d * 5.0, -10.0), c + Vec2::new(d * 5.0, 0.0), 4.0, color, out_v, out_i);
                push_line(c + Vec2::new(d * 5.0, 0.0), c + Vec2::new(-d * 5.0, 10.0), 4.0, color, out_v, out_i);
            }
            let label = format!("{} / {}", self.page + 1, pages);
            let w = font.measure(&label, 18.0);
            let y = pos.y + size.y - FOOTER_H / 2.0 - 9.0;
            font.push_text(&label, Vec2::new(pos.x + (size.x - w) / 2.0, y), 18.0, COLOR_DETAIL, glyph_v, glyph_i);
        }
    }
}

/// Folders to look in, with the name shown for each.
fn search_roots() -> Vec<(PathBuf, String)> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut roots: Vec<(PathBuf, String)> = ["Belgeler", "İndirilenler", "Masaüstü", "Documents", "Downloads", "Desktop"]
        .iter()
        .map(|name| (home.join(name), name.to_string()))
        .collect();
    // USB sticks: udisks mounts them at /media/<user>/<label>.
    if let Some(user) = std::env::var_os("USER") {
        for base in [Path::new("/media").join(&user), Path::new("/run/media").join(&user)] {
            if let Ok(entries) = std::fs::read_dir(&base) {
                for e in entries.flatten() {
                    roots.push((e.path(), format!("USB: {}", e.file_name().to_string_lossy())));
                }
            }
        }
    }
    roots
}

/// PDFs directly in each root or one subfolder down (hidden ones
/// skipped), newest first, each with the folder name to show. A file
/// reachable twice (a symlinked folder) is listed once.
fn find_pdfs(roots: &[(PathBuf, String)]) -> Vec<(PathBuf, String, SystemTime)> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut add_dir = |dir: &Path, label: &str, found: &mut Vec<(PathBuf, String, SystemTime)>, subdirs: &mut Vec<(PathBuf, String)>| {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.is_dir() {
                subdirs.push((path, format!("{label}/{name}")));
            } else if meta.is_file()
                && path.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf"))
                && found.len() < MAX_FILES
                && seen.insert(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()))
            {
                found.push((path, label.to_string(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
            }
        }
    };
    for (root, label) in roots {
        let mut subdirs = Vec::new();
        add_dir(root, label, &mut found, &mut subdirs);
        for (dir, sub_label) in subdirs {
            add_dir(&dir, &sub_label, &mut found, &mut Vec::new());
        }
    }
    found.sort_by(|a, b| b.2.cmp(&a.2));
    found
}

/// File names written on a Mac (or copied from one) keep Turkish letters
/// decomposed — "g" + combining breve — and the font atlas only has the
/// precomposed letters, so put those back together.
fn compose_turkish(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let composed = match (out.chars().last(), ch) {
            (Some('g'), '\u{306}') => Some('ğ'),
            (Some('G'), '\u{306}') => Some('Ğ'),
            (Some('s'), '\u{327}') => Some('ş'),
            (Some('S'), '\u{327}') => Some('Ş'),
            (Some('c'), '\u{327}') => Some('ç'),
            (Some('C'), '\u{327}') => Some('Ç'),
            (Some('o'), '\u{308}') => Some('ö'),
            (Some('O'), '\u{308}') => Some('Ö'),
            (Some('u'), '\u{308}') => Some('ü'),
            (Some('U'), '\u{308}') => Some('Ü'),
            (Some('I'), '\u{307}') => Some('İ'),
            _ => None,
        };
        match composed {
            Some(c) => {
                out.pop();
                out.push(c);
            }
            None => out.push(ch),
        }
    }
    out
}

/// "31.10.2026 14:05" in local time.
fn format_time(t: SystemTime) -> String {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` we hand it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return String::new();
    }
    format!("{:02}.{:02}.{} {:02}:{:02}", tm.tm_mday, tm.tm_mon + 1, tm.tm_year + 1900, tm.tm_hour, tm.tm_min)
}

fn rect_contains((top_left, size): (Vec2, Vec2), point: Vec2) -> bool {
    point.x >= top_left.x && point.x <= top_left.x + size.x && point.y >= top_left.y && point.y <= top_left.y + size.y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdfs_are_found_one_folder_deep_newest_first() {
        let root = std::env::temp_dir().join(format!("tahta-opener-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ders/derin")).unwrap();
        std::fs::create_dir_all(root.join(".gizli")).unwrap();
        for (path, age) in [("eski.pdf", 300), ("ders/yeni.PDF", 10), ("ders/derin/cok.pdf", 0), (".gizli/x.pdf", 0), ("not.txt", 0)] {
            let p = root.join(path);
            std::fs::write(&p, b"%PDF").unwrap();
            let t = SystemTime::now() - std::time::Duration::from_secs(age);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        let found = find_pdfs(&[(root.clone(), "Belgeler".to_string())]);
        let got: Vec<(String, String)> = found
            .iter()
            .map(|(p, label, _)| (p.file_name().unwrap().to_string_lossy().into_owned(), label.clone()))
            .collect();
        assert_eq!(got, [("yeni.PDF".to_string(), "Belgeler/ders".to_string()), ("eski.pdf".to_string(), "Belgeler".to_string())]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn decomposed_turkish_letters_are_put_together() {
        assert_eq!(compose_turkish("Hal Deg\u{306}is\u{327}im O\u{308}devi I\u{307}zmir"), "Hal Değişim Ödevi İzmir");
        assert_eq!(compose_turkish("Değişim"), "Değişim");
    }

    #[test]
    fn pages_split_the_list_and_stay_in_range() {
        let screen = Vec2::new(1920.0, 1080.0);
        let per_page = Opener::rows_per_page(screen);
        let mut opener = Opener {
            items: (0..per_page + 2)
                .map(|i| Item { choice: Choice::Pdf(PathBuf::from(format!("{i}.pdf"))), title: i.to_string(), detail: String::new() })
                .collect(),
            page: 0,
        };
        assert_eq!(opener.page_count(screen), 2);
        let [prev, next] = opener.page_button_rects(screen);
        let center = |(p, s): (Vec2, Vec2)| p + s / 2.0;
        opener.press_at(screen, center(next));
        opener.press_at(screen, center(next));
        assert_eq!(opener.page, 1);
        assert_eq!(opener.visible_rows(screen).len(), 2);
        let (row, _) = opener.visible_rows(screen)[1];
        assert!(matches!(opener.press_at(screen, center(row)), Press::Choose(Choice::Pdf(p)) if p == Path::new(&format!("{}.pdf", per_page + 1))));
        opener.press_at(screen, center(prev));
        assert_eq!(opener.page, 0);
        assert!(matches!(opener.press_at(screen, center(opener.close_rect(screen))), Press::Close));
    }
}
