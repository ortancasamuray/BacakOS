// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Debian package backend (read/extract). A `.deb` is an `ar` archive holding
//! `debian-binary`, `control.tar.*` and `data.tar.*`; the installed files live
//! in `data.tar.*`, which is what we list and extract.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{safe_join, ArchiveError, Backend, Codec, Member, Options};

pub struct DebBackend;

impl Backend for DebBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        with_data_tar(archive, |reader| {
            let mut tar = tar::Archive::new(reader);
            let mut out = Vec::new();
            for entry in tar.entries().map_err(ArchiveError::Io)? {
                let entry = entry.map_err(ArchiveError::Io)?;
                let header = entry.header();
                let path = entry.path().map_err(ArchiveError::Io)?.into_owned();
                out.push(Member {
                    path,
                    is_dir: header.entry_type().is_dir(),
                    size: header.size().unwrap_or(0),
                    compressed_size: header.size().unwrap_or(0),
                    encrypted: false,
                });
            }
            Ok(out)
        })
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        with_data_tar(archive, |reader| {
            let mut tar = tar::Archive::new(reader);
            let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
            for entry in tar.entries().map_err(ArchiveError::Io)? {
                let mut entry = entry.map_err(ArchiveError::Io)?;
                let member = entry.path().map_err(ArchiveError::Io)?.into_owned();
                let out_path = safe_join(into, &member)?;
                if entry.header().entry_type().is_dir() {
                    std::fs::create_dir_all(&out_path)?;
                } else {
                    if let Some(parent) = out_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let mut out = std::fs::File::create(&out_path)?;
                    let n = io::copy(&mut entry, &mut out)?;
                    state.bytes_done = state.bytes_done.saturating_add(n);
                }
                state.files_done += 1;
                on_progress(state);
            }
            Ok(())
        })
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

/// Open the `.deb`, locate the `data.tar.*` member, and hand a decompressing
/// reader for it to `f`.
fn with_data_tar<T>(
    archive: &Path,
    f: impl FnOnce(Box<dyn Read + '_>) -> Result<T, ArchiveError>,
) -> Result<T, ArchiveError> {
    let file = std::fs::File::open(archive)?;
    let mut ar = ar::Archive::new(file);
    while let Some(entry) = ar.next_entry() {
        let entry = entry.map_err(ArchiveError::Io)?;
        let name = String::from_utf8_lossy(entry.header().identifier()).into_owned();
        let trimmed = name.trim_end_matches('/');
        if let Some(codec) = data_tar_codec(trimmed) {
            let reader: Box<dyn Read> = match codec {
                None => Box::new(entry),
                Some(Codec::Gzip) => Box::new(flate2::read::GzDecoder::new(entry)),
                Some(Codec::Xz) | Some(Codec::Lzma) => Box::new(xz2::read::XzDecoder::new(entry)),
                Some(Codec::Bzip2) => Box::new(bzip2::read::BzDecoder::new(entry)),
                Some(Codec::Zstd) => Box::new(zstd::stream::read::Decoder::new(entry)?),
            };
            return f(reader);
        }
    }
    Err(ArchiveError::Backend("no data.tar member found in .deb".into()))
}

/// Recognise `data.tar[.gz|.xz|.zst|.bz2|.lzma]` and report its codec.
fn data_tar_codec(name: &str) -> Option<Option<Codec>> {
    let rest = name.strip_prefix("data.tar")?;
    Some(match rest {
        "" => None,
        ".gz" => Some(Codec::Gzip),
        ".xz" => Some(Codec::Xz),
        ".zst" => Some(Codec::Zstd),
        ".bz2" => Some(Codec::Bzip2),
        ".lzma" => Some(Codec::Lzma),
        _ => return None,
    })
}
