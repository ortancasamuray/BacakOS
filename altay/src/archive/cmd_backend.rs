// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Command-based backend: delegates to the `7z` (p7zip-full) binary.
//! Handles legacy container formats — ACE, ALZ, ARJ, LZH/LHA, ZOO, CBR, CAB,
//! ISO, CPIO — that have no maintained pure-Rust crate.
//!
//! List and extract work if `7z` is on PATH; create always returns Unsupported.
//! If `7z` is missing the error message tells the user to install p7zip-full.

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{check_listing, ArchiveError, Backend, Member, Options};

pub struct CmdBackend;

const TOOL: &str = "7z";
const MISSING: &str = "7z bulunamadı — `sudo apt install p7zip-full` komutuyla kurabilirsiniz";

impl Backend for CmdBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        Ok(list_7z(archive)?.into_iter().map(|l| l.member).collect())
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        // 7z writes the members itself, so its own listing is checked first:
        // older p7zip releases follow `../` names and links (see `safe_join`).
        let listed: Vec<(PathBuf, bool)> = list_7z(archive)?.into_iter().map(|l| (l.member.path, l.link)).collect();
        check_listing(into, &listed)?;
        std::fs::create_dir_all(into)?;
        let status = std::process::Command::new(TOOL)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", into.display()))
            .arg("--")
            .arg(archive)
            .status()
            .map_err(|_| ArchiveError::Backend(MISSING.into()))?;
        if !status.success() {
            return Err(ArchiveError::Backend("7z çıkarma işlemi başarısız".into()));
        }
        on_progress(Progress { bytes_done: 0, bytes_total: 0, files_done: 1, files_total: 1 });
        Ok(())
    }

    fn create(
        &self,
        _archive: &Path,
        _sources: &[PathBuf],
        _opts: &Options,
        _on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        Err(ArchiveError::Unsupported)
    }
}

/// A member as `7z l -slt` reports it.
struct Listed {
    member: Member,
    /// A symbolic or hard link.
    link: bool,
}

fn list_7z(archive: &Path) -> Result<Vec<Listed>, ArchiveError> {
    let out = std::process::Command::new(TOOL)
        .args(["l", "-slt", "--"])
        .arg(archive)
        .output()
        .map_err(|_| ArchiveError::Backend(MISSING.into()))?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(ArchiveError::Backend(if msg.is_empty() { "7z başarısız".into() } else { msg }));
    }
    Ok(parse_7z_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse the machine-readable `7z l -slt` output. The archive's own block
/// comes first and ends at the `----------` line; then each member block
/// starts with a blank line; fields are `Key = Value` lines.
fn parse_7z_list(raw: &str) -> Vec<Listed> {
    let raw = raw.split_once("\n----------").map_or(raw, |(_, members)| members);
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut size: u64 = 0;
    let mut is_dir = false;
    let mut link = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            if let Some(p) = path.take() {
                out.push(Listed { member: Member { path: p, is_dir, size }, link });
            }
            size = 0; is_dir = false; link = false;
            continue;
        }
        if let Some((key, val)) = line.split_once(" =") {
            let val = val.trim();
            match key.trim() {
                "Path"       => path = Some(PathBuf::from(val)),
                "Size"       => size = val.parse().unwrap_or(0),
                "Attributes" => is_dir |= val.contains('D'),
                "Folder"     => is_dir |= val == "+",
                "Symbolic Link" | "Hard Link" => link |= !val.is_empty(),
                _ => {}
            }
        }
    }
    // flush last block
    if let Some(p) = path {
        out.push(Listed { member: Member { path: p, is_dir, size }, link });
    }
    out
}
