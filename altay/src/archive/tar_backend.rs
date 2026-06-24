// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! tar-family backend: plain tar plus gzip/bzip2/xz/zstd-wrapped tarballs.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::filesystem::Progress;

use super::{safe_join, ArchiveError, Backend, Codec, Member, Options};

pub struct TarBackend {
    pub codec: Option<Codec>,
}

impl Backend for TarBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let reader = decompressing_reader(archive, self.codec)?;
        let mut tar = tar::Archive::new(reader);
        let mut out = Vec::new();
        for entry in tar.entries().map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let header = entry.header();
            let is_dir = header.entry_type().is_dir();
            let path = entry.path().map_err(io_err)?.into_owned();
            out.push(Member {
                path,
                is_dir,
                size: header.size().unwrap_or(0),
                compressed_size: header.size().unwrap_or(0),
                encrypted: false,
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
        let reader = decompressing_reader(archive, self.codec)?;
        let mut tar = tar::Archive::new(reader);
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        for entry in tar.entries().map_err(io_err)? {
            let mut entry = entry.map_err(io_err)?;
            let member = entry.path().map_err(io_err)?.into_owned();
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
    }

    fn create(
        &self,
        archive: &Path,
        sources: &[PathBuf],
        _opts: &Options,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<(), ArchiveError> {
        let sink = compressing_sink(archive, self.codec)?;
        let mut builder = tar::Builder::new(sink);
        let mut state = Progress { bytes_done: 0, bytes_total: 0, files_done: 0, files_total: 0 };
        for src in sources {
            let base = src.parent().unwrap_or_else(|| Path::new(""));
            let name = src.strip_prefix(base).unwrap_or(src);
            if src.is_dir() {
                builder.append_dir_all(name, src).map_err(io_err)?;
            } else {
                builder.append_path_with_name(src, name).map_err(io_err)?;
            }
            state.files_done += 1;
            on_progress(state);
        }
        let sink = builder.into_inner().map_err(io_err)?;
        sink.finish().map_err(io_err)?;
        Ok(())
    }
}

/// A reader that transparently decompresses according to `codec`.
fn decompressing_reader(path: &Path, codec: Option<Codec>) -> Result<Box<dyn Read>, ArchiveError> {
    let file = std::fs::File::open(path)?;
    Ok(match codec {
        None => Box::new(file),
        Some(Codec::Gzip) => Box::new(flate2::read::GzDecoder::new(file)),
        Some(Codec::Bzip2) => Box::new(bzip2::read::BzDecoder::new(file)),
        Some(Codec::Xz) | Some(Codec::Lzma) => Box::new(xz2::read::XzDecoder::new(file)),
        Some(Codec::Zstd) => Box::new(zstd::stream::read::Decoder::new(file)?),
    })
}

/// A writer that compresses according to `codec`, with explicit finalisation.
enum Sink {
    Plain(std::fs::File),
    Gz(flate2::write::GzEncoder<std::fs::File>),
    Bz(bzip2::write::BzEncoder<std::fs::File>),
    Xz(xz2::write::XzEncoder<std::fs::File>),
    Zstd(zstd::stream::write::Encoder<'static, std::fs::File>),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(buf),
            Sink::Gz(w) => w.write(buf),
            Sink::Bz(w) => w.write(buf),
            Sink::Xz(w) => w.write(buf),
            Sink::Zstd(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Gz(w) => w.flush(),
            Sink::Bz(w) => w.flush(),
            Sink::Xz(w) => w.flush(),
            Sink::Zstd(w) => w.flush(),
        }
    }
}

impl Sink {
    fn finish(self) -> io::Result<()> {
        match self {
            Sink::Plain(_) => Ok(()),
            Sink::Gz(w) => w.finish().map(|_| ()),
            Sink::Bz(w) => w.finish().map(|_| ()),
            Sink::Xz(w) => w.finish().map(|_| ()),
            Sink::Zstd(w) => w.finish().map(|_| ()),
        }
    }
}

fn compressing_sink(path: &Path, codec: Option<Codec>) -> Result<Sink, ArchiveError> {
    let file = std::fs::File::create(path)?;
    Ok(match codec {
        None => Sink::Plain(file),
        Some(Codec::Gzip) => Sink::Gz(flate2::write::GzEncoder::new(file, flate2::Compression::default())),
        Some(Codec::Bzip2) => Sink::Bz(bzip2::write::BzEncoder::new(file, bzip2::Compression::default())),
        Some(Codec::Xz) | Some(Codec::Lzma) => Sink::Xz(xz2::write::XzEncoder::new(file, 6)),
        Some(Codec::Zstd) => Sink::Zstd(zstd::stream::write::Encoder::new(file, 3)?),
    })
}

fn io_err(e: io::Error) -> ArchiveError {
    ArchiveError::Io(e)
}
