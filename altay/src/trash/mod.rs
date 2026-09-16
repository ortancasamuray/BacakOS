// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Trash support following the freedesktop.org Trash specification, via the
//! `trash` crate. User-facing deletes go here rather than to `rm`, so they are
//! recoverable. All paths are sandbox-validated before being trashed.

use std::path::{Path, PathBuf};

use crate::security::{AccessDenied, Sandbox};

#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    #[error(transparent)]
    Denied(#[from] AccessDenied),
    #[error("trash operation failed: {0}")]
    Backend(String),
}

/// An item currently sitting in the trash.
#[derive(Debug, Clone)]
pub struct TrashedItem {
    pub original_path: PathBuf,
    pub name: String,
    pub deleted_at: i64,
    /// Opaque id used to restore/purge this exact item.
    id: TrashId,
}

#[derive(Debug, Clone)]
struct TrashId(#[allow(dead_code)] String);

/// Move a path to the trash (recoverable delete).
pub fn move_to_trash(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), TrashError> {
    let safe = sandbox.resolve(path)?;
    trash::delete(safe.as_path()).map_err(|e| TrashError::Backend(e.to_string()))
}

/// Trash several paths in one call.
pub fn move_many_to_trash(
    sandbox: &Sandbox,
    paths: &[PathBuf],
) -> Result<(), TrashError> {
    let mut validated = Vec::with_capacity(paths.len());
    for p in paths {
        validated.push(sandbox.resolve(p)?.into_path_buf());
    }
    trash::delete_all(&validated).map_err(|e| TrashError::Backend(e.to_string()))
}

/// List the contents of the trash (where the platform supports it).
pub fn list() -> Result<Vec<TrashedItem>, TrashError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use trash::os_limited as t;
        let items = t::list().map_err(|e| TrashError::Backend(e.to_string()))?;
        Ok(items
            .into_iter()
            .map(|i| TrashedItem {
                name: i.name.to_string_lossy().into_owned(),
                original_path: i.original_path(),
                deleted_at: i.time_deleted,
                id: TrashId(format!("{:?}", i.id)),
            })
            .collect())
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn trash_then_restore_roundtrip() {
        let home = std::env::temp_dir().join(format!("altay-trash-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let file = canon.join("restore-me.txt");
        std::fs::write(&file, b"bring me back").unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        // Trashing needs a freedesktop trash backend; skip if unavailable.
        if move_to_trash(&sb, &file).is_err() {
            eprintln!("skipping: no trash backend available");
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        assert!(!file.exists(), "file should be gone after trashing");

        let restored = restore_many(&sb, &[file.clone()]).expect("restore should succeed");
        assert_eq!(restored, 1);
        assert!(file.exists(), "file should be back at its original path");
        assert_eq!(std::fs::read(&file).unwrap(), b"bring me back");
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Restore trashed items (matched by original path) back to where they were.
/// Each restore target is validated against the sandbox, so nothing can be
/// restored to a location outside the permitted areas. Returns how many were
/// restored.
pub fn restore_many(sandbox: &Sandbox, originals: &[PathBuf]) -> Result<usize, TrashError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::collections::HashSet;
        use trash::os_limited as t;
        let wanted: HashSet<&Path> = originals.iter().map(|p| p.as_path()).collect();
        let all = t::list().map_err(|e| TrashError::Backend(e.to_string()))?;
        let mut to_restore = Vec::new();
        for item in all {
            let original = item.original_path();
            if wanted.contains(original.as_path()) {
                // The restore destination must lie within the sandbox.
                sandbox.resolve_for_create(&original)?;
                to_restore.push(item);
            }
        }
        let n = to_restore.len();
        if n == 0 {
            return Ok(0);
        }
        t::restore_all(to_restore).map_err(|e| TrashError::Backend(e.to_string()))?;
        Ok(n)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (sandbox, originals);
        Ok(0)
    }
}

/// Permanently empty the trash.
pub fn empty() -> Result<(), TrashError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use trash::os_limited as t;
        let items = t::list().map_err(|e| TrashError::Backend(e.to_string()))?;
        t::purge_all(items).map_err(|e| TrashError::Backend(e.to_string()))
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        Ok(())
    }
}
