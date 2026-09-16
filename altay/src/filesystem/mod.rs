// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Filesystem listing and operations. Every entry point takes the [`Sandbox`]
//! so no operation can touch a path the policy forbids.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::security::{AccessDenied, SafePath, Sandbox};

mod ops;

pub use ops::{OperationError, Progress};

/// One row in a directory listing.
#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Lowercase extension without the dot, e.g. "png". Empty for dirs/no-ext.
    pub extension: String,
}

impl Entry {
    fn from_dir_entry(entry: &std::fs::DirEntry) -> Option<Self> {
        let path = entry.path();
        let meta = entry.metadata().ok()?;
        let file_type = meta.file_type();
        let is_symlink = file_type.is_symlink();
        // For symlinks, resolve the target's type so folders-via-symlink open.
        let is_dir = if is_symlink {
            std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false)
        } else {
            file_type.is_dir()
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let extension = if is_dir {
            String::new()
        } else {
            path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
        };
        Some(Entry {
            path,
            name,
            is_dir,
            is_symlink,
            size: if is_dir { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            extension,
        })
    }
}

/// Recursively sum the byte sizes of all files under `path`.
pub fn recursive_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0; };
    if meta.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else { return 0; };
        rd.flatten().map(|e| recursive_size(&e.path())).sum()
    } else {
        meta.len()
    }
}

/// Count the direct children of a directory (non-recursive).
pub fn child_count(path: &Path) -> usize {
    std::fs::read_dir(path).map(|rd| rd.flatten().count()).unwrap_or(0)
}

/// How a listing should be ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

/// List the contents of a directory. The directory must pass the sandbox; the
/// returned entries are *not* individually re-validated (cheap), but any later
/// operation on them is.
pub fn list_dir(
    sandbox: &Sandbox,
    dir: impl AsRef<Path>,
    show_hidden: bool,
    sort: SortKey,
) -> Result<Vec<Entry>, ListError> {
    let safe = sandbox.resolve(dir)?;
    let read = std::fs::read_dir(safe.as_path())?;
    let mut entries: Vec<Entry> = read
        .flatten()
        .filter_map(|e| Entry::from_dir_entry(&e))
        .filter(|e| show_hidden || !e.name.starts_with('.'))
        .collect();
    sort_entries(&mut entries, sort);
    Ok(entries)
}

/// Stable sort: directories first, then by the requested key.
pub fn sort_entries(entries: &mut [Entry], sort: SortKey) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir) // dirs first
            .then_with(|| match sort {
                SortKey::Name => natural_cmp(&a.name, &b.name),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Modified => a.modified.cmp(&b.modified),
                SortKey::Kind => a.extension.cmp(&b.extension).then_with(|| natural_cmp(&a.name, &b.name)),
            })
    });
}

/// Case-insensitive comparison that orders embedded numbers naturally
/// (`file2` before `file10`).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                match na.cmp(&nb) {
                    std::cmp::Ordering::Equal => continue,
                    ord => return ord,
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_ascii_lowercase(), y.to_ascii_lowercase());
                match lx.cmp(&ly) {
                    std::cmp::Ordering::Equal => {
                        ai.next();
                        bi.next();
                    }
                    ord => return ord,
                }
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> u64 {
    let mut n: u64 = 0;
    while let Some(c) = it.peek().copied() {
        if let Some(d) = c.to_digit(10) {
            n = n.saturating_mul(10).saturating_add(d as u64);
            it.next();
        } else {
            break;
        }
    }
    n
}

/// Errors when listing a directory.
#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// Re-export the validated operations.
pub use ops::{
    copy, create_dir, create_file, duplicate, move_to, rename, validate_pair, validate_target,
};

/// Validate a single path for an operation that mutates it (delete/rename).
pub fn require(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<SafePath, AccessDenied> {
    sandbox.resolve(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    fn home_sandbox(tag: &str) -> (Sandbox, PathBuf) {
        let home = std::env::temp_dir().join(format!("altay-fs-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&home).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        (Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]), canon)
    }

    #[test]
    fn rename_moves_within_parent() {
        let (sb, home) = home_sandbox("rename");
        std::fs::write(home.join("old.txt"), b"x").unwrap();
        let safe = rename(&sb, home.join("old.txt"), "new.txt").unwrap();
        assert!(safe.as_path().ends_with("new.txt"));
        assert!(home.join("new.txt").exists());
        assert!(!home.join("old.txt").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn rename_rejects_path_separators_and_dotdot() {
        let (sb, home) = home_sandbox("rename2");
        std::fs::write(home.join("f"), b"x").unwrap();
        assert!(rename(&sb, home.join("f"), "../escape").is_err());
        assert!(rename(&sb, home.join("f"), "a/b").is_err());
        assert!(rename(&sb, home.join("f"), "..").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
