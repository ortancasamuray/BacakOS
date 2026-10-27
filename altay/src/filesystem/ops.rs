// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Sandbox-checked file operations: rename, create and permanent delete.
//! (Copies and moves run in the `transfer` module.)
//!
//! Source and destination are both validated. Destinations use
//! [`Sandbox::resolve_for_create`] because the target usually doesn't exist yet.

use std::path::Path;

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

/// Delete `path` for good (a folder with everything in it) — for network
/// shares, which have no trash. A symlink is removed itself, never what it
/// points at: only its folder is resolved through the sandbox.
pub fn delete_permanently(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), OperationError> {
    let path = path.as_ref();
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(OperationError::Denied(AccessDenied::MissingParent));
    };
    let target = sandbox.resolve(parent)?.as_path().join(name);
    if std::fs::symlink_metadata(&target)?.is_dir() {
        std::fs::remove_dir_all(&target)?;
    } else {
        std::fs::remove_file(&target)?;
    }
    Ok(())
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
