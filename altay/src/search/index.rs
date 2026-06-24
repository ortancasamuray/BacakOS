// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! A live, in-memory filename index for fast search.
//!
//! On creation it walks a sandbox root on a background thread, then keeps itself
//! fresh with `notify` filesystem events (incremental add/remove). Queries scan
//! the in-memory map — no disk walk per keystroke — so search stays instant
//! even on large trees. (On-disk persistence is a possible refinement; rebuild
//! on launch is fast and avoids stale-cache bugs.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use notify::{RecursiveMode, Watcher};

use crate::filesystem::Entry;
use crate::security::Sandbox;

use super::{entry_from_path, Query};

/// A shared, clonable handle to the index for one root.
#[derive(Clone)]
pub struct Index {
    root: PathBuf,
    map: Arc<RwLock<HashMap<PathBuf, Entry>>>,
    ready: Arc<AtomicBool>,
    // Keeps the filesystem watcher alive for the index's lifetime.
    _watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

impl Index {
    /// Build an index for `root` (validated against the sandbox). The initial
    /// walk and the watcher both run in the background; the returned handle is
    /// usable immediately and fills in as the walk progresses.
    pub fn build(sandbox: &Sandbox, root: impl AsRef<Path>) -> Option<Index> {
        let root = sandbox.resolve(root).ok()?.into_path_buf();
        let map: Arc<RwLock<HashMap<PathBuf, Entry>>> = Arc::new(RwLock::new(HashMap::new()));
        let ready = Arc::new(AtomicBool::new(false));

        // Instant start: load the persisted index from a previous run, if any.
        if let Some(loaded) = load_map(&root) {
            log::info!("search index loaded from cache for {} ({} entries)", root.display(), loaded.len());
            *map.write().unwrap() = loaded;
            ready.store(true, Ordering::SeqCst);
        }

        // Background full walk to refresh the (possibly stale) cache and persist.
        {
            let root = root.clone();
            let map = map.clone();
            let ready = ready.clone();
            std::thread::Builder::new()
                .name("search-index-build".into())
                .spawn(move || {
                    let mut local = HashMap::new();
                    for entry in walkdir::WalkDir::new(&root).follow_links(false).into_iter().flatten() {
                        if let Some(e) = entry_from_path(entry.path()) {
                            local.insert(entry.path().to_path_buf(), e);
                        }
                    }
                    save_map(&root, &local);
                    let n = local.len();
                    *map.write().unwrap() = local;
                    ready.store(true, Ordering::SeqCst);
                    log::info!("search index rebuilt for {} ({n} entries)", root.display());
                })
                .ok();
        }

        // Filesystem watcher for incremental updates.
        let watcher = start_watcher(&root, map.clone());

        Some(Index {
            root,
            map,
            ready,
            _watcher: Arc::new(Mutex::new(watcher)),
        })
    }

    /// Whether `path` falls within this index's root.
    pub fn covers(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Run a query against the index, restricted to entries under `scope`
    /// (which must be inside the root). Honours `query.limit`.
    pub fn query(&self, scope: &Path, query: &Query) -> Vec<Entry> {
        let limit = if query.limit == 0 { usize::MAX } else { query.limit };
        let map = self.map.read().unwrap();
        let mut hits: Vec<Entry> = map
            .values()
            .filter(|e| e.path.starts_with(scope))
            .filter(|e| query.include_hidden || !is_hidden(&e.path))
            .filter(|e| query.matches(e))
            .take(limit)
            .cloned()
            .collect();
        // Stable, useful ordering: directories first, then by name.
        crate::filesystem::sort_entries(&mut hits, crate::filesystem::SortKey::Name);
        hits
    }
}

/// Spawn a recursive watcher that incrementally maintains `map`.
fn start_watcher(
    root: &Path,
    map: Arc<RwLock<HashMap<PathBuf, Entry>>>,
) -> Option<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        use notify::EventKind::*;
        match event.kind {
            Create(_) | Modify(_) => {
                let mut m = map.write().unwrap();
                for path in &event.paths {
                    if let Some(e) = entry_from_path(path) {
                        m.insert(path.clone(), e);
                    } else {
                        m.remove(path);
                    }
                }
            }
            Remove(_) => {
                let mut m = map.write().unwrap();
                for path in &event.paths {
                    m.remove(path);
                }
            }
            _ => {}
        }
    })
    .ok()?;
    if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
        log::info!("search index watcher unavailable for {}: {e}", root.display());
        return None;
    }
    Some(watcher)
}

fn is_hidden(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(c, std::path::Component::Normal(name) if name.to_string_lossy().starts_with('.'))
    })
}

