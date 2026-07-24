// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Locating the squashfs image(s) `kur` itself is running from.
//!
//! `live-boot` mounts the boot device at `/run/live/medium` and stacks every
//! `*.squashfs` found under its `live/` directory into the union that becomes
//! the running `/`. [`super::stages::extract_squashfs`] unpacks that same
//! stack onto the target disk instead of rebuilding it from scratch with
//! `debootstrap`.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::cmd;

/// Where `live-boot` mounts the medium the system booted from.
const LIVE_DIR: &str = "/run/live/medium/live";

/// Every squashfs layer to unpack, in union order (alphabetical, matching
/// `live-boot`'s own stacking order).
///
/// A dry run never boots from a real medium, so it reports a single
/// placeholder path instead of globbing a directory that does not exist —
/// same trick as `stages::uuid_of`.
pub fn find_squashfs_layers() -> Result<Vec<PathBuf>> {
    if cmd::is_dry_run() {
        return Ok(vec![PathBuf::from("/dry-run/filesystem.squashfs")]);
    }

    let mut layers: Vec<PathBuf> = std::fs::read_dir(LIVE_DIR)
        .with_context(|| format!("{LIVE_DIR} okunamadı"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "squashfs"))
        .collect();

    if layers.is_empty() {
        bail!("{LIVE_DIR} altında squashfs bulunamadı — canlı ortamdan mı çalıştırılıyor?");
    }

    layers.sort();
    Ok(layers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_yields_a_placeholder_layer() {
        std::env::set_var("KUR_DRY_RUN", "1");
        let layers = find_squashfs_layers().expect("dry run never touches the filesystem");
        assert_eq!(layers, vec![PathBuf::from("/dry-run/filesystem.squashfs")]);
    }
}
