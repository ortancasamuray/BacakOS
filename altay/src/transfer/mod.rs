// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Background transfer queue: cancellable, pausable copy/move operations with
//! live progress, built so the UI never blocks. Each transfer runs on its own
//! thread; the UI polls [`Manager::snapshots`] on a timer.
//!
//! Paths are validated through the [`Sandbox`](crate::security::Sandbox) before
//! any bytes move, exactly like the synchronous `filesystem::ops`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::filesystem;
use crate::security::Sandbox;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Paused,
    Done,
    Cancelled,
    Failed,
}

impl Status {
    fn code(self) -> u8 {
        match self {
            Status::Running => 0,
            Status::Paused => 1,
            Status::Done => 2,
            Status::Cancelled => 3,
            Status::Failed => 4,
        }
    }
    fn from_code(c: u8) -> Status {
        match c {
            1 => Status::Paused,
            2 => Status::Done,
            3 => Status::Cancelled,
            4 => Status::Failed,
            _ => Status::Running,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Cancelled | Status::Failed)
    }
}

/// Shared, thread-safe state for one transfer.
struct TransferState {
    id: u64,
    label: String,
    bytes_done: AtomicU64,
    bytes_total: AtomicU64,
    files_done: AtomicU64,
    files_total: AtomicU64,
    status: AtomicU8,
    cancel: AtomicBool,
    pause: Mutex<bool>,
    pause_cv: Condvar,
    error: Mutex<Option<String>>,
}

impl TransferState {
    fn set_status(&self, s: Status) {
        // Don't overwrite a terminal status.
        let cur = Status::from_code(self.status.load(Ordering::SeqCst));
        if !cur.is_terminal() {
            self.status.store(s.code(), Ordering::SeqCst);
        }
    }

    /// Block while paused; returns `false` if the transfer was cancelled.
    fn checkpoint(&self) -> bool {
        if self.cancel.load(Ordering::SeqCst) {
            return false;
        }
        let mut paused = self.pause.lock().unwrap();
        while *paused {
            self.set_status(Status::Paused);
            paused = self.pause_cv.wait(paused).unwrap();
            if self.cancel.load(Ordering::SeqCst) {
                return false;
            }
        }
        self.set_status(Status::Running);
        true
    }
}

/// An immutable view of a transfer for the UI.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: u64,
    pub label: String,
    pub status: Status,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    pub error: Option<String>,
}

impl Snapshot {
    /// Fraction complete in `0.0..=1.0` (by bytes, falling back to files).
    pub fn fraction(&self) -> f32 {
        if self.bytes_total > 0 {
            (self.bytes_done as f64 / self.bytes_total as f64) as f32
        } else if self.files_total > 0 {
            (self.files_done as f64 / self.files_total as f64) as f32
        } else {
            0.0
        }
    }
}

/// Owns all transfers. Cheap to clone (shared inner).
#[derive(Clone, Default)]
pub struct Manager {
    inner: Arc<Mutex<Vec<Arc<TransferState>>>>,
    next_id: Arc<AtomicU64>,
}

impl Manager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a copy/move of `sources` into `dest_dir`. Spawns a worker thread
    /// and returns the new transfer's id immediately.
    pub fn enqueue(
        &self,
        sandbox: &Sandbox,
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
        kind: Kind,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let label = match sources.first().and_then(|p| p.file_name()) {
            Some(name) if sources.len() == 1 => name.to_string_lossy().into_owned(),
            _ => format!("{} items", sources.len()),
        };
        let state = Arc::new(TransferState {
            id,
            label,
            bytes_done: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            files_done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            status: AtomicU8::new(Status::Running.code()),
            cancel: AtomicBool::new(false),
            pause: Mutex::new(false),
            pause_cv: Condvar::new(),
            error: Mutex::new(None),
        });
        self.inner.lock().unwrap().push(state.clone());

        let sandbox = sandbox.clone();
        std::thread::Builder::new()
            .name(format!("transfer-{id}"))
            .spawn(move || run_transfer(&sandbox, &state, &sources, &dest_dir, kind))
            .ok();
        id
    }

    pub fn pause(&self, id: u64) {
        if let Some(s) = self.find(id) {
            *s.pause.lock().unwrap() = true;
            s.set_status(Status::Paused);
        }
    }

    pub fn resume(&self, id: u64) {
        if let Some(s) = self.find(id) {
            *s.pause.lock().unwrap() = false;
            s.pause_cv.notify_all();
        }
    }

    pub fn cancel(&self, id: u64) {
        if let Some(s) = self.find(id) {
            s.cancel.store(true, Ordering::SeqCst);
            *s.pause.lock().unwrap() = false; // wake if paused so it can observe cancel
            s.pause_cv.notify_all();
        }
    }

    /// Drop finished transfers from the list (called when the user clears them).
    pub fn clear_finished(&self) {
        self.inner
            .lock()
            .unwrap()
            .retain(|s| !Status::from_code(s.status.load(Ordering::SeqCst)).is_terminal());
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|s| Snapshot {
                id: s.id,
                label: s.label.clone(),
                status: Status::from_code(s.status.load(Ordering::SeqCst)),
                bytes_done: s.bytes_done.load(Ordering::SeqCst),
                bytes_total: s.bytes_total.load(Ordering::SeqCst),
                files_done: s.files_done.load(Ordering::SeqCst),
                files_total: s.files_total.load(Ordering::SeqCst),
                error: s.error.lock().unwrap().clone(),
            })
            .collect()
    }

    fn find(&self, id: u64) -> Option<Arc<TransferState>> {
        self.inner.lock().unwrap().iter().find(|s| s.id == id).cloned()
    }
}