// ---- On-disk persistence ----------------------------------------------------
//
// The index is cached so the next launch can search instantly without a full
// re-walk. Format is a compact length-prefixed binary record stream (no serde
// dependency, robust against any byte in a path):
//   [u32 path_len][path bytes][u64 size][i64 mtime_secs][u8 flags]
// flags: bit0 = is_dir, bit1 = is_symlink. mtime_secs 0 == unknown.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn cache_file(root: &Path) -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("altay");
    let _ = std::fs::create_dir_all(&dir);
    let digest = format!("{:x}", md5::compute(root.to_string_lossy().as_bytes()));
    Some(dir.join(format!("index-{digest}.idx")))
}

fn save_map(root: &Path, map: &HashMap<PathBuf, Entry>) {
    let Some(path) = cache_file(root) else { return };
    use std::os::unix::ffi::OsStrExt;
    let mut buf: Vec<u8> = Vec::with_capacity(map.len() * 64);
    for e in map.values() {
        let bytes = e.path.as_os_str().as_bytes();
        buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(bytes);
        buf.extend_from_slice(&e.size.to_le_bytes());
        let mtime = e
            .modified
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        buf.extend_from_slice(&mtime.to_le_bytes());
        buf.push((e.is_dir as u8) | ((e.is_symlink as u8) << 1));
    }
    if let Err(e) = std::fs::write(&path, &buf) {
        log::info!("could not persist search index: {e}");
    }
}

fn load_map(root: &Path) -> Option<HashMap<PathBuf, Entry>> {
    let data = std::fs::read(cache_file(root)?).ok()?;
    let mut map = HashMap::new();
    let mut i = 0usize;
    let need = |i: usize, n: usize, len: usize| i + n <= len;
    while need(i, 4, data.len()) {
        let plen = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        i += 4;
        if !need(i, plen + 8 + 8 + 1, data.len()) {
            break; // truncated/corrupt — stop, keep what we parsed
        }
        let path = path_from_bytes(&data[i..i + plen]);
        i += plen;
        let size = u64::from_le_bytes(data[i..i + 8].try_into().ok()?);
        i += 8;
        let mtime = i64::from_le_bytes(data[i..i + 8].try_into().ok()?);
        i += 8;
        let flags = data[i];
        i += 1;
        let is_dir = flags & 1 != 0;
        let is_symlink = flags & 2 != 0;
        let extension = if is_dir {
            String::new()
        } else {
            path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let modified = (mtime > 0).then(|| UNIX_EPOCH + Duration::from_secs(mtime as u64));
        map.insert(
            path.clone(),
            Entry { path, name, is_dir, is_symlink, size, modified, extension },
        );
    }
    Some(map)
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    #[test]
    fn index_finds_files_and_tracks_creation() {
        let base = std::env::temp_dir().join(format!("altay-idx-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join("docs")).unwrap();
        std::fs::write(home.join("docs/report.pdf"), b"x").unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        let idx = Index::build(&sb, &canon).unwrap();
        // Wait for the initial walk.
        for _ in 0..500 {
            if idx.is_ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(idx.is_ready());

        let q = Query { name_contains: Some("report".into()), limit: 10, ..Default::default() };
        let hits = idx.query(&canon, &q);
        assert!(hits.iter().any(|e| e.name == "report.pdf"), "report.pdf not indexed");

        // A newly created file should appear via the watcher (best-effort timing).
        std::fs::write(canon.join("docs/notes.txt"), b"y").unwrap();
        let mut found = false;
        for _ in 0..500 {
            let q2 = Query { name_contains: Some("notes".into()), limit: 10, ..Default::default() };
            if idx.query(&canon, &q2).iter().any(|e| e.name == "notes.txt") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        assert!(found, "watcher did not pick up the new file");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn persisted_index_roundtrips() {
        let root = std::env::temp_dir().join(format!("altay-idxpersist-{}", std::process::id()));
        let _ = std::fs::remove_file(cache_file(&root).unwrap());
        let mut map = HashMap::new();
        let p = root.join("docs/a b.txt"); // space to exercise byte-exact paths
        map.insert(
            p.clone(),
            Entry {
                path: p.clone(),
                name: "a b.txt".into(),
                is_dir: false,
                is_symlink: false,
                size: 1234,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_000_000)),
                extension: "txt".into(),
            },
        );
        save_map(&root, &map);
        let loaded = load_map(&root).expect("cache file present");
        let got = loaded.get(&p).expect("entry round-tripped");
        assert_eq!(got.size, 1234);
        assert_eq!(got.extension, "txt");
        assert_eq!(got.name, "a b.txt");
        let _ = std::fs::remove_file(cache_file(&root).unwrap());
    }
}
