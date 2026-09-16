// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! 7-Zip backend (read/extract + create) via the pure-Rust `sevenz-rust` crate.
//! Password-protected archives are supported for extraction.

use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{ArchiveError, Backend, Member, Options};

pub struct SevenZBackend;

impl Backend for SevenZBackend {
    fn list(&self, archive: &Path, opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let meta = sevenz_rust::Archive::open(archive).map_err(sz_err)?;
        let members = meta
            .files
            .iter()
            .map(|f| Member {
                path: PathBuf::from(f.name()),
                is_dir: f.is_directory(),
                size: f.size(),
                compressed_size: f.compressed_size,
                encrypted: opts.password.is_some(),
            })
            .collect();
        Ok(members)
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let result = if opts.password.is_some() {
            sevenz_rust::decompress_file_with_password(archive, into, password(opts))
        } else {
            sevenz_rust::decompress_file(archive, into)
        };
        result.map_err(sz_err)?;
        // sevenz-rust extracts in one shot; report completion.
        on_progress(Progress { bytes_done: 0, bytes_total: 0, files_done: 1, files_total: 1 });
        Ok(())
    }

    fn create(
        &self,
        archive: &Path,
        sources: &[PathBuf],
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let mut writer = sevenz_rust::SevenZWriter::create(archive).map_err(sz_err)?;
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        for src in sources {
            let base = src.parent().unwrap_or_else(|| Path::new(""));
            add_path(&mut writer, src, base, &mut state, on_progress)?;
        }
        writer.finish().map_err(ArchiveError::Io)?;
        Ok(())
    }
}

fn add_path(
    writer: &mut sevenz_rust::SevenZWriter<std::fs::File>,
    path: &Path,
    base: &Path,
    state: &mut Progress,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), ArchiveError> {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let name = rel.to_string_lossy().replace('\\', "/");
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            add_path(writer, &entry?.path(), base, state, on_progress)?;
        }
    } else {
        let entry = sevenz_rust::SevenZArchiveEntry::from_path(path, name);
        let file = std::fs::File::open(path)?;
        writer.push_archive_entry(entry, Some(file)).map_err(sz_err)?;
        state.files_done += 1;
        on_progress(*state);
    }
    Ok(())
}

fn password(opts: &Options) -> sevenz_rust::Password {
    match &opts.password {
        Some(p) => sevenz_rust::Password::from(p.as_str()),
        None => sevenz_rust::Password::empty(),
    }
}

fn sz_err(e: sevenz_rust::Error) -> ArchiveError {
    ArchiveError::Backend(e.to_string())
}
