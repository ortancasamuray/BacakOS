// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! Single-stream compressors (`file.txt.gz`, `.bz2`, `.xz`, `.zst`, `.lzma`).
//! These wrap exactly one file, so the "archive" has a single member whose name
//! is the original file name with the compression suffix removed.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::filesystem::Progress;

use super::{ArchiveError, Backend, Codec, Member, Options};

pub struct SingleBackend {
    pub codec: Codec,
}

/// Backend for formats needing an external process (rzip, etc.).
pub struct SinglePipedBackend {
    pub decompress_cmd: &'static str,
    pub decompress_args: &'static [&'static str],
    pub compress_cmd: &'static str,
    pub compress_args: &'static [&'static str],
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
                "tek dosya sıkıştırıcılar yalnızca bir dosya alır".into(),
            ));
        }
        let mut input = std::fs::File::open(&sources[0])?;
        let out = std::fs::File::create(archive)?;
        let n = match self.codec {
            Codec::Gzip => {
                let mut w = flate2::write::GzEncoder::new(out, flate2::Compression::default());
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?; n
            }
            Codec::Bzip2 => {
                let mut w = bzip2::write::BzEncoder::new(out, bzip2::Compression::default());
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?; n
            }
            Codec::Xz | Codec::Lzma => {
                let mut w = xz2::write::XzEncoder::new(out, 6);
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?; n
            }
            Codec::Zstd => {
                let mut w = zstd::stream::write::Encoder::new(out, 3)?;
                let n = io::copy(&mut input, &mut w)?;
                w.finish()?; n
            }
            Codec::Brotli => {
                let mut w = brotli::CompressorWriter::new(out, 65536, 6, 22);
                let n = io::copy(&mut input, &mut w)?;
                drop(w); n
            }
            Codec::Lzip => {
                drop(out);
                pipe_compress("lzip", &["-c", "-"], &mut input, archive)?
            }
            Codec::Lzop => {
                drop(out);
                pipe_compress("lzop", &["-c", "-"], &mut input, archive)?
            }
            Codec::Compress => {
                drop(out);
                pipe_compress("compress", &["-c"], &mut input, archive)?
            }
        };
        on_progress(Progress { bytes_done: n, bytes_total: n, files_done: 1, files_total: 1 });
        Ok(())
    }
}

fn reader(path: &Path, codec: Codec) -> Result<Box<dyn Read>, ArchiveError> {
    let file = std::fs::File::open(path)?;
    Ok(match codec {
        Codec::Gzip     => Box::new(flate2::read::GzDecoder::new(file)),
        Codec::Bzip2    => Box::new(bzip2::read::BzDecoder::new(file)),
        Codec::Xz | Codec::Lzma => Box::new(xz2::read::XzDecoder::new(file)),
        Codec::Zstd     => Box::new(zstd::stream::read::Decoder::new(file)?),
        Codec::Brotli   => Box::new(brotli::Decompressor::new(file, 65536)),
        Codec::Lzip     => proc_decompress("lzip",     &["-d", "-c", "--"], path)?,
        Codec::Lzop     => proc_decompress("lzop",     &["-d", "-c", "--"], path)?,
        Codec::Compress => proc_decompress("uncompress", &["-c", "--"],     path)?,
    })
}

fn pipe_compress(cmd: &str, args: &[&str], input: &mut dyn Read, out_path: &Path) -> Result<u64, ArchiveError> {
    let out_file = std::fs::File::create(out_path)?;
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(out_file)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ArchiveError::Backend(format!("{cmd} başlatılamadı: {e}")))?;
    let n = io::copy(input, child.stdin.as_mut().unwrap())
        .map_err(|e| ArchiveError::Io(e))?;
    drop(child.stdin.take());
    child.wait()?;
    Ok(n)
}

fn proc_decompress(cmd: &str, args: &[&str], path: &Path) -> Result<Box<dyn Read>, ArchiveError> {
    let mut child = Command::new(cmd)
        .args(args)
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ArchiveError::Backend(format!("{cmd} başlatılamadı: {e}")))?;
    let stdout = child.stdout.take().unwrap();
    Ok(Box::new(ProcReader { stdout, child }))
}

struct ProcReader {
    stdout: std::process::ChildStdout,
    child: std::process::Child,
}
impl Read for ProcReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> { self.stdout.read(buf) }
}
impl Drop for ProcReader {
    fn drop(&mut self) { let _ = self.child.wait(); }
}

/// Strip the compression suffix to recover the original file name.
fn inner_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for suffix in [".gz", ".bz2", ".bz", ".xz", ".zst", ".lzma", ".lz", ".lzo", ".br", ".rz", ".Z", ".z"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    format!("{name}.out")
}

// ---- SinglePipedBackend (rzip and similar) ----------------------------------

impl Backend for SinglePipedBackend {
    fn list(&self, archive: &Path, _opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        let meta = std::fs::metadata(archive)?;
        Ok(vec![Member {
            path: PathBuf::from(inner_name(archive)),
            is_dir: false,
            size: 0,
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
        let out_path = into.join(inner_name(archive));
        let out_file = std::fs::File::create(&out_path)?;
        let status = Command::new(self.decompress_cmd)
            .args(self.decompress_args)
            .arg(archive)
            .stdout(out_file)
            .stderr(Stdio::null())
            .status()
            .map_err(|e| ArchiveError::Backend(format!("{} başlatılamadı: {e}", self.decompress_cmd)))?;
        if !status.success() {
            return Err(ArchiveError::Backend(format!("{} başarısız", self.decompress_cmd)));
        }
        let n = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
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
                "tek dosya sıkıştırıcılar yalnızca bir dosya alır".into(),
            ));
        }
        let mut args: Vec<&std::ffi::OsStr> = self.compress_args.iter().map(|s| s.as_ref()).collect();
        let arc_str = archive.as_os_str();
        args.push(arc_str);
        args.push(sources[0].as_os_str());
        let status = Command::new(self.compress_cmd)
            .args(&args)
            .stderr(Stdio::null())
            .status()
            .map_err(|e| ArchiveError::Backend(format!("{} başlatılamadı: {e}", self.compress_cmd)))?;
        if !status.success() {
            return Err(ArchiveError::Backend(format!("{} başarısız", self.compress_cmd)));
        }
        let n = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
        on_progress(Progress { bytes_done: n, bytes_total: n, files_done: 1, files_total: 1 });
        Ok(())
    }
}
