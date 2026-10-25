// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! ZIP-family backend (zip/zipx/jar/war/ear/apk). Supports listing, extraction
//! (incl. password-protected entries) and creation (optionally AES-encrypted).

use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{copy_member, create_member_dir, create_member_file, create_member_symlink, safe_join, ArchiveError, Backend, Member, Options};

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
        let file = std::fs::File::open(archive).map_err(ArchiveError::at(archive))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| zip_err_at(e, archive))?;
        let total = zip.len() as u64;
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: total };

        for i in 0..zip.len() {
            // Determine the (safe) output path without holding a decrypted borrow.
            let (out_path, is_dir, is_symlink) = {
                let raw = zip.by_index_raw(i).map_err(|e| zip_err_at(e, archive))?;
                let name = raw.enclosed_name().ok_or_else(|| ArchiveError::Unsafe(PathBuf::from(raw.name())))?;
                (safe_join(into, &name)?, raw.is_dir(), raw.is_symlink())
            };

            if is_dir {
                create_member_dir(into, &out_path)?;
            } else if is_symlink {
                // The link target is stored as the entry's content.
                let mut target = Vec::new();
                open_entry(&mut zip, i, opts)?.read_to_end(&mut target).map_err(ArchiveError::at(archive))?;
                let target = PathBuf::from(std::ffi::OsString::from_vec(target));
                create_member_symlink(into, &out_path, &target)?;
            } else {
                let mut entry = open_entry(&mut zip, i, opts)?;
                let mut out = create_member_file(into, &out_path)?;
                let written = copy_member(&mut entry, &mut out, archive, &out_path)?;
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
        opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        // Note: encrypted-zip *creation* is deferred — `SimpleFileOptions` requires
        // a 'static key; reading password-protected zips is fully supported.
        let file = std::fs::File::create(archive).map_err(ArchiveError::at(archive))?;
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        let cancel = opts.cancel.as_deref();
        let exclude = &opts.exclude;

        // Pre-scan the sources so progress has a real denominator. Without a
        // total, the UI percentage is meaningless (stuck at 0%). The scan is
        // metadata-only, so it's cheap next to the compression itself.
        let (files_total, bytes_total) = sources
            .iter()
            .map(|s| scan_totals(s, exclude))
            .fold((0u64, 0u64), |(f, b), (sf, sb)| (f + sf, b + sb));
        let mut state = Progress { bytes_done: 0, bytes_total, files_done: 0, files_total };
        for src in sources {
            let base = src.parent().unwrap_or_else(|| Path::new(""));
            add_to_zip(&mut zip, archive, src, base, options, exclude, &mut state, on_progress, cancel)?;
        }
        zip.finish().map_err(|e| zip_err_at(e, archive))?;
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
#[allow(clippy::too_many_arguments)]
fn add_to_zip(
    zip: &mut zip::ZipWriter<std::fs::File>,
    archive: &Path,
    path: &Path,
    base: &Path,
    options: zip::write::SimpleFileOptions,
    exclude: &super::Exclude,
    state: &mut Progress,
    on_progress: &mut dyn FnMut(Progress),
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), ArchiveError> {
    if is_cancelled(cancel) {
        return Err(ArchiveError::Cancelled);
    }
    let rel = path.strip_prefix(base).unwrap_or(path);
    let name = rel.to_string_lossy().replace('\\', "/");
    let meta = std::fs::symlink_metadata(path).map_err(ArchiveError::at(path))?;
    if meta.is_dir() {
        if !name.is_empty() {
            zip.add_directory(format!("{name}/"), options).map_err(|e| zip_err_at(e, archive))?;
        }
        for entry in std::fs::read_dir(path).map_err(ArchiveError::at(path))? {
            let entry = entry.map_err(ArchiveError::at(path))?;
            if exclude.matches(&entry.file_name()) {
                continue;
            }
            add_to_zip(zip, archive, &entry.path(), base, options, exclude, state, on_progress, cancel)?;
        }
    } else if meta.file_type().is_symlink() {
        // Store the link itself, not what it points to: following it would
        // pull in files outside the selection, fail on links to folders and
        // loop on cyclic links. Extraction recreates it as a link.
        let target = std::fs::read_link(path).map_err(ArchiveError::at(path))?;
        zip.add_symlink(name, target.to_string_lossy(), options).map_err(|e| zip_err_at(e, archive))?;
        state.files_done += 1;
        on_progress(*state);
    } else if !meta.is_file() {
        // FIFOs, sockets and device nodes have no content to archive, and
        // opening a FIFO would block until a writer appears.
        log::info!("compress: skipping special file {}", path.display());
        state.files_done += 1;
        on_progress(*state);
    } else {
        // Without ZIP64 the zip writer refuses entries past 4 GiB ("Large
        // file option has not been set"). Margin for deflate overhead on
        // incompressible data, which can grow slightly past the input size.
        let options = options.large_file(meta.len() >= LARGE_FILE_THRESHOLD);
        zip.start_file(name, options).map_err(|e| zip_err_at(e, archive))?;
        let mut f = std::fs::File::open(path).map_err(ArchiveError::at(path))?;
        let mut buf = [0u8; 64 * 1024];
        // Emit intermediate progress every ~8 MiB so a single large file
        // still moves the bar (on_progress otherwise only fires per file).
        let mut since_emit = 0u64;
        loop {
            let n = f.read(&mut buf).map_err(ArchiveError::at(path))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n]).map_err(ArchiveError::at(archive))?;
            state.bytes_done = state.bytes_done.saturating_add(n as u64);
            since_emit += n as u64;
            if since_emit >= 8 * 1024 * 1024 {
                since_emit = 0;
                // Check for cancellation at the same cadence as progress.
                if is_cancelled(cancel) {
                    return Err(ArchiveError::Cancelled);
                }
                on_progress(*state);
            }
        }
        state.files_done += 1;
        on_progress(*state);
    }
    Ok(())
}

/// Whether a cooperative-cancel flag has been raised.
fn is_cancelled(cancel: Option<&std::sync::atomic::AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
}

/// Count regular files and sum their uncompressed sizes under `path`,
/// mirroring how [`add_to_zip`] traverses (directories recurse, files add
/// their size). Used to give the progress bar a real total. Unreadable
/// entries are skipped so a permission error never aborts the estimate.
fn scan_totals(path: &Path, exclude: &super::Exclude) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if meta.is_dir() {
        let mut files = 0u64;
        let mut bytes = 0u64;
        if let Ok(rd) = std::fs::read_dir(path) {
            for entry in rd.flatten().filter(|e| !exclude.matches(&e.file_name())) {
                let (f, b) = scan_totals(&entry.path(), exclude);
                files += f;
                bytes += b;
            }
        }
        (files, bytes)
    } else {
        (1, meta.len())
    }
}

/// Entries at least this large are written with ZIP64 extensions: 4 GiB less
/// a 1/256 margin, since deflate can make incompressible data slightly larger.
const LARGE_FILE_THRESHOLD: u64 = u32::MAX as u64 - (u32::MAX as u64 >> 8);

fn zip_err(e: zip::result::ZipError) -> ArchiveError {
    ArchiveError::Backend(e.to_string())
}

/// Like [`zip_err`] for writer calls, keeping I/O errors (disk full, …) as
/// I/O errors on the archive file instead of flattening them to text.
fn zip_err_at(e: zip::result::ZipError, archive: &Path) -> ArchiveError {
    match e {
        zip::result::ZipError::Io(source) => ArchiveError::File { path: archive.to_path_buf(), source },
        other => zip_err(other),
    }
}
