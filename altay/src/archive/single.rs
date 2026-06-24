// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Single-stream compressors (`file.txt.gz`, `.bz2`, `.xz`, `.zst`, `.lzma`).
//! These wrap exactly one file, so the "archive" has a single member whose name
//! is the original file name with the compression suffix removed.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{ArchiveError, Backend, Codec, Member, Options};

pub struct SingleBackend {
    pub codec: Codec,
}

impl Backend for SingleBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let meta = std::fs::metadata(archive)?;
        Ok(vec![Member {
            path: PathBuf::from(inner_name(archive)),
            is_dir: false,
            size: 0, // uncompressed size is unknown without decompressing
            compressed_size: meta.len(),
            encrypted: false,
        }])
    }

    fn extract(
        &self,
        archive: &Path,
        into: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let mut reader = reader(archive, self.codec)?;
        let out_path = into.join(inner_name(archive));
        let mut out = std::fs::File::create(&out_path)?;
        let n = io::copy(&mut reader, &mut out)?;
        on_progress(Progress { bytes_done: n, bytes_total: n, files_done: 1, files_total: 1 });
        Ok(())
    }

    fn create(
        &self,
        archive: &Path,
        sources: &[PathBuf],
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        if sources.len() != 1 || sources[0].is_dir() {
            return Err(ArchiveError::Backend(
                "single-file compressors take exactly one regular file".into(),
            ));
        }
        let mut input = std::fs::File::open(&sources[0])?;
        let out = std::fs::File::create(archive)?;
        let n = match self.codec {
            Codec::Gzip => {
                let mut w = flate2::write::GzEncoder::new(out, flate2::Compression::default());
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?;
                n
            }
            Codec::Bzip2 => {
                let mut w = bzip2::write::BzEncoder::new(out, bzip2::Compression::default());
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?;
                n
            }
            Codec::Xz | Codec::Lzma => {
                let mut w = xz2::write::XzEncoder::new(out, 6);
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?;
                n
            }
            Codec::Zstd => {
                let mut w = zstd::stream::write::Encoder::new(out, 3)?;
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?;
                n
            }
        };
        on_progress(Progress { bytes_done: n, bytes_total: n, files_done: 1, files_total: 1 });
        Ok(())
    }
}

fn reader(path: &Path, codec: Codec) -> Result<Box<dyn Read>, ArchiveError> {
    let file = std::fs::File::open(path)?;
    Ok(match codec {
        Codec::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
        Codec::Bzip2 => Box::new(bzip2::read::BzDecoder::new(file)),
        Codec::Xz | Codec::Lzma => Box::new(xz2::read::XzDecoder::new(file)),
        Codec::Zstd => Box::new(zstd::stream::read::Decoder::new(file)?),
    })
}

/// Strip the compression suffix to recover the original file name.
fn inner_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for suffix in [".gz", ".bz2", ".xz", ".zst", ".lzma"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    format!("{name}.out")
}
