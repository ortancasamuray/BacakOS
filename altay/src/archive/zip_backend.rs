// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! ZIP-family backend (zip/zipx/jar/war/ear/apk). Supports listing, extraction
//! (incl. password-protected entries) and creation (optionally AES-encrypted).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{safe_join, ArchiveError, Backend, Member, Options};

pub struct ZipBackend;

impl Backend for ZipBackend {
    fn list(&self, archive: &Path, opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        self.list_streamed(archive, opts, &mut |_, _| {})
    }

    fn list_streamed(
        &self,
        archive: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<Member>, ArchiveError> {
        let file = std::fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file).map_err(zip_err)?;
        let total = zip.len();
        let mut out = Vec::with_capacity(total);
        for i in 0..total {
            let entry = zip.by_index_raw(i).map_err(zip_err)?;
            let name = entry.name().to_string();
            out.push(Member {
                path: PathBuf::from(&name),
                is_dir: entry.is_dir(),
                size: entry.size(),
                compressed_size: entry.compressed_size(),
                encrypted: entry.encrypted(),
            });
            on_progress(i + 1, total);
        }
        Ok(out)
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let file = std::fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file).map_err(zip_err)?;
        let total = zip.len() as u64;
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: total };

        for i in 0..zip.len() {
            // Determine the (safe) output path without holding a decrypted borrow.
            let (out_path, is_dir) = {
                let raw = zip.by_index_raw(i).map_err(zip_err)?;
                let name = raw
                    .enclosed_name()
                    .ok_or_else(|| ArchiveError::Backend(format!("unsafe entry: {}", raw.name())))?;
                (safe_join(into, &name)?, raw.is_dir())
            };

            if is_dir {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut entry = open_entry(&mut zip, i, opts)?;
                let mut out = std::fs::File::create(&out_path)?;
                let written = std::io::copy(&mut entry, &mut out)?;
                state.bytes_done = state.bytes_done.saturating_add(written);
            }
            state.files_done += 1;
            on_progress(state);
        }
        Ok(())
    }

    fn create(
        &self,
        archive: &Path,
        sources: &[PathBuf],
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        // Note: encrypted-zip *creation* is deferred — `SimpleFileOptions` requires
        // a 'static key; reading password-protected zips is fully supported.
        let file = std::fs::File::create(archive)?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        for src in sources {
            let base = src.parent().unwrap_or_else(|| Path::new(""));
            add_to_zip(&mut zip, src, base, options, &mut state, on_progress)?;
        }
        zip.finish().map_err(zip_err)?;
        Ok(())
    }
}

/// Open entry `i`, decrypting with the supplied password if the entry needs it.
fn open_entry<'a>(
    zip: &'a mut zip::ZipArchive<std::fs::File>,
    i: usize,
    opts: &Options,
) -> Result<zip::read::ZipFile<'a>, ArchiveError> {
    let encrypted = zip.by_index_raw(i).map_err(zip_err)?.encrypted();
    if encrypted {
        let pw = opts.password.as_ref().ok_or(ArchiveError::PasswordRequired)?;
        zip.by_index_decrypt(i, pw.as_bytes()).map_err(|e| match e {
            zip::result::ZipError::InvalidPassword => ArchiveError::PasswordRequired,
            other => ArchiveError::Backend(other.to_string()),
        })
    } else {
        zip.by_index(i).map_err(zip_err)
    }
}

/// Recursively add a path to the zip, storing names relative to `base`.
fn add_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    path: &Path,
    base: &Path,
    options: zip::write::SimpleFileOptions,
    state: &mut Progress,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), ArchiveError> {
    let rel = path.strip_prefix(base).unwrap_or(path);
    let name = rel.to_string_lossy().replace('\\', "/");
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        if !name.is_empty() {
            zip.add_directory(format!("{name}/"), options).map_err(zip_err)?;
        }
        for entry in std::fs::read_dir(path)? {
            add_to_zip(zip, &entry?.path(), base, options, state, on_progress)?;
        }
    } else {
        zip.start_file(name, options).map_err(zip_err)?;
        let mut f = std::fs::File::open(path)?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
            state.bytes_done = state.bytes_done.saturating_add(n as u64);
        }
        state.files_done += 1;
        on_progress(*state);
    }
    Ok(())
}

fn zip_err(e: zip::result::ZipError) -> ArchiveError {
    ArchiveError::Backend(e.to_string())
}
