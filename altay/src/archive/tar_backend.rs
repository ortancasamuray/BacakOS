// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Anadolu Panteri <bilgi@anadolupanteri.org.tr>

//! tar-family backend: plain tar plus gzip/bzip2/xz/zstd-wrapped tarballs.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::filesystem::Progress;

use super::{safe_join, ArchiveError, Backend, Codec, Member, Options};

pub struct TarBackend {
    pub codec: Option<Codec>,
}

impl Backend for TarBackend {
    fn list(&self, archive: &Path, opts: &Options) -> Result<Vec<Member>, ArchiveError> {
        self.list_streamed(archive, opts, &mut |_, _| {})
    }

    fn list_streamed(
        &self,
        archive: &Path,
        _opts: &Options,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<Member>, ArchiveError> {
        let file_size = std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
        let reader = decompressing_reader(archive, self.codec)?;
        let mut tar = tar::Archive::new(reader);
        let mut out = Vec::new();
        let mut bytes_seen: u64 = 0;
        for entry in tar.entries().map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let header = entry.header();
            let is_dir = header.entry_type().is_dir();
            let path = entry.path().map_err(io_err)?.into_owned();
            bytes_seen = bytes_seen.saturating_add(512 + header.size().unwrap_or(0));
            out.push(Member {
                path,
                is_dir,
                size: header.size().unwrap_or(0),
                compressed_size: header.size().unwrap_or(0),
                encrypted: false,
            });
            // Encode progress as (bytes_seen * total / file_size, total) so the
            // caller can compute fraction = done as f32 / total as f32.
            // Use total = file_size so fraction ≈ bytes_read / file_size.
            let done = if file_size > 0 { bytes_seen.min(file_size) as usize } else { out.len() };
            let total = if file_size > 0 { file_size as usize } else { 0 };
            on_progress(done, total);
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

/// Wraps a child process + its stdout so the process is waited on drop.
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

fn proc_reader(cmd: &str, args: &[&str], path: &Path) -> Result<Box<dyn Read>, ArchiveError> {
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

/// A reader that transparently decompresses according to `codec`.
fn decompressing_reader(path: &Path, codec: Option<Codec>) -> Result<Box<dyn Read>, ArchiveError> {
    let file = std::fs::File::open(path)?;
    Ok(match codec {
        None => Box::new(file),
        Some(Codec::Gzip)   => Box::new(flate2::read::GzDecoder::new(file)),
        Some(Codec::Bzip2)  => Box::new(bzip2::read::BzDecoder::new(file)),
        Some(Codec::Xz) | Some(Codec::Lzma) => Box::new(xz2::read::XzDecoder::new(file)),
        Some(Codec::Zstd)   => Box::new(zstd::stream::read::Decoder::new(file)?),
        Some(Codec::Lzip)   => proc_reader("lzip",  &["-d", "-c", "--"], path)?,
        Some(Codec::Lzop)   => proc_reader("lzop",  &["-d", "-c", "--"], path)?,
        Some(Codec::Brotli) => Box::new(brotli::Decompressor::new(file, 65536)),
        Some(Codec::Compress) => proc_reader("uncompress", &["-c", "--"], path)?,
    })
}

/// Wraps a child process + its stdin so stdin is closed (→ EOF) on finish.
struct ProcWriter {
    stdin: std::process::ChildStdin,
    child: std::process::Child,
}
impl Write for ProcWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> { self.stdin.write(buf) }
    fn flush(&mut self) -> io::Result<()> { self.stdin.flush() }
}
impl ProcWriter {
    fn finish(self) -> io::Result<()> {
        drop(self.stdin); // EOF → compressor flushes and exits
        let mut c = self.child;
        c.wait().map(|_| ())
    }
}

/// A writer that compresses according to `codec`, with explicit finalisation.
enum Sink {
    Plain(std::fs::File),
    Gz(flate2::write::GzEncoder<std::fs::File>),
    Bz(bzip2::write::BzEncoder<std::fs::File>),
    Xz(xz2::write::XzEncoder<std::fs::File>),
    Zstd(zstd::stream::write::Encoder<'static, std::fs::File>),
    Brotli(brotli::CompressorWriter<std::fs::File>),
    Proc(ProcWriter),
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Sink::Plain(w)  => w.write(buf),
            Sink::Gz(w)     => w.write(buf),
            Sink::Bz(w)     => w.write(buf),
            Sink::Xz(w)     => w.write(buf),
            Sink::Zstd(w)   => w.write(buf),
            Sink::Brotli(w) => w.write(buf),
            Sink::Proc(w)   => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Sink::Plain(w)  => w.flush(),
            Sink::Gz(w)     => w.flush(),
            Sink::Bz(w)     => w.flush(),
            Sink::Xz(w)     => w.flush(),
            Sink::Zstd(w)   => w.flush(),
            Sink::Brotli(w) => w.flush(),
            Sink::Proc(w)   => w.flush(),
        }
    }
}

impl Sink {
    fn finish(self) -> io::Result<()> {
        match self {
            Sink::Plain(_)  => Ok(()),
            Sink::Gz(w)     => w.finish().map(|_| ()),
            Sink::Bz(w)     => w.finish().map(|_| ()),
            Sink::Xz(w)     => w.finish().map(|_| ()),
            Sink::Zstd(w)   => w.finish().map(|_| ()),
            Sink::Brotli(w) => { drop(w); Ok(()) }
            Sink::Proc(w)   => w.finish(),
        }
    }
}

fn proc_sink(cmd: &str, args: &[&str], path: &Path) -> Result<Sink, ArchiveError> {
    let out_file = std::fs::File::create(path)?;
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(out_file)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ArchiveError::Backend(format!("{cmd} başlatılamadı: {e}")))?;
    let stdin = child.stdin.take().unwrap();
    Ok(Sink::Proc(ProcWriter { stdin, child }))
}

// stdin-piped child: write to stdin, compressor writes to `path` as stdout
fn proc_sink_from_child(mut child: std::process::Child) -> Sink {
    let stdin = child.stdin.take().unwrap();
    Sink::Proc(ProcWriter { stdin, child })
}

fn compressing_sink(path: &Path, codec: Option<Codec>) -> Result<Sink, ArchiveError> {
    let file = std::fs::File::create(path)?;
    Ok(match codec {
        None =>
            Sink::Plain(file),
        Some(Codec::Gzip) =>
            Sink::Gz(flate2::write::GzEncoder::new(file, flate2::Compression::default())),
        Some(Codec::Bzip2) =>
            Sink::Bz(bzip2::write::BzEncoder::new(file, bzip2::Compression::default())),
        Some(Codec::Xz) | Some(Codec::Lzma) =>
            Sink::Xz(xz2::write::XzEncoder::new(file, 6)),
        Some(Codec::Zstd) =>
            Sink::Zstd(zstd::stream::write::Encoder::new(file, 3)?),
        Some(Codec::Brotli) =>
            Sink::Brotli(brotli::CompressorWriter::new(file, 65536, 6, 22)),
        Some(Codec::Lzip) => {
            drop(file);
            proc_sink("lzip", &["-c", "-"], path)?
        }
        Some(Codec::Lzop) => {
            drop(file);
            proc_sink("lzop", &["-c", "-"], path)?
        }
        Some(Codec::Compress) => {
            drop(file);
            // `compress` reads stdin and writes to stdout
            let out = std::fs::File::create(path)?;
            let child = Command::new("compress")
                .arg("-c")
                .stdin(Stdio::piped())
                .stdout(out)
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| ArchiveError::Backend(format!("compress başlatılamadı: {e}")))?;
            proc_sink_from_child(child)
        }
    })
}

fn io_err(e: io::Error) -> ArchiveError {
    ArchiveError::Io(e)
}
