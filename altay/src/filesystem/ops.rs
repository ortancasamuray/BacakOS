// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Sandbox-checked file operations: copy, move, rename, create, duplicate.
//!
//! Source and destination are both validated. Destinations use
//! [`Sandbox::resolve_for_create`] because the target usually doesn't exist yet.

use std::path::{Path, PathBuf};

use crate::security::{AccessDenied, SafePath, Sandbox};

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("destination already exists: {0}")]
    Exists(String),
    #[error("source and destination are the same")]
    SamePath,
}

/// Progress callback payload for long-running copies/moves.
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
}

/// Validate that a destination directory is writable within the sandbox.
pub fn validate_target(sandbox: &Sandbox, dir: impl AsRef<Path>) -> Result<SafePath, AccessDenied> {
    sandbox.resolve(dir)
}

/// Validate a (source, destination) pair for a copy/move.
pub fn validate_pair(
    sandbox: &Sandbox,
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
) -> Result<(SafePath, SafePath), OperationError> {
    let src = sandbox.resolve(src)?;
    let dst = sandbox.resolve_for_create(dst)?;
    if src.as_path() == dst.as_path() {
        return Err(OperationError::SamePath);
    }
    Ok((src, dst))
}

/// Recursively copy `src` to `dst`, reporting progress. Both validated.
pub fn copy(
    sandbox: &Sandbox,
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<SafePath, OperationError> {
    let (src, dst) = validate_pair(sandbox, src, dst)?;
    if dst.as_path().exists() {
        return Err(OperationError::Exists(dst.as_path().display().to_string()));
    }
    let total = dir_size(src.as_path());
    let mut state = Progress { bytes_done: 0, bytes_total: total.0, files_done: 0, files_total: total.1 };
    copy_recursive(src.as_path(), dst.as_path(), &mut state, on_progress)?;
    Ok(dst)
}

fn copy_recursive(
    src: &Path,
    dst: &Path,
    state: &mut Progress,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), OperationError> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()), state, on_progress)?;
        }
    } else {
        let bytes = std::fs::copy(src, dst)?;
        state.bytes_done = state.bytes_done.saturating_add(bytes);
        state.files_done += 1;
        on_progress(*state);
    }
    Ok(())
}

/// Move `src` to `dst`. Tries an atomic rename first (same filesystem), then
/// falls back to copy + delete for cross-device moves.
pub fn move_to(
    sandbox: &Sandbox,
    src: impl AsRef<Path>,
    dst: impl AsRef<Path>,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<SafePath, OperationError> {
    let (src, dst) = validate_pair(sandbox, src, dst)?;
    if dst.as_path().exists() {
        return Err(OperationError::Exists(dst.as_path().display().to_string()));
    }
    match std::fs::rename(src.as_path(), dst.as_path()) {
        Ok(()) => Ok(dst),
        Err(e) if e.raw_os_error() == Some(libc_exdev()) => {
            // Cross-device: copy then remove the source.
            let total = dir_size(src.as_path());
            let mut state = Progress { bytes_done: 0, bytes_total: total.0, files_done: 0, files_total: total.1 };
            copy_recursive(src.as_path(), dst.as_path(), &mut state, on_progress)?;
            remove_path(src.as_path())?;
            Ok(dst)
        }
        Err(e) => Err(e.into()),
    }
}

/// Rename in place (same parent). `new_name` is a bare file name.
pub fn rename(
    sandbox: &Sandbox,
    path: impl AsRef<Path>,
    new_name: &str,
) -> Result<SafePath, OperationError> {
    if new_name.is_empty() || new_name.contains('/') || new_name == "." || new_name == ".." {
        return Err(OperationError::Denied(AccessDenied::OutsideSandbox));
    }
    let src = sandbox.resolve(path)?;
    let parent = src.as_path().parent().ok_or(AccessDenied::MissingParent)?;
    let dst = sandbox.resolve_for_create(parent.join(new_name))?;
    if dst.as_path().exists() {
        return Err(OperationError::Exists(new_name.to_string()));
    }
    std::fs::rename(src.as_path(), dst.as_path())?;
    Ok(dst)
}

/// Create a new directory (and parents within the sandbox).
pub fn create_dir(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<SafePath, OperationError> {
    let dst = sandbox.resolve_for_create(path)?;
    if dst.as_path().exists() {
        return Err(OperationError::Exists(dst.as_path().display().to_string()));
    }
    std::fs::create_dir_all(dst.as_path())?;
    Ok(dst)
}

/// Create a new empty file.
pub fn create_file(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<SafePath, OperationError> {
    let dst = sandbox.resolve_for_create(path)?;
    if dst.as_path().exists() {
        return Err(OperationError::Exists(dst.as_path().display().to_string()));
    }
    std::fs::File::create(dst.as_path())?;
    Ok(dst)
}

/// Duplicate a file/dir next to itself with a " (copy)" suffix.
pub fn duplicate(
    sandbox: &Sandbox,
    path: impl AsRef<Path>,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<SafePath, OperationError> {
    let src = sandbox.resolve(path)?;
    let candidate = unique_copy_name(src.as_path());
    copy(sandbox, src.as_path(), candidate, on_progress)
}

/// Produce a non-colliding "x (copy)" / "x (copy 2)" path.
fn unique_copy_name(src: &Path) -> PathBuf {
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1..=9999 {
        let suffix = if n == 1 { " (copy)".to_string() } else { format!(" (copy {n})") };
        let candidate = parent.join(format!("{stem}{suffix}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!("{stem} (copy){ext}"))
}

/// Remove a file or directory tree (used by move fallback; trash is preferred
/// for user-facing deletes — see the `trash` module).
fn remove_path(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// (total bytes, file count) of a path tree, for progress totals.
fn dir_size(path: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_dir() {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    let (b, f) = dir_size(&e.path());
                    bytes += b;
                    files += f;
                }
            }
        } else {
            bytes += meta.len();
            files += 1;
        }
    }
    (bytes, files)
}

/// EXDEV constant (cross-device link) without pulling in the libc crate.
fn libc_exdev() -> i32 {
    18
}
