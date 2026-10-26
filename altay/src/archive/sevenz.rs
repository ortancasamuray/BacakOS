// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! 7-Zip backend (read/extract + create) via the pure-Rust `sevenz-rust` crate.
//! Password-protected archives are supported for extraction.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{copy_member, create_member_dir, create_member_file, create_member_symlink, safe_join, ArchiveError, Backend, Member, Options};

pub struct SevenZBackend;

impl Backend for SevenZBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let meta = sevenz_rust::Archive::open(archive).map_err(sz_err)?;
        let members = meta
            .files
            .iter()
            .map(|f| Member {
                path: PathBuf::from(f.name()),
                is_dir: f.is_directory(),
                size: f.size(),
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
        // Members go through the same checks as zip/tar (see `safe_join` and
        // `create_member_*`), not sevenz-rust's default writer, which joins
        // names onto `into` as they are and follows links.
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        let mut failure = None;
        let result = {
            let mut write = |entry: &sevenz_rust::SevenZArchiveEntry, reader: &mut dyn Read, _: &PathBuf| {
                match write_entry(archive, into, entry, reader) {
                    Ok(n) => {
                        state.bytes_done = state.bytes_done.saturating_add(n);
                        state.files_done += 1;
                        on_progress(state);
                        Ok(true)
                    }
                    Err(e) => {
                        failure = Some(e);
                        Err(sevenz_rust::Error::other("extraction stopped"))
                    }
                }
            };
            let file = std::fs::File::open(archive).map_err(ArchiveError::at(archive))?;
            if opts.password.is_some() {
                sevenz_rust::decompress_with_extract_fn_and_password(file, into, password(opts), &mut write)
            } else {
                sevenz_rust::decompress_with_extract_fn(file, into, &mut write)
            }
        };
        result.map_err(|e| failure.unwrap_or_else(|| sz_err(e)))
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

/// Unix file-type bits, stored by p7zip/7-Zip in the high half of the
/// attributes when bit 15 is set.
const UNIX_EXTENSION: u32 = 0x8000;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

/// Write one member under `into`; returns the bytes written.
fn write_entry(archive: &Path, into: &Path, entry: &sevenz_rust::SevenZArchiveEntry, reader: &mut dyn Read) -> Result<u64, ArchiveError> {
    if entry.is_anti_item() {
        return Ok(0); // a deletion marker from an update, nothing to write
    }
    let out_path = safe_join(into, Path::new(entry.name()))?;
    let attrs = entry.windows_attributes();
    if entry.is_directory() {
        create_member_dir(into, &out_path)?;
        Ok(0)
    } else if attrs & UNIX_EXTENSION != 0 && (attrs >> 16) & S_IFMT == S_IFLNK {
        // A link's content is its target.
        let mut target = Vec::new();
        reader.read_to_end(&mut target).map_err(ArchiveError::at(archive))?;
        create_member_symlink(into, &out_path, Path::new(std::ffi::OsStr::from_bytes(&target)))?;
        Ok(0)
    } else {
        let mut out = create_member_file(into, &out_path)?;
        let n = copy_member(reader, &mut out, archive, &out_path)?;
        if entry.has_last_modified_date {
            let _ = out.set_modified(entry.last_modified_date().into());
        }
        Ok(n)
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
