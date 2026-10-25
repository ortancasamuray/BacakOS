// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Live refresh for the directory on screen.
//!
//! Watches a single directory non-recursively and calls back once a burst of
//! changes has settled, so files added, removed or renamed by another app (or
//! another Altay window) show up without pressing F5. A bulk copy produces
//! hundreds of events; they are coalesced into one callback.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};

/// Quiet period after the last event before the callback fires.
const SETTLE: Duration = Duration::from_millis(300);
/// Upper bound on how long a continuous stream of events can delay the
/// callback (a long copy still refreshes the view periodically).
const MAX_DELAY: Duration = Duration::from_millis(1500);

pub struct DirWatch {
    watcher: notify::RecommendedWatcher,
    dir: Option<PathBuf>,
}

impl DirWatch {
    /// Create a watcher that calls `on_change` (from a background thread) after
    /// the watched directory's contents change. Watches nothing until
    /// [`DirWatch::set_dir`] is called.
    pub fn new(on_change: impl Fn() + Send + 'static) -> Option<DirWatch> {
        let (tx, rx) = mpsc::channel::<()>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            // Reads (opening files for the preview, listing the directory
            // itself) must not trigger a refresh that reads it again.
            if !matches!(event.kind, EventKind::Access(_) | EventKind::Other) {
                let _ = tx.send(());
            }
        })
        .ok()?;

        std::thread::Builder::new()
            .name("dir-watch".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    let started = std::time::Instant::now();
                    loop {
                        let left = MAX_DELAY.saturating_sub(started.elapsed());
                        match rx.recv_timeout(SETTLE.min(left)) {
                            Ok(()) if !left.is_zero() => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            _ => break,
                        }
                    }
                    on_change();
                }
            })
            .ok()?;

        Some(DirWatch { watcher, dir: None })
    }

    /// Watch `dir` instead of the previous directory. No-op if unchanged.
    pub fn set_dir(&mut self, dir: &Path) {
        if self.dir.as_deref() == Some(dir) {
            return;
        }
        self.clear();
        match self.watcher.watch(dir, RecursiveMode::NonRecursive) {
            Ok(()) => self.dir = Some(dir.to_path_buf()),
            Err(e) => log::debug!("dir watch: cannot watch {}: {e}", dir.display()),
        }
    }

    /// Stop watching (the view no longer shows a plain directory listing).
    pub fn clear(&mut self) {
        if let Some(old) = self.dir.take() {
            let _ = self.watcher.unwatch(&old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> TmpDir {
            let p = std::env::temp_dir().join(format!("altay-watch-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for(count: &AtomicUsize, at_least: usize) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if count.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn burst_of_changes_fires_once() {
        let dir = TmpDir::new("burst");
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let mut w = DirWatch::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        w.set_dir(dir.path());
        for i in 0..50 {
            std::fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        }
        assert!(wait_for(&count, 1));
        std::thread::sleep(SETTLE * 3);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn removal_fires_and_reads_do_not() {
        let dir = TmpDir::new("remove");
        let file = dir.path().join("a");
        std::fs::write(&file, b"x").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let mut w = DirWatch::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        w.set_dir(dir.path());
        let _ = std::fs::read(&file).unwrap();
        let _ = std::fs::read_dir(dir.path()).unwrap().count();
        std::thread::sleep(SETTLE * 3);
        assert_eq!(count.load(Ordering::SeqCst), 0, "reads must not trigger a refresh");
        std::fs::remove_file(&file).unwrap();
        assert!(wait_for(&count, 1));
    }

    #[test]
    fn switching_dirs_stops_watching_the_old_one() {
        let a = TmpDir::new("a");
        let b = TmpDir::new("b");
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let mut w = DirWatch::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        w.set_dir(a.path());
        w.set_dir(b.path());
        std::fs::write(a.path().join("x"), b"x").unwrap();
        std::thread::sleep(SETTLE * 3);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        std::fs::write(b.path().join("y"), b"y").unwrap();
        assert!(wait_for(&count, 1));
    }
}
