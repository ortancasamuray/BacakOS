use anyhow::{Context, Result};
use std::path::PathBuf;

slint::include_modules!();

/// Strip basic HTML tags, decode common entities, return plain text.
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
                if tag == "p" || tag == "/p" || tag == "br" || tag == "br/" || tag == "br /"
                    || tag.starts_with("h1") || tag.starts_with("h2") || tag.starts_with("h3")
                    || tag.starts_with("/h")
                {
                    out.push('\n');
                    if tag.starts_with("h") && !tag.starts_with("/h") {
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
                    "amp"  => '&',
                    "lt"   => '<',
                    "gt"   => '>',
                    "nbsp" => '\u{00A0}',
                    "quot" => '"',
                    "apos" => '\'',
                    _      => { out.push_str(&buf); out.push(';'); buf.clear(); continue; }
                });
                buf.clear();
            }
            _ if buf.starts_with('&') => { buf.push(ch); }
            _ => { out.push(ch); }
        }
    }

    // Collapse excessive blank lines
    let mut result = String::new();
    let mut blank = 0u32;
    for line in out.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            blank += 1;
            if blank <= 2 { result.push('\n'); }
        } else {
            blank = 0;
            result.push_str(trimmed);
            result.push('\n');
        }
    }
    result
}

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .context("Kullanım: bacak-epub <kitap.epub>")?;

    let mut book = epub::doc::EpubDoc::new(&path)
        .context("EPUB dosyası açılamadı")?;

    let title = book.mdata("title")
        .map(|m| m.value.clone())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Kitap")
                .to_string()
        });

    let num_chapters = book.get_num_chapters();

    // Collect chapter titles
    let chapter_titles: Vec<String> = (0..num_chapters)
        .map(|i| {
            book.set_current_chapter(i);
            book.get_current_id()
                .map(|id| format!("Bölüm {} — {}", i + 1, id))
                .unwrap_or_else(|| format!("Bölüm {}", i + 1))
        })
        .collect();

    let load_chapter = |book: &mut epub::doc::EpubDoc<std::io::BufReader<std::fs::File>>, idx: usize| -> String {
        book.set_current_chapter(idx);
        book.get_current_str()
            .map(|(html, _)| html_to_text(&html))
            .unwrap_or_else(|| "(Bu bölüm okunamadı)".to_string())
    };

    let ui = AppWindow::new()?;

    let slint_chapters: Vec<slint::SharedString> = chapter_titles
        .iter()
        .map(|s| s.as_str().into())
        .collect();

    ui.set_book_title(title.into());
    ui.set_chapters(slint::ModelRc::new(slint::VecModel::from(slint_chapters)));

    let book = std::rc::Rc::new(std::cell::RefCell::new(book));
    let idx  = std::rc::Rc::new(std::cell::Cell::new(0usize));

    let update = {
        let ui_h    = ui.as_weak();
        let book_rc = book.clone();
        let titles  = chapter_titles.clone();
        move |i: usize| {
            let Some(ui) = ui_h.upgrade() else { return };
            let text = load_chapter(&mut book_rc.borrow_mut(), i);
            let ctitle = titles.get(i).cloned().unwrap_or_default();
            ui.set_chapter_title(ctitle.into());
            ui.set_body_text(text.into());
            ui.set_chapter_index(i as i32);
        }
    };

    update(0);

    let idx_c = idx.clone();
    let upd_c = update.clone();
    ui.on_prev_chapter(move || {
        let i = idx_c.get();
        if i > 0 { idx_c.set(i - 1); upd_c(i - 1); }
    });

    let idx_c = idx.clone();
    let upd_c = update.clone();
    ui.on_next_chapter(move || {
        let i = idx_c.get();
        if i + 1 < num_chapters { idx_c.set(i + 1); upd_c(i + 1); }
    });

    let idx_c = idx.clone();
    let upd_c = update.clone();
    ui.on_select_chapter(move |i| {
        let i = i as usize;
        idx_c.set(i);
        upd_c(i);
    });

    let ui_h = ui.as_weak();
    ui.on_font_larger(move || {
        if let Some(ui) = ui_h.upgrade() {
            let sz = (ui.get_font_size() + 2).min(32);
            ui.set_font_size(sz);
        }
    });

    let ui_h = ui.as_weak();
    ui.on_font_smaller(move || {
        if let Some(ui) = ui_h.upgrade() {
            let sz = (ui.get_font_size() - 2).max(10);
            ui.set_font_size(sz);
        }
    });

    let ui_h = ui.as_weak();
    ui.on_toggle_dark(move || {
        if let Some(ui) = ui_h.upgrade() {
            ui.set_dark_mode(!ui.get_dark_mode());
        }
    });

    ui.run()?;
    Ok(())
}
