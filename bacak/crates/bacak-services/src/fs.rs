//! Virtual filesystem — native + archive-backed paths, async ops, watchers.
//!
//! `VfsPath` abstracts over native paths, archive-relative paths, and (eventually)
//! remote schemes. The file manager UI does not care which is which: directory
//! listings, copies, and metadata go through the same surface.

use crate::archive;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use thiserror::Error;
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// Path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VfsPath {
    Native  { path: PathBuf },
    Archive { archive: PathBuf, inside: PathBuf },
    Remote  { scheme: String, url: String },
}

impl VfsPath {
    pub fn native<P: Into<PathBuf>>(p: P) -> Self {
        Self::Native { path: p.into() }
    }

    pub fn archive<A: Into<PathBuf>, I: Into<PathBuf>>(archive: A, inside: I) -> Self {
        Self::Archive { archive: archive.into(), inside: inside.into() }
    }

    pub fn name(&self) -> String {
        match self {
            VfsPath::Native { path } => path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            VfsPath::Archive { inside, archive } => inside
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| archive.display().to_string()),
            VfsPath::Remote { url, .. } => url.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry & metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: VfsPath,
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: i64,
    pub mime: Option<String>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum FsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive: {0}")]
    Archive(#[from] archive::ArcError),
    #[error("unsupported path variant: {0}")]
    Unsupported(&'static str),
    #[error("watch: {0}")]
    Watch(String),
}

pub type Result<T> = std::result::Result<T, FsError>;

// ---------------------------------------------------------------------------
// Read directory
// ---------------------------------------------------------------------------

pub async fn read_dir(p: &VfsPath) -> Result<Vec<DirEntry>> {
    match p {
        VfsPath::Native { path } => read_native_dir(path).await,
        VfsPath::Archive { archive, inside } => read_archive_dir(archive, inside).await,
        VfsPath::Remote { .. } => Err(FsError::Unsupported("remote")),
    }
}

async fn read_native_dir(p: &Path) -> Result<Vec<DirEntry>> {
    let mut rd = tokio::fs::read_dir(p).await?;
    let mut out = Vec::new();
    while let Some(entry) = rd.next_entry().await? {
        let meta = entry.metadata().await?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let mime = mime_for_path(&path);
        out.push(DirEntry {
            name,
            path: VfsPath::Native { path },
            is_dir: meta.is_dir(),
            size: meta.len(),
            mtime_ms,
            mime,
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

async fn read_archive_dir(arc: &Path, inside: &Path) -> Result<Vec<DirEntry>> {
    let archive_pb = arc.to_path_buf();
    let inside_pb = inside.to_path_buf();
    let entries = tokio::task::spawn_blocking(move || {
        archive::list_dir(&archive_pb, &inside_pb)
    })
    .await
    .map_err(|e| FsError::Watch(format!("join: {e}")))??;

    let out = entries
        .into_iter()
        .map(|e| DirEntry {
            name: e.name.clone(),
            path: VfsPath::Archive {
                archive: arc.to_path_buf(),
                inside: e.path_inside,
            },
            is_dir: e.is_dir,
            size: e.size,
            mtime_ms: e.mtime_ms,
            mime: mime_for_name(&e.name),
        })
        .collect::<Vec<_>>();
    let mut out = out;
    sort_entries(&mut out);
    Ok(out)
}

fn sort_entries(v: &mut Vec<DirEntry>) {
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub is_dir: bool,
    pub size: u64,
    pub mtime_ms: i64,
    pub mime: Option<String>,
}

pub async fn metadata(p: &VfsPath) -> Result<Metadata> {
    match p {
        VfsPath::Native { path } => {
            let m = tokio::fs::metadata(path).await?;
            Ok(Metadata {
                is_dir: m.is_dir(),
                size: m.len(),
                mtime_ms: m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0),
                mime: mime_for_path(path),
            })
        }
        VfsPath::Archive { archive: arc, inside } => {
            let a = arc.clone();
            let i = inside.clone();
            let entry = tokio::task::spawn_blocking(move || archive::stat(&a, &i))
                .await
                .map_err(|e| FsError::Watch(format!("join: {e}")))??;
            Ok(Metadata {
                is_dir: entry.is_dir,
                size: entry.size,
                mtime_ms: entry.mtime_ms,
                mime: mime_for_name(&entry.name),
            })
        }
        VfsPath::Remote { .. } => Err(FsError::Unsupported("remote")),
    }
}

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

pub async fn copy_to_native(from: &VfsPath, to: &Path) -> Result<u64> {
    match from {
        VfsPath::Native { path } => {
            let bytes = tokio::fs::copy(path, to).await?;
            Ok(bytes)
        }
        VfsPath::Archive { archive: arc, inside } => {
            let arc = arc.clone();
            let inside = inside.clone();
            let to = to.to_path_buf();
            let n = tokio::task::spawn_blocking(move || {
                archive::extract_entry(&arc, &inside, &to)
            })
            .await
            .map_err(|e| FsError::Watch(format!("join: {e}")))??;
            Ok(n)
        }
        VfsPath::Remote { .. } => Err(FsError::Unsupported("remote")),
    }
}

// ---------------------------------------------------------------------------
// Watch
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WatchEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
    Other,
}

pub fn watch(dir: &Path) -> Result<(WatchHandle, mpsc::Receiver<WatchEvent>)> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};
    let (tx, rx) = mpsc::channel::<WatchEvent>(256);
    let tx2 = tx.clone();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let evt = match res {
            Ok(e) => match e.kind {
                EventKind::Create(_) => e.paths.into_iter().next().map(WatchEvent::Created),
                EventKind::Modify(_) => e.paths.into_iter().next().map(WatchEvent::Modified),
                EventKind::Remove(_) => e.paths.into_iter().next().map(WatchEvent::Removed),
                _ => Some(WatchEvent::Other),
            },
            Err(_) => return,
        };
        if let Some(evt) = evt {
            let _ = tx2.try_send(evt);
        }
    })
    .map_err(|e| FsError::Watch(e.to_string()))?;

    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|e| FsError::Watch(e.to_string()))?;