/// Worker body: validate, tally totals, then copy file-by-file (cancellable),
/// removing sources afterwards for a move.
fn run_transfer(
    sandbox: &Sandbox,
    state: &TransferState,
    sources: &[PathBuf],
    dest_dir: &Path,
    kind: Kind,
) {
    let mut total_bytes = 0u64;
    let mut total_files = 0u64;
    for src in sources {
        let (b, f) = tree_size(src);
        total_bytes += b;
        total_files += f;
    }
    state.bytes_total.store(total_bytes, Ordering::SeqCst);
    state.files_total.store(total_files, Ordering::SeqCst);

    for src in sources {
        let Some(name) = src.file_name() else { continue };
        let dst = dest_dir.join(name);
        // Re-validate every pair against the sandbox before touching disk.
        let validated = filesystem::validate_pair(sandbox, src, &dst);
        let (src_ok, dst_ok) = match validated {
            Ok((s, d)) => (s.into_path_buf(), d.into_path_buf()),
            Err(e) => {
                fail(state, e.to_string());
                return;
            }
        };

        if kind == Kind::Move {
            // Try a fast rename first (same filesystem, instantaneous).
            if std::fs::rename(&src_ok, &dst_ok).is_ok() {
                bump(state, tree_size(&dst_ok).0, tree_size_files(&dst_ok));
                continue;
            }
        }
        if !copy_tree(state, &src_ok, &dst_ok) {
            return; // cancelled or failed (status already set)
        }
        if kind == Kind::Move {
            let _ = remove_tree(&src_ok);
        }
    }

    state.set_status(Status::Done);
}

/// Recursively copy, honouring pause/cancel between files. Returns `false` if
/// the transfer should stop (cancelled or errored).
fn copy_tree(state: &TransferState, src: &Path, dst: &Path) -> bool {
    let meta = match std::fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) => {
            fail(state, e.to_string());
            return false;
        }
    };
    if meta.is_dir() {
        if let Err(e) = std::fs::create_dir_all(dst) {
            fail(state, e.to_string());
            return false;
        }
        let entries = match std::fs::read_dir(src) {
            Ok(e) => e,
            Err(e) => {
                fail(state, e.to_string());
                return false;
            }
        };
        for entry in entries.flatten() {
            if !copy_tree(state, &entry.path(), &dst.join(entry.file_name())) {
                return false;
            }
        }
    } else {
        if !state.checkpoint() {
            state.set_status(Status::Cancelled);
            return false;
        }
        match std::fs::copy(src, dst) {
            Ok(bytes) => {
                state.bytes_done.fetch_add(bytes, Ordering::SeqCst);
                state.files_done.fetch_add(1, Ordering::SeqCst);
            }
            Err(e) => {
                fail(state, e.to_string());
                return false;
            }
        }
    }
    true
}

fn fail(state: &TransferState, msg: String) {
    *state.error.lock().unwrap() = Some(msg);
    state.set_status(Status::Failed);
}

fn bump(state: &TransferState, bytes: u64, files: u64) {
    state.bytes_done.fetch_add(bytes, Ordering::SeqCst);
    state.files_done.fetch_add(files, Ordering::SeqCst);
}

fn tree_size(path: &Path) -> (u64, u64) {
    let mut bytes = 0;
    let mut files = 0;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_dir() {
            if let Ok(rd) = std::fs::read_dir(path) {
                for e in rd.flatten() {
                    let (b, f) = tree_size(&e.path());
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

fn tree_size_files(path: &Path) -> u64 {
    tree_size(path).1
}

fn remove_tree(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AllowedRoot;

    fn wait_done(m: &Manager, id: u64) -> Snapshot {
        for _ in 0..2000 {
            let snap = m.snapshots().into_iter().find(|s| s.id == id).unwrap();
            if snap.status.is_terminal() {
                return snap;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("transfer {id} did not finish");
    }

    #[test]
    fn copy_transfer_completes() {
        let base = std::env::temp_dir().join(format!("altay-xfer-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join("src")).unwrap();
        std::fs::write(home.join("src/file.txt"), vec![b'x'; 4096]).unwrap();
        std::fs::create_dir_all(home.join("dest")).unwrap();
        let sandbox = Sandbox::with_roots(vec![AllowedRoot::home(std::fs::canonicalize(&home).unwrap())]);

        let m = Manager::new();
        let id = m.enqueue(&sandbox, vec![home.join("src")], home.join("dest"), Kind::Copy);
        let snap = wait_done(&m, id);
        assert_eq!(snap.status, Status::Done, "error: {:?}", snap.error);
        assert!(home.join("dest/src/file.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn rejects_destination_outside_sandbox() {
        let base = std::env::temp_dir().join(format!("altay-xfer2-{}", std::process::id()));
        let home = base.join("home");
        std::fs::create_dir_all(home.join("src")).unwrap();
        std::fs::write(home.join("src/f"), b"z").unwrap();
        let sandbox = Sandbox::with_roots(vec![AllowedRoot::home(std::fs::canonicalize(&home).unwrap())]);
        let m = Manager::new();
        // Destination /tmp/... is outside the (home-only) sandbox.
        let id = m.enqueue(&sandbox, vec![home.join("src")], base.join("outside"), Kind::Copy);
        let snap = wait_done(&m, id);
        assert_eq!(snap.status, Status::Failed);
        let _ = std::fs::remove_dir_all(&base);
    }
}
