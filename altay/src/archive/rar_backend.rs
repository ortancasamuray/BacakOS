// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! RAR backend (read/extract) via the `unrar` crate, which wraps the official
//! UnRAR library. Handles RAR4, RAR5 and multi-volume sets natively. Creation
//! is not supported (RAR is a proprietary compressor — read-only by design).

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{create_member_dir, prepare_member_path, safe_join, ArchiveError, Backend, Member, Options};

pub struct RarBackend;

impl Backend for RarBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let listing = unrar::Archive::new(archive)
            .open_for_listing()
            .map_err(rar_err)?;
        let mut out = Vec::new();
        for entry in listing {
            let header = entry.map_err(rar_err)?;
            out.push(Member {
                path: header.filename.clone(),
                is_dir: header.is_directory(),
                size: header.unpacked_size,
            });
        }
        Ok(out)
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        // The UnRAR processing state machine follows multi-volume sets on its own.
        let mut open = unrar::Archive::new(archive)
            .open_for_processing()
            .map_err(rar_err)?;
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        while let Some(header) = open.read_header().map_err(rar_err)? {
            let entry = header.entry();
            let is_dir = entry.is_directory();
            let size = entry.unpacked_size;
            // UnRAR writes the member itself, so pick its exact path here
            // (`extract_with_base` would join the stored name as it is).
            let out_path = safe_join(into, &entry.filename)?;
            open = if is_dir {
                create_member_dir(into, &out_path)?;
                header.skip().map_err(rar_err)?
            } else {
                prepare_member_path(into, &out_path, is_symlink(entry))?;
                let next = header.extract_to(&out_path).map_err(rar_err)?;
                state.bytes_done = state.bytes_done.saturating_add(size);
                state.files_done += 1;
                on_progress(state);
                next
            };
        }
        Ok(())
    }

    fn create(
        &self,
        _archive: &Path,
        _sources: &[PathBuf],
        _opts: &Options,
        _on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        Err(ArchiveError::Unsupported) // RAR creation is proprietary / unsupported
    }
}

/// Whether a member may be a symbolic link: unix mode bits for archives made
/// on Unix, the reparse-point attribute for Windows ones. The header doesn't
/// say which, so this errs towards a link; a false match only refuses a
/// member whose path is already taken.
fn is_symlink(entry: &unrar::FileHeader) -> bool {
    const S_IFMT: u32 = 0o170000;
    const S_IFLNK: u32 = 0o120000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    entry.file_attr & S_IFMT == S_IFLNK || entry.file_attr & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn rar_err(e: unrar::error::UnrarError) -> ArchiveError {
    ArchiveError::Backend(format!("{e}"))
}
