// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Trash support following the freedesktop.org Trash specification, via the
//! `trash` crate. User-facing deletes go here rather than to `rm`, so they are
//! recoverable. All paths are sandbox-validated before being trashed.

use std::path::PathBuf;

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
    /// The backend's record, used to restore this exact item (several items
    /// can share an original path).
    item: trash::TrashItem,
}

/// Outcome of a restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restored {
    pub restored: usize,
    /// Older items left in the trash because a newer one was restored to the
    /// same original path in the same call.
    pub kept: usize,
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
                item: i,
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
    use std::path::Path;

    /// Move a single path to the trash.
    fn move_to_trash(sandbox: &Sandbox, path: impl AsRef<Path>) -> Result<(), TrashError> {
        let safe = sandbox.resolve(path)?;
        trash::delete(safe.as_path()).map_err(|e| TrashError::Backend(e.to_string()))
    }

    #[test]
    fn restore_latest_puts_back_only_the_newest_twin() {
        let home = std::env::temp_dir().join(format!("altay-trash-twin-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let file = canon.join("twin.txt");
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        std::fs::write(&file, b"older").unwrap();
        if move_to_trash(&sb, &file).is_err() {
            eprintln!("skipping: no trash backend available");
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        // trashinfo deletion times have one-second resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&file, b"newer").unwrap();
        move_many_to_trash(&sb, &[file.clone()]).unwrap();

        assert_eq!(restore_latest(&sb, &[file.clone()]).unwrap(), 1);
        assert_eq!(std::fs::read(&file).unwrap(), b"newer");

        // Clean up the older twin left in the trash.
        use trash::os_limited as t;
        let leftovers: Vec<_> = t::list().unwrap().into_iter().filter(|i| i.original_path() == file).collect();
        assert_eq!(leftovers.len(), 1, "the older twin should still be in the trash");
        t::purge_all(leftovers).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn restore_latest_matches_paths_through_symlinked_dirs() {
        let home = std::env::temp_dir().join(format!("altay-trash-link-{}", std::process::id()));
        std::fs::create_dir_all(home.join("real")).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        std::os::unix::fs::symlink(canon.join("real"), canon.join("via-link")).unwrap();
        let shown = canon.join("via-link/f.txt"); // the path the listing showed
        std::fs::write(&shown, b"x").unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);
        if move_many_to_trash(&sb, &[shown.clone()]).is_err() {
            eprintln!("skipping: no trash backend available");
            let _ = std::fs::remove_dir_all(&home);
            return;
        }
        assert_eq!(restore_latest(&sb, &[shown.clone()]).unwrap(), 1);
        assert!(canon.join("real/f.txt").exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Trash `older` then `newer` content from the same path; returns the two
    /// trash records (older first), or None without a trash backend.
    fn trashed_twins(sb: &Sandbox, file: &Path) -> Option<(TrashedItem, TrashedItem)> {
        std::fs::write(file, b"older").unwrap();
        move_to_trash(sb, file).ok()?;
        // trashinfo deletion times have one-second resolution.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(file, b"newer").unwrap();
        move_to_trash(sb, file).unwrap();
        let mut twins: Vec<TrashedItem> = list().unwrap().into_iter().filter(|i| i.original_path == file).collect();
        assert_eq!(twins.len(), 2);
        twins.sort_by_key(|i| i.item.time_deleted);
        let newer = twins.pop().unwrap();
        Some((twins.pop().unwrap(), newer))
    }

    #[test]
    fn restore_items_picks_the_exact_twin() {
        let home = std::env::temp_dir().join(format!("altay-trash-pick-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let file = canon.join("twin.txt");
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);
        let Some((older, _newer)) = trashed_twins(&sb, &file) else {
            eprintln!("skipping: no trash backend available");
            let _ = std::fs::remove_dir_all(&home);
            return;
        };
        // Only the older twin selected: exactly that one comes back.
        assert_eq!(restore_items(&sb, &[older]).unwrap(), Restored { restored: 1, kept: 0 });
        assert_eq!(std::fs::read(&file).unwrap(), b"older");
        // Both selected: the newest wins, the other stays in the trash.
        std::fs::remove_file(&file).unwrap();
        let again: Vec<TrashedItem> = list().unwrap().into_iter().filter(|i| i.original_path == file).collect();
        assert_eq!(again.len(), 1, "the newer twin is still trashed");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&file, b"third").unwrap();
        move_to_trash(&sb, &file).unwrap();
        let both: Vec<TrashedItem> = list().unwrap().into_iter().filter(|i| i.original_path == file).collect();
        assert_eq!(restore_items(&sb, &both).unwrap(), Restored { restored: 1, kept: 1 });
        assert_eq!(std::fs::read(&file).unwrap(), b"third");
        // Clean up what this test left in the trash.
        let left: Vec<_> = trash::os_limited::list().unwrap().into_iter().filter(|i| i.original_path() == file).collect();
        trash::os_limited::purge_all(left).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

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

        let restored = restore_latest(&sb, &[file.clone()]).expect("restore should succeed");
        assert_eq!(restored, 1);
        assert!(file.exists(), "file should be back at its original path");
        assert_eq!(std::fs::read(&file).unwrap(), b"bring me back");
        let _ = std::fs::remove_dir_all(&home);
    }
}

/// Undo a [`move_many_to_trash`]: put back, for each original path, only the
/// most recently trashed item. An older item trashed from the same path
/// stays in the trash.
pub fn restore_latest(sandbox: &Sandbox, originals: &[PathBuf]) -> Result<usize, TrashError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use trash::os_limited as t;
        // Trashing resolved the paths through the sandbox; compare in the
        // same (canonical-parent) form the trash recorded them in.
        let wanted: Vec<PathBuf> = originals
            .iter()
            .map(|p| sandbox.resolve_for_create(p).map(|s| s.into_path_buf()).unwrap_or_else(|_| p.clone()))
            .collect();
        let all = t::list().map_err(|e| TrashError::Backend(e.to_string()))?;
        let matching = all.into_iter().filter(|i| wanted.contains(&i.original_path())).collect();
        Ok(restore_newest(sandbox, matching)?.restored)
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (sandbox, originals);
        Ok(0)
    }
}

/// Restore exactly these items (as listed by [`list`]) to their original
/// locations. If several share an original path only the newest is restored
/// — two can't be put in the same place — and the others stay in the trash.
pub fn restore_items(sandbox: &Sandbox, items: &[TrashedItem]) -> Result<Restored, TrashError> {
    restore_newest(sandbox, items.iter().map(|i| i.item.clone()).collect())
}

/// Restore `items`, keeping only the most recently deleted one per original
/// path (`restore_all` rejects such twins outright). Every destination is
/// validated against the sandbox first.
fn restore_newest(sandbox: &Sandbox, items: Vec<trash::TrashItem>) -> Result<Restored, TrashError> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::collections::HashMap;
        let total = items.len();
        let mut newest: HashMap<PathBuf, trash::TrashItem> = HashMap::new();
        for item in items {
            let original = item.original_path();
            match newest.get(&original) {
                Some(seen) if seen.time_deleted >= item.time_deleted => {}
                _ => {
                    newest.insert(original, item);
                }
            }
        }
        for original in newest.keys() {
            sandbox.resolve_for_create(original)?;
        }
        let restored = newest.len();
        if restored > 0 {
            trash::os_limited::restore_all(newest.into_values()).map_err(|e| TrashError::Backend(e.to_string()))?;
        }
        Ok(Restored { restored, kept: total - restored })
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        let _ = (sandbox, items);
        Ok(Restored { restored: 0, kept: 0 })
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
