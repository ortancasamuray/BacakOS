//! Archive backends — ZIP / TAR / TAR.GZ presented as virtual directories.
//!
//! Each backend implements the same listing/stat/extract surface; selection is
//! by file extension. Operations are blocking — callers in async contexts wrap
//! them in `spawn_blocking` to keep the runtime free.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{copy as io_copy, Read};
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub name: String,
    pub path_inside: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: i64,
}

#[derive(Debug, Error)]
pub enum ArcError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("unsupported archive format: {0}")]
    Unsupported(String),
    #[error("entry not found: {0}")]
    NotFound(String),
    #[error("path escapes archive root: {0}")]
    Traversal(String),
}

pub type Result<T> = std::result::Result<T, ArcError>;

// ---------------------------------------------------------------------------
// Backend selection
// ---------------------------------------------------------------------------

enum Backend {
    Zip,
    Tar,
    TarGz,
}

fn detect_backend(p: &Path) -> Result<Backend> {
    let name = p
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| ArcError::Unsupported("no filename".into()))?
        .to_lowercase();

    if name.ends_with(".zip") {
        Ok(Backend::Zip)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Ok(Backend::TarGz)
    } else if name.ends_with(".tar") {
        Ok(Backend::Tar)
    } else {
        Err(ArcError::Unsupported(name))
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn list_dir(archive: &Path, dir: &Path) -> Result<Vec<ArchiveEntry>> {
    match detect_backend(archive)? {
        Backend::Zip   => zip_list_dir(archive, dir),
        Backend::Tar   => tar_list_dir(archive, dir, false),
        Backend::TarGz => tar_list_dir(archive, dir, true),
    }
}

pub fn stat(archive: &Path, inside: &Path) -> Result<ArchiveEntry> {
    match detect_backend(archive)? {
        Backend::Zip   => zip_stat(archive, inside),
        Backend::Tar   => tar_stat(archive, inside, false),
        Backend::TarGz => tar_stat(archive, inside, true),
    }
}

pub fn extract_entry(archive: &Path, inside: &Path, to: &Path) -> Result<u64> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match detect_backend(archive)? {
        Backend::Zip   => zip_extract(archive, inside, to),
        Backend::Tar   => tar_extract(archive, inside, to, false),
        Backend::TarGz => tar_extract(archive, inside, to, true),
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

fn normalize_inside(p: &Path) -> Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                return Err(ArcError::Traversal(p.display().to_string()));
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Ok(out)
}

fn direct_child(entry: &Path, dir: &Path) -> Option<String> {
    let rel = entry.strip_prefix(dir).ok()?;
    let mut comps = rel.components();
    let first = comps.next()?;
    if comps.next().is_some() {
        return None;
    }
    match first {
        Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
        _ => None,
    }
}

fn descends_from(entry: &Path, dir: &Path) -> bool {
    entry.starts_with(dir) && entry != dir
}

// ---------------------------------------------------------------------------
// ZIP backend
// ---------------------------------------------------------------------------

fn zip_list_dir(archive: &Path, dir: &Path) -> Result<Vec<ArchiveEntry>> {
    let dir = normalize_inside(dir)?;
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;

    let mut direct: Vec<ArchiveEntry> = Vec::new();
    let mut implicit_dirs: BTreeSet<String> = BTreeSet::new();

    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        let name = entry.name();
        if name.contains("..") {
            continue;
        }
        let p = PathBuf::from(name);

        if let Some(child) = direct_child(&p, &dir) {
            let is_dir = entry.is_dir() || name.ends_with('/');
            let size = if is_dir { 0 } else { entry.size() };
            direct.push(ArchiveEntry {
                name: child,
                path_inside: p.clone(),
                is_dir,
                size,
                mtime_ms: zip_mtime_ms(&entry),
            });
        } else if descends_from(&p, &dir) {
            if let Ok(rel) = p.strip_prefix(&dir) {
                if let Some(Component::Normal(first)) = rel.components().next() {
                    implicit_dirs.insert(first.to_string_lossy().into_owned());
                }
            }
        }
    }

    let explicit: BTreeSet<String> = direct.iter().map(|e| e.name.clone()).collect();
    for d in implicit_dirs {
        if !explicit.contains(&d) {
            direct.push(ArchiveEntry {
                name: d.clone(),
                path_inside: dir.join(&d),
                is_dir: true,
                size: 0,
                mtime_ms: 0,
            });
        }
    }

    Ok(direct)
}

fn zip_stat(archive: &Path, inside: &Path) -> Result<ArchiveEntry> {
    let inside = normalize_inside(inside)?;
    let target = inside.to_string_lossy();
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    for i in 0..zip.len() {
        let e = zip.by_index(i)?;
        let n = e.name();
        if n.trim_end_matches('/') == target {
            return Ok(ArchiveEntry {
                name: inside
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path_inside: inside.clone(),
                is_dir: e.is_dir(),
                size: if e.is_dir() { 0 } else { e.size() },
                mtime_ms: zip_mtime_ms(&e),
            });
        }
    }
    Err(ArcError::NotFound(target.into_owned()))
}

