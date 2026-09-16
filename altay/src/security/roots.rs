// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Discovery of the directories the sandbox permits, and how to label them.

use std::path::{Path, PathBuf};

/// What kind of place a root is — drives the sidebar icon & grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Home,
    Removable,
    Network,
}

/// A permitted top-level location.
#[derive(Debug, Clone)]
pub struct AllowedRoot {
    path: PathBuf,
    label: String,
    kind: RootKind,
}

impl AllowedRoot {
    pub fn home(path: PathBuf) -> Self {
        Self { path, label: "Home".into(), kind: RootKind::Home }
    }

    pub fn new(path: PathBuf, label: impl Into<String>, kind: RootKind) -> Self {
        Self { path, label: label.into(), kind }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn label(&self) -> &str {
        &self.label
    }
    pub fn kind(&self) -> RootKind {
        self.kind
    }
    pub fn is_home(&self) -> bool {
        self.kind == RootKind::Home
    }
}

/// Build the live list of permitted roots from the environment.
pub fn discover() -> Vec<AllowedRoot> {
    let mut roots = Vec::new();

    if let Some(home) = dirs::home_dir() {
        if let Ok(canon) = std::fs::canonicalize(&home) {
            roots.push(AllowedRoot::home(canon));
        } else {
            roots.push(AllowedRoot::home(home));
        }
    }

    let user = std::env::var("USER").ok();

    // Removable / external mounts. These directories are where udisks2 mounts
    // USB sticks, SD cards and external disks for the logged-in user.
    let mut mount_bases: Vec<PathBuf> = vec![PathBuf::from("/mnt")];
    if let Some(u) = &user {
        mount_bases.push(PathBuf::from("/media").join(u));
        mount_bases.push(PathBuf::from("/run/media").join(u));
    }
    mount_bases.push(PathBuf::from("/media"));

    for base in mount_bases {
        for mount in list_subdirs(&base) {
            let label = mount
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| mount.display().to_string());
            roots.push(AllowedRoot::new(mount, label, RootKind::Removable));
        }
    }

    // Network mounts exposed by gvfs (SMB/SFTP/FTP/WebDAV via the portal stack).
    if let Some(runtime) = dirs::runtime_dir() {
        let gvfs = runtime.join("gvfs");
        for mount in list_subdirs(&gvfs) {
            let label = gvfs_label(&mount);
            roots.push(AllowedRoot::new(mount, label, RootKind::Network));
        }
    }

    roots
}

/// List immediate sub-directories of `base`, canonicalised. Silently skips a
/// base that does not exist or is unreadable (the common case).
fn list_subdirs(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return out;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.push(std::fs::canonicalize(&p).unwrap_or(p));
        }
    }
    out
}

/// gvfs mount directory names look like `smb-share:server=x,share=y` — turn that
/// into something a human wants to read.
fn gvfs_label(mount: &Path) -> String {
    let raw = mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some((scheme, rest)) = raw.split_once(':') {
        let detail = rest
            .split(',')
            .filter_map(|kv| kv.split_once('=').map(|(_, v)| v))
            .collect::<Vec<_>>()
            .join(" / ");
        let proto = scheme.trim_end_matches("-share").to_uppercase();
        if detail.is_empty() {
            proto
        } else {
            format!("{detail} ({proto})")
        }
    } else {
        raw
    }
}
