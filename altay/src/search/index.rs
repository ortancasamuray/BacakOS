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
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};

use notify::{RecursiveMode, Watcher};

use crate::filesystem::Entry;
use crate::security::Sandbox;

use super::{entry_from_path, Query};

/// How long the background thread waits after loading a cached index before
/// starting the watcher and the refresh walk (see [`Index::build`]).
const REFRESH_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// A shared, clonable handle to the index for one root.
#[derive(Clone)]
pub struct Index {
    root: PathBuf,
    map: Arc<RwLock<HashMap<PathBuf, Entry>>>,
    ready: Arc<AtomicBool>,
    // Keeps the filesystem watcher alive for the index's lifetime.
    _watcher: WatcherSlot,
}

impl Index {
    /// Build an index for `root` (validated against the sandbox). The initial
    /// walk and the watcher both run in the background; the returned handle is
    /// usable immediately and fills in as the walk progresses.
    pub fn build(sandbox: &Sandbox, root: impl AsRef<Path>) -> Option<Index> {
        let root = sandbox.resolve(root).ok()?.into_path_buf();
        let map: Arc<RwLock<HashMap<PathBuf, Entry>>> = Arc::new(RwLock::new(HashMap::new()));
        let ready = Arc::new(AtomicBool::new(false));
        let watcher_slot: WatcherSlot = Arc::new(Mutex::new(None));

        // Background thread: load cache first for instant search, then the
        // watcher, then a full walk. Everything here stays off the UI thread —
        // a recursive inotify watch adds one watch per directory under $HOME
        // and used to block the window from appearing for many seconds.
        {
            let root = root.clone();
            let map = map.clone();
            let ready = ready.clone();
            let watcher_slot = watcher_slot.clone();
            std::thread::Builder::new()
                .name("search-index-build".into())
                .spawn(move || {
                    // Load persisted cache so search is available without a full walk.
                    let cached = load_map(&root);
                    let had_cache = cached.is_some();
                    if let Some(loaded) = cached {
                        log::info!("search index loaded from cache for {} ({} entries)", root.display(), loaded.len());
                        *map.write().unwrap() = loaded;
                        ready.store(true, Ordering::SeqCst);
                        // Search already works off the cache; let the window's
                        // first directory listing have the disk before the
                        // watcher setup and full re-walk start thrashing it.
                        std::thread::sleep(REFRESH_DELAY);
                    }
                    // Filesystem watcher for incremental updates. Directories
                    // are added one by one during the walk below.
                    *watcher_slot.lock().unwrap() =
                        start_watcher(&root, map.clone(), watcher_slot.clone());
                    // Full walk to refresh the stale cache.
                    let mut local = HashMap::new();
                    for entry in walkdir::WalkDir::new(&root).follow_links(false).into_iter().flatten() {
                        if entry.file_type().is_dir() && should_watch(&root, entry.path()) {
                            if let Some(w) = watcher_slot.lock().unwrap().as_mut() {
                                w.add_dir(entry.path());
                            }
                        }
                        if let Some(e) = entry_from_path(entry.path()) {
                            local.insert(entry.path().to_path_buf(), e);
                        }
                    }
                    if let Some(w) = watcher_slot.lock().unwrap().as_ref() {
                        log::info!("search index watching {} directories under {}", w.watched.len(), root.display());
                    }
                    save_map(&root, &local);
                    let n = local.len();
                    *map.write().unwrap() = local;
                    ready.store(true, Ordering::SeqCst);
                    log::info!(
                        "search index rebuilt for {} ({n} entries, cache={had_cache})",
                        root.display()
                    );
                })
                .ok();
        }

        Some(Index { root, map, ready, _watcher: watcher_slot })
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

type WatcherSlot = Arc<Mutex<Option<DirWatcher>>>;

/// Directory names never watched live (still indexed by the launch walk):
/// build output and caches churn constantly and would eat the inotify budget.
const UNWATCHED_DIRS: &[&str] = &["target", "node_modules", "__pycache__"];

/// A filesystem watcher that tracks directories individually instead of using
/// `RecursiveMode::Recursive`. notify's recursive inotify mode aborts the whole
/// watch when a single subdirectory is unreadable (e.g. a root-owned
/// `.../private` dir in a build cache), and it would also try to watch every
/// directory under `$HOME` — easily more than `max_user_watches`, starving
/// every other inotify user in the session.
struct DirWatcher {
    watcher: notify::RecommendedWatcher,
    watched: HashSet<PathBuf>,
    budget: usize,
    budget_logged: bool,
}

impl DirWatcher {
    /// Watch `dir` (non-recursively). Unreadable dirs are skipped silently;
    /// once the budget is spent further dirs are only covered by the next
    /// launch's walk.
    fn add_dir(&mut self, dir: &Path) {
        if self.watched.contains(dir) {
            return;
        }
        if self.watched.len() >= self.budget {
            if !self.budget_logged {
                self.budget_logged = true;
                log::info!("search index watch budget ({}) reached; remaining dirs refresh on next launch", self.budget);
            }
            return;
        }
        match self.watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                self.watched.insert(dir.to_path_buf());
            }
            Err(e) => log::debug!("search index: cannot watch {}: {e}", dir.display()),
        }
    }
}

/// Up to a quarter of the per-user inotify watch limit, leaving the rest for
/// the compositor and other apps.
fn watch_budget() -> usize {
    std::fs::read_to_string("/proc/sys/fs/inotify/max_user_watches")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .map(|max| (max / 4).max(1024))
        .unwrap_or(8192)
}