fn zip_extract(archive: &Path, inside: &Path, to: &Path) -> Result<u64> {
    let inside = normalize_inside(inside)?;
    let target = inside.to_string_lossy();
    let file = File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    for i in 0..zip.len() {
        let mut e = zip.by_index(i)?;
        if e.name() == target {
            let mut out = File::create(to)?;
            return Ok(io_copy(&mut e, &mut out)?);
        }
    }
    Err(ArcError::NotFound(target.into_owned()))
}

fn zip_mtime_ms(_entry: &zip::read::ZipFile<'_>) -> i64 {
    // zip-rs 0.6 does not expose a stable epoch ms; defer real mtime to v2.
    0
}

// ---------------------------------------------------------------------------
// TAR backend (gz optional)
// ---------------------------------------------------------------------------

fn open_tar(archive: &Path, gz: bool) -> Result<tar::Archive<Box<dyn Read>>> {
    let f = File::open(archive)?;
    let reader: Box<dyn Read> = if gz {
        Box::new(flate2::read::GzDecoder::new(f))
    } else {
        Box::new(f)
    };
    Ok(tar::Archive::new(reader))
}

fn tar_list_dir(archive: &Path, dir: &Path, gz: bool) -> Result<Vec<ArchiveEntry>> {
    let dir = normalize_inside(dir)?;
    let mut tar = open_tar(archive, gz)?;

    let mut direct: Vec<ArchiveEntry> = Vec::new();
    let mut implicit_dirs: BTreeSet<String> = BTreeSet::new();

    for entry in tar.entries()? {
        let entry = entry?;
        let header = entry.header();
        let path = entry.path()?.into_owned();
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            continue;
        }
        let is_dir = header.entry_type().is_dir();
        let size = header.size().unwrap_or(0);
        let mtime_ms = header.mtime().map(|m| m as i64 * 1000).unwrap_or(0);

        if let Some(child) = direct_child(&path, &dir) {
            direct.push(ArchiveEntry {
                name: child,
                path_inside: path.clone(),
                is_dir,
                size,
                mtime_ms,
            });
        } else if descends_from(&path, &dir) {
            if let Ok(rel) = path.strip_prefix(&dir) {
                if let Some(Component::Normal(first)) = rel.components().next() {
                    implicit_dirs.insert(first.to_string_lossy().into_owned());
                }
            }
        }
    }

    let explicit: BTreeSet<String> = direct.iter().map(|e| e.name.clone()).collect();
    for d in implicit_dirs {
        if !explicit.contains(&d) {
            direct.push(ArchiveEntry {
                name: d.clone(),
                path_inside: dir.join(&d),
                is_dir: true,
                size: 0,
                mtime_ms: 0,
            });
        }
    }

    Ok(direct)
}

fn tar_stat(archive: &Path, inside: &Path, gz: bool) -> Result<ArchiveEntry> {
    let inside = normalize_inside(inside)?;
    let mut tar = open_tar(archive, gz)?;
    for entry in tar.entries()? {
        let entry = entry?;
        let path = entry.path()?.into_owned();
        if path == inside {
            let h = entry.header();
            return Ok(ArchiveEntry {
                name: inside
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                path_inside: inside.clone(),
                is_dir: h.entry_type().is_dir(),
                size: h.size().unwrap_or(0),
                mtime_ms: h.mtime().map(|m| m as i64 * 1000).unwrap_or(0),
            });
        }
    }
    Err(ArcError::NotFound(inside.display().to_string()))
}

fn tar_extract(archive: &Path, inside: &Path, to: &Path, gz: bool) -> Result<u64> {
    let inside = normalize_inside(inside)?;
    let mut tar = open_tar(archive, gz)?;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path == inside {
            let mut out = File::create(to)?;
            return Ok(io_copy(&mut entry, &mut out)?);
        }
    }
    Err(ArcError::NotFound(inside.display().to_string()))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(stem: &str, ext: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("bacak-archive-{}-{}.{}", stem, std::process::id(), ext));
        p
    }

    #[test]
    fn lists_zip_root() {
        let p = tmp("simple", "zip");
        let f = File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.start_file("hello.txt", opts).unwrap();
        z.write_all(b"hi").unwrap();
        z.start_file("dir/inside.txt", opts).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();

        let root = list_dir(&p, Path::new("")).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"hello.txt".to_string()));
        assert!(names.contains(&"dir".to_string()));
        let dir = root.iter().find(|e| e.name == "dir").unwrap();
        assert!(dir.is_dir);

        let inside = list_dir(&p, Path::new("dir")).unwrap();
        let names: Vec<_> = inside.iter().map(|e| e.name.clone()).collect();
        assert!(names.contains(&"inside.txt".to_string()));

        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn rejects_path_traversal() {
        let p = tmp("trav", "zip");
        let f = File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::FileOptions::default();
        z.start_file("ok.txt", opts).unwrap();
        z.write_all(b"hi").unwrap();
        z.finish().unwrap();

        let err = list_dir(&p, Path::new("../etc")).unwrap_err();
        assert!(matches!(err, ArcError::Traversal(_)));
        std::fs::remove_file(&p).ok();
    }
}
