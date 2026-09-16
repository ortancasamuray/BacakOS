// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! File search. Phase 1 ships a fast on-demand recursive walk with name/glob,
//! extension, size and date filters, constrained to a sandbox root. A
//! persistent inverted index (and optional content search) is a later phase.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::filesystem::Entry;
use crate::security::Sandbox;

mod index;

pub use index::Index;

/// Coarse file categories used by the filter chips in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Documents,
    Images,
    Videos,
    Audio,
    Archives,
    Applications,
}

impl Category {
    pub fn matches_ext(self, ext: &str) -> bool {
        let set: &[&str] = match self {
            Category::Documents => &["pdf", "doc", "docx", "odt", "txt", "md", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "csv"],
            Category::Images => &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "tiff", "heic", "avif", "ico"],
            Category::Videos => &["mp4", "mkv", "webm", "mov", "avi", "flv", "wmv", "m4v", "mpg", "mpeg"],
            Category::Audio => &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma"],
            Category::Archives => &["zip", "zipx", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "lzma", "cab", "ar", "cpio", "iso", "wim", "deb", "rpm"],
            Category::Applications => &["appimage", "desktop", "sh", "run", "bin", "jar"],
        };
        set.contains(&ext)
    }
}

/// A search request.
#[derive(Debug, Clone, Default)]
pub struct Query {
    /// Substring (case-insensitive) the file name must contain.
    pub name_contains: Option<String>,
    /// Substring (case-insensitive) the file's *contents* must contain. Only
    /// applied to text-like files under a size cap; expensive, so opt-in.
    pub content_contains: Option<String>,
    /// Restrict to a specific extension (without the dot).
    pub extension: Option<String>,
    /// Restrict to a coarse category.
    pub category: Option<Category>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<SystemTime>,
    /// Include hidden (dotfiles) in results.
    pub include_hidden: bool,
    /// Stop after this many hits (keeps the UI responsive).
    pub limit: usize,
}

impl Query {
    pub(crate) fn matches(&self, e: &Entry) -> bool {
        if let Some(sub) = &self.name_contains {
            if !e.name.to_lowercase().contains(&sub.to_lowercase()) {
                return false;
            }
        }
        if let Some(ext) = &self.extension {
            if e.extension != ext.to_lowercase() {
                return false;
            }
        }
        if let Some(cat) = self.category {
            if e.is_dir || !cat.matches_ext(&e.extension) {
                return false;
            }
        }
        if let Some(min) = self.min_size {
            if e.size < min {
                return false;
            }
        }
        if let Some(max) = self.max_size {
            if e.size > max {
                return false;
            }
        }
        if let Some(after) = self.modified_after {
            match e.modified {
                Some(m) if m >= after => {}
                _ => return false,
            }
        }
        true
    }
}

/// Walk `root` (which must be inside the sandbox) and return matching entries.
pub fn search(sandbox: &Sandbox, root: impl AsRef<Path>, query: &Query) -> Vec<Entry> {
    let Ok(safe) = sandbox.resolve(root) else {
        return Vec::new();
    };
    let limit = if query.limit == 0 { usize::MAX } else { query.limit };
    let mut hits = Vec::new();
    let walker = walkdir::WalkDir::new(safe.as_path())
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| query.include_hidden || !is_hidden(e.path()));
    for entry in walker.flatten() {
        if hits.len() >= limit {
            break;
        }
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = meta.is_dir();
        let extension = if is_dir {
            String::new()
        } else {
            path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
        };
        let candidate = Entry {
            path: path.to_path_buf(),
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
            is_symlink: meta.file_type().is_symlink(),
            size: if is_dir { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            extension,
        };
        if query.matches(&candidate) && content_ok(&candidate, query) {
            hits.push(candidate);
        }
    }
    hits
}

/// Apply the optional content filter: read text-like files (size-capped) and
/// test for the needle. Directories never match a content query.
fn content_ok(entry: &Entry, query: &Query) -> bool {
    let Some(needle) = &query.content_contains else {
        return true;
    };
    if entry.is_dir {
        return false;
    }
    file_contains(&entry.path, &needle.to_lowercase())
}

/// Largest file scanned for a content search.
const CONTENT_SCAN_CAP: u64 = 8 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn content_search_matches_inside_text_files() {
        let home = std::env::temp_dir().join(format!("altay-content-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("notes.txt"), b"the quick brown fox").unwrap();
        std::fs::write(home.join("other.txt"), b"nothing here").unwrap();
        std::fs::write(home.join("blob.bin"), [0u8, 1, 2, b'f', b'o', b'x']).unwrap(); // binary: skipped
        let canon = std::fs::canonicalize(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        let q = Query { content_contains: Some("BROWN".into()), limit: 100, ..Default::default() };
        let hits = search(&sb, &canon, &q);
        assert!(hits.iter().any(|e| e.name == "notes.txt"), "should find the match");
        assert!(!hits.iter().any(|e| e.name == "other.txt"));
        assert!(!hits.iter().any(|e| e.name == "blob.bin"), "binary files are skipped");
        let _ = std::fs::remove_dir_all(&home);
    }
}

fn file_contains(path: &Path, needle_lower: &str) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() > CONTENT_SCAN_CAP {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else { return false };
    // Heuristic: a NUL byte early on means "binary" — skip.
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return false;
    }
    String::from_utf8_lossy(&bytes).to_lowercase().contains(needle_lower)
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().starts_with('.'))
        .unwrap_or(false)
}

/// Build an [`Entry`] from a path using its own metadata. Returns `None` if the
/// path is gone. Shared by the live walk and the index.
pub(crate) fn entry_from_path(path: &Path) -> Option<Entry> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let is_symlink = meta.file_type().is_symlink();
    let is_dir = if is_symlink {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    } else {
        meta.is_dir()
    };
    let extension = if is_dir {
        String::new()
    } else {
        path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
    };
    Some(Entry {
        path: path.to_path_buf(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        is_dir,
        is_symlink,
        size: if is_dir { 0 } else { meta.len() },
        modified: meta.modified().ok(),
        extension,
    })
}