    Ok((WatchHandle(Box::new(watcher)), rx))
}

pub struct WatchHandle(#[allow(dead_code)] Box<dyn notify::Watcher + Send + Sync>);

// ---------------------------------------------------------------------------
// MIME inference (extension-based fallback)
// ---------------------------------------------------------------------------

pub fn mime_for_path(p: &Path) -> Option<String> {
    p.file_name()
        .and_then(|n| n.to_str())
        .and_then(mime_for_name)
}

pub fn mime_for_name(name: &str) -> Option<String> {
    let ext = name.rsplit('.').next()?.to_lowercase();
    Some(
        match ext.as_str() {
            "png"  => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif"  => "image/gif",
            "webp" => "image/webp",
            "svg"  => "image/svg+xml",
            "pdf"  => "application/pdf",
            "zip"  => "application/zip",
            "7z"   => "application/x-7z-compressed",
            "tar"  => "application/x-tar",
            "gz" | "tgz" => "application/gzip",
            "rar"  => "application/vnd.rar",
            "iso"  => "application/x-iso9660-image",
            "txt" | "md" => "text/plain",
            "html" | "htm" => "text/html",
            "css"  => "text/css",
            "js" | "mjs" => "application/javascript",
            "json" => "application/json",
            "toml" => "application/toml",
            "rs"   => "text/x-rust",
            _ => return None,
        }
        .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_a_native_dir() {
        let tmp = std::env::temp_dir().join("bacak-services-fs-test");
        let _ = std::fs::create_dir_all(&tmp);
        let _ = std::fs::write(tmp.join("a.txt"), b"hi");
        let _ = std::fs::create_dir_all(tmp.join("sub"));

        let v = read_dir(&VfsPath::native(&tmp)).await.unwrap();
        assert!(v.iter().any(|e| e.name == "a.txt"));
        assert!(v.iter().any(|e| e.name == "sub" && e.is_dir));
        assert!(v.first().map(|e| e.is_dir).unwrap_or(false));
    }

    #[test]
    fn mime_basic() {
        assert_eq!(mime_for_name("foo.png").as_deref(), Some("image/png"));
        assert_eq!(mime_for_name("Cargo.toml").as_deref(), Some("application/toml"));
        assert_eq!(mime_for_name("noext"), None);
    }

    #[test]
    fn vfs_name_native() {
        let v = VfsPath::native("/tmp/file.txt");
        assert_eq!(v.name(), "file.txt");
    }
}
