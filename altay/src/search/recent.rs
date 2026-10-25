// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! The "Recent" view: files under home touched lately, newest first.
//!
//! Two sources are merged. Modification times (from the live index, or a walk
//! when the index isn't ready) cover everything the user edits, whichever app
//! did it. The freedesktop `recently-used.xbel` adds files that were only
//! opened (LibreOffice, Firefox, GTK apps write it; BacakOS apps don't).
//! Hidden directories and build output are left out — a single `cargo build`
//! would otherwise bury every document.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::filesystem::Entry;
use crate::security::Sandbox;

use super::index::should_watch;
use super::{entry_from_path, Index};

/// How far back the view reaches.
pub const WINDOW: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// At most this many files are shown.
pub const LIMIT: usize = 200;

/// Recently modified or opened files under `home`, newest first.
pub fn recent_files(sandbox: &Sandbox, home: &Path, index: Option<&Index>) -> Vec<Entry> {
    let since = SystemTime::now().checked_sub(WINDOW).unwrap_or(SystemTime::UNIX_EPOCH);
    let modified = match index {
        Some(idx) if idx.covers(home) && idx.is_ready() => idx.modified_since(since),
        _ => walk_modified_since(home, since),
    };
    let xbel = std::fs::read_to_string(home.join(".local/share/recently-used.xbel"))
        .map(|text| parse_xbel(&text))
        .unwrap_or_default();
    merge(sandbox, home, since, modified, xbel)
}

fn walk_modified_since(home: &Path, since: SystemTime) -> Vec<Entry> {
    walkdir::WalkDir::new(home)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || should_watch(home, e.path()))
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|m| m >= since))
        .filter_map(|e| entry_from_path(e.path()))
        .collect()
}

/// Combine both sources: one row per file, ranked by the later of its mtime
/// and its last use, keeping only the user's own files inside the sandbox.
fn merge(
    sandbox: &Sandbox,
    home: &Path,
    since: SystemTime,
    modified: Vec<Entry>,
    used: Vec<(PathBuf, SystemTime)>,
) -> Vec<Entry> {
    let mut rank: HashMap<PathBuf, (SystemTime, Entry)> = HashMap::new();
    for e in modified {
        if let Some(m) = e.modified {
            rank.insert(e.path.clone(), (m, e));
        }
    }
    for (path, when) in used {
        if when < since {
            continue;
        }
        if let Some((t, _)) = rank.get_mut(&path) {
            *t = (*t).max(when);
        } else if let Some(e) = entry_from_path(&path).filter(|e| !e.is_dir) {
            rank.insert(path, (when, e));
        }
    }
    let mut rows: Vec<(SystemTime, Entry)> = rank
        .into_values()
        .filter(|(_, e)| should_watch(home, &e.path) && sandbox.resolve(&e.path).is_ok())
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.truncate(LIMIT);
    rows.into_iter().map(|(_, e)| e).collect()
}

/// `(path, last use)` for every local file bookmark in a
/// `recently-used.xbel` document. Last use is the latest of the bookmark's
/// `added`/`modified`/`visited` stamps.
fn parse_xbel(text: &str) -> Vec<(PathBuf, SystemTime)> {
    let mut out = Vec::new();
    for tag in text.split("<bookmark ").skip(1) {
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        let Some(path) = attr(tag, "href").and_then(|h| h.strip_prefix("file://").and_then(percent_decode)) else {
            continue;
        };
        let when = ["added", "modified", "visited"]
            .iter()
            .filter_map(|k| attr(tag, k))
            .filter_map(|v| chrono::DateTime::parse_from_rfc3339(&v).ok())
            .map(SystemTime::from)
            .max();
        if let Some(when) = when {
            out.push((PathBuf::from(path), when));
        }
    }
    out
}

/// The value of `name="…"` in a tag's attribute text, XML entities decoded.
fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let mut from = 0;
    while let Some(pos) = tag[from..].find(&key) {
        let at = from + pos;
        // A whole attribute name, not the tail of a longer one.
        if at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace() {
            let rest = &tag[at + key.len()..];
            return Some(xml_unescape(&rest[..rest.find('"')?]));
        }
        from = at + key.len();
    }
    None
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Decode `%XX` escapes of a file URI path (UTF-8). `None` if malformed.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn xbel_parsing() {
        let doc = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0">
  <bookmark href="file:///home/u/Belgeler/Rapor%20%C3%A7%C4%B1kt%C4%B1.pdf" added="2026-10-01T10:00:00Z" modified="2026-10-02T10:00:00Z" visited="2026-10-03T10:00:00.5Z">
    <info/>
  </bookmark>
  <bookmark href="https://example.org/x" added="2026-10-01T10:00:00Z"/>
  <bookmark href="file:///home/u/a&amp;b.txt" xmodified="2099-01-01T00:00:00Z" modified="2026-09-01T00:00:00+03:00"/>
</xbel>"#;
        let got = parse_xbel(doc);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].0, PathBuf::from("/home/u/Belgeler/Rapor çıktı.pdf"));
        let visited = SystemTime::from(chrono::DateTime::parse_from_rfc3339("2026-10-03T10:00:00.5Z").unwrap());
        assert_eq!(got[0].1, visited, "latest of added/modified/visited");
        assert_eq!(got[1].0, PathBuf::from("/home/u/a&b.txt"));
        // "xmodified" is not "modified".
        let modified = SystemTime::from(chrono::DateTime::parse_from_rfc3339("2026-09-01T00:00:00+03:00").unwrap());
        assert_eq!(got[1].1, modified);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/%C3%BC").as_deref(), Some("/ü"));
        assert_eq!(percent_decode("/bad%2"), None);
        assert_eq!(percent_decode("/bad%zz"), None);
    }

    #[test]
    fn merge_ranks_and_filters() {
        let home = std::env::temp_dir().join(format!("altay-recent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for d in ["Belgeler", "proje/target/release", ".cache"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let home = std::fs::canonicalize(&home).unwrap();
        let doc = home.join("Belgeler/rapor.odt");
        let opened = home.join("Belgeler/okunan.pdf");
        let build = home.join("proje/target/release/app");
        let cache = home.join(".cache/x");
        for f in [&doc, &opened, &build, &cache] {
            std::fs::write(f, b"x").unwrap();
        }
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(home.clone())]);
        let now = SystemTime::now();
        let since = now - WINDOW;
        let modified: Vec<Entry> = [&doc, &build, &cache].iter().filter_map(|p| entry_from_path(p)).collect();
        // Opened after the edit, so it ranks first; one stale use is dropped.
        let used = vec![(opened.clone(), now + Duration::from_secs(60)), (doc.clone(), since - Duration::from_secs(1))];
        let names: Vec<String> = merge(&sb, &home, since, modified, used).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["okunan.pdf", "rapor.odt"]);
        let _ = std::fs::remove_dir_all(&home);
    }
}
