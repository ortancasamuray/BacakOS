// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! RAR backend (read/extract) via the `unrar` crate, which wraps the official
//! UnRAR library. Handles RAR4, RAR5 and multi-volume sets natively. Creation
//! is not supported (RAR is a proprietary compressor — read-only by design).

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{ArchiveError, Backend, Member, Options};

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
                compressed_size: header.unpacked_size,
                encrypted: header.is_encrypted(),
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
            open = if is_dir {
                header.skip().map_err(rar_err)?
            } else {
                let next = header.extract_with_base(into).map_err(rar_err)?;
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

fn rar_err(e: unrar::error::UnrarError) -> ArchiveError {
    ArchiveError::Backend(format!("{e}"))
}