/// Whether `dir` (under `root`) should get a live watch: not inside a hidden
/// directory or one of [`UNWATCHED_DIRS`].
fn should_watch(root: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir.strip_prefix(root) else { return false };
    rel.components().all(|c| match c {
        std::path::Component::Normal(name) => {
            let name = name.to_string_lossy();
            !name.starts_with('.') && !UNWATCHED_DIRS.contains(&name.as_ref())
        }
        _ => true,
    })
}

enum DirEvent {
    Added(PathBuf),
    Removed(PathBuf),
}

/// Create the watcher that incrementally maintains `map`. Directories created
/// later are handed to a helper thread that watches them and indexes whatever
/// landed inside before the watch existed. (`watch()` can't be called from the
/// event callback itself: it round-trips through the event-loop thread the
/// callback runs on.)
fn start_watcher(root: &Path, map: Arc<RwLock<HashMap<PathBuf, Entry>>>, slot: WatcherSlot) -> Option<DirWatcher> {
    let (tx, rx) = mpsc::channel::<DirEvent>();
    let cb_map = map.clone();
    let cb_root = root.to_path_buf();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        use notify::event::{ModifyKind, RemoveKind};
        use notify::EventKind::*;
        match event.kind {
            Create(_) | Modify(_) => {
                let mut m = cb_map.write().unwrap();
                for path in &event.paths {
                    if let Some(e) = entry_from_path(path) {
                        m.insert(path.clone(), e);
                    } else {
                        m.remove(path);
                    }
                }
                drop(m);
                if matches!(event.kind, Create(_) | Modify(ModifyKind::Name(_))) {
                    for path in &event.paths {
                        if path.is_dir() && should_watch(&cb_root, path) {
                            let _ = tx.send(DirEvent::Added(path.clone()));
                        }
                    }
                }
            }
            Remove(kind) => {
                let mut m = cb_map.write().unwrap();
                for path in &event.paths {
                    m.remove(path);
                }
                drop(m);
                if matches!(kind, RemoveKind::Folder | RemoveKind::Any) {
                    for path in &event.paths {
                        let _ = tx.send(DirEvent::Removed(path.clone()));
                    }
                }
            }
            _ => {}
        }
    })
    .ok()?;

    let root = root.to_path_buf();
    std::thread::Builder::new()
        .name("search-index-watch".into())
        .spawn(move || {
            for ev in rx {
                match ev {
                    DirEvent::Added(dir) => {
                        let mut found = Vec::new();
                        {
                            let mut guard = slot.lock().unwrap();
                            let Some(w) = guard.as_mut() else { continue };
                            let walk = walkdir::WalkDir::new(&dir).follow_links(false).into_iter();
                            for entry in walk.filter_entry(|e| !e.file_type().is_dir() || should_watch(&root, e.path())).flatten() {
                                if entry.file_type().is_dir() {
                                    w.add_dir(entry.path());
                                }
                                if let Some(e) = entry_from_path(entry.path()) {
                                    found.push(e);
                                }
                            }
                        }
                        // Map lock only after releasing the watcher (see above).
                        let mut m = map.write().unwrap();
                        for e in found {
                            m.insert(e.path.clone(), e);
                        }
                    }
                    DirEvent::Removed(dir) => {
                        if let Some(w) = slot.lock().unwrap().as_mut() {
                            w.watched.retain(|p| !p.starts_with(&dir));
                        }
                    }
                }
            }
        })
        .ok()?;

    Some(DirWatcher { watcher, watched: HashSet::new(), budget: watch_budget(), budget_logged: false })
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

    fn wait_for(idx: &Index, scope: &Path, name: &str) -> bool {
        for _ in 0..500 {
            let q = Query { name_contains: Some(name.into()), limit: 10, ..Default::default() };
            if idx.query(scope, &q).iter().any(|e| e.name == name) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        false
    }

    #[test]
    fn unreadable_subdir_does_not_disable_watching() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("altay-idxperm-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join("locked/inner")).unwrap();
        std::fs::create_dir_all(home.join("docs")).unwrap();
        std::fs::set_permissions(home.join("locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        let idx = Index::build(&sb, &canon).unwrap();
        for _ in 0..500 {
            if idx.is_ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::fs::write(canon.join("docs/after.txt"), b"x").unwrap();
        let ok = wait_for(&idx, &canon, "after.txt");
        std::fs::set_permissions(canon.join("locked"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&base);
        assert!(ok, "watcher died because of an unreadable sibling directory");
    }

    #[test]
    fn new_directory_is_watched() {
        let base = std::env::temp_dir().join(format!("altay-idxnewdir-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let canon = std::fs::canonicalize(&home).unwrap();
        let sb = Sandbox::with_roots(vec![AllowedRoot::home(canon.clone())]);

        let idx = Index::build(&sb, &canon).unwrap();
        for _ in 0..500 {
            if idx.is_ready() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::fs::create_dir_all(canon.join("fresh")).unwrap();
        assert!(wait_for(&idx, &canon, "fresh"), "new dir not indexed");
        // Give the helper thread a moment to attach the watch, then create a
        // file inside — it must arrive via the watcher.
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(canon.join("fresh/late.txt"), b"y").unwrap();
        let ok = wait_for(&idx, &canon, "late.txt");
        let _ = std::fs::remove_dir_all(&base);
        assert!(ok, "file inside a newly created dir was not picked up");
    }

    #[test]
    fn should_watch_skips_hidden_and_build_dirs() {
        let root = Path::new("/home/u");
        assert!(should_watch(root, Path::new("/home/u")));
        assert!(should_watch(root, Path::new("/home/u/Belgeler/proje")));
        assert!(!should_watch(root, Path::new("/home/u/.cache/x")));
        assert!(!should_watch(root, Path::new("/home/u/kod/target/release")));
        assert!(!should_watch(root, Path::new("/home/u/web/node_modules")));
        assert!(!should_watch(Path::new("/home/u"), Path::new("/etc")));
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
