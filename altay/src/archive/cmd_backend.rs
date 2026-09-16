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

use super::{ArchiveError, Backend, Member, Options};

pub struct CmdBackend;

const TOOL: &str = "7z";
const MISSING: &str = "7z bulunamadı — `sudo apt install p7zip-full` komutuyla kurabilirsiniz";

impl Backend for CmdBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
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

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
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

/// Parse the machine-readable `7z l -slt` output into Members.
/// Each file block starts with a blank line; fields are `Key = Value` lines.
fn parse_7z_list(raw: &str) -> Vec<Member> {
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut size: u64 = 0;
    let mut compressed: u64 = 0;
    let mut is_dir = false;
    let mut encrypted = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            if let Some(p) = path.take() {
                out.push(Member { path: p, is_dir, size, compressed_size: compressed, encrypted });
            }
            size = 0; compressed = 0; is_dir = false; encrypted = false;
            continue;
        }
        if let Some((key, val)) = line.split_once(" = ") {
            match key.trim() {
                "Path"       => path = Some(PathBuf::from(val.trim())),
                "Size"       => size = val.trim().parse().unwrap_or(0),
                "Packed Size"=> compressed = val.trim().parse().unwrap_or(0),
                "Attributes" => is_dir = val.contains('D'),
                "Encrypted"  => encrypted = val.trim() == "+",
                _ => {}
            }
        }
    }
    // flush last block
    if let Some(p) = path {
        out.push(Member { path: p, is_dir, size, compressed_size: compressed, encrypted });
    }
    out
}
